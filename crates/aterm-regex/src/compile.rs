// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Ast`] → NFA [`Program`], under a hard ceiling.
//!
//! A Thompson construction emitted right-to-left: each fragment is compiled knowing the
//! instruction it continues to, so concatenation is a fold from the end with no
//! back-patching. The loops (`*`, `+`, `{n,}`) are the exception: an [`Inst::Jump`]
//! placeholder is reserved first and overwritten with the real [`Inst::Split`] once the
//! body's entry is known.
//!
//! There is one instruction per code point a pattern can consume, so `x{n}` costs `n`
//! instructions and a thirteen-byte pattern can ask for a multi-megabyte automaton. The
//! `size_limit` budget is charged in bytes on every instruction pushed (see
//! [`INST_COST`]), so an oversized pattern fails early instead of allocating.

use crate::Error;
use crate::parse::{Assertion, Ast, ClassSet};
use crate::pikevm::Thread;

/// One NFA instruction. `next`/`a`/`b` are indices into [`Program::insts`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum Inst {
    /// Consume exactly this code point, then go to `next`.
    Char { c: char, next: usize },
    /// Consume one code point from `Program::classes[class]`, then go to `next`.
    Class { class: usize, next: usize },
    /// Zero width: follow `a` first, then `b`. Order *is* the match priority.
    Split { a: usize, b: usize },
    /// Zero width, unconditional. Only the placeholder a loop reserves before its body's
    /// entry is known, then overwritten with a [`Split`](Self::Split), so a finished
    /// program holds none; every walk handles it anyway.
    Jump { next: usize },
    /// Zero width, conditional on the surrounding text.
    Assert { kind: Assertion, next: usize },
    /// The pattern is satisfied.
    Match,
}

/// Bytes one instruction is charged against `size_limit`: the instruction plus, in each
/// of the Pike VM's two thread lists, a `Thread` slot and a generation stamp.
pub(crate) const INST_COST: usize =
    size_of::<Inst>() + 2 * (size_of::<Thread>() + size_of::<u32>());

/// A compiled pattern: instructions, the class table they index, and the
/// prefilter.
#[derive(Debug)]
pub(crate) struct Program {
    pub(crate) insts: Vec<Inst>,
    pub(crate) classes: Vec<ClassSet>,
    pub(crate) start: usize,
    /// The prefilter, when one can be armed. See [`Prefilter`].
    pub(crate) first: Option<Prefilter>,
    /// Every path out of `start` passes a `\A` before it can consume or accept,
    /// so the pattern can only match at offset 0. See [`start_anchored`].
    pub(crate) start_anchored: bool,
    /// `classes[i]`'s membership over U+0000..U+007F, one bit per code point, parallel to
    /// `classes`. Kept apart from `ClassSet` for locality (sixteen bytes per class in the
    /// hot loop), and because a `ClassSet` is mutated while parsing (ranges added, folded,
    /// negated): built once the classes are frozen, the bitmap cannot go stale.
    pub(crate) class_ascii: Vec<u128>,
}

/// Compile `ast` into a program of at most `size_limit` bytes.
///
/// # Errors
/// [`Error::CompiledTooBig`] once the program passes the ceiling.
pub(crate) fn compile(ast: &Ast, size_limit: usize) -> Result<Program, Error> {
    let mut c = Compiler {
        insts: Vec::new(),
        classes: Vec::new(),
        class_bytes: 0,
        limit: size_limit,
    };
    let accept = c.push(Inst::Match)?;
    let start = c.emit(ast, accept)?;
    let first = first_set(&c.insts, &c.classes, start);
    let anchored = start_anchored(&c.insts, start);
    let class_ascii = c.classes.iter().map(ascii_bitmap).collect();
    Ok(Program {
        insts: c.insts,
        classes: c.classes,
        start,
        first,
        start_anchored: anchored,
        class_ascii,
    })
}

/// Membership of `set` over U+0000..U+007F, one bit per code point.
///
/// The whole answer, not a hint: negation and the perl classes are folded in, so the
/// caller never consults the set as well.
fn ascii_bitmap(set: &ClassSet) -> u128 {
    let mut bits: u128 = 0;
    for code in 0u32..128 {
        let Some(c) = char::from_u32(code) else {
            continue;
        };
        if set.matches(c) {
            bits |= 1u128 << code;
        }
    }
    bits
}

struct Compiler {
    insts: Vec<Inst>,
    classes: Vec<ClassSet>,
    class_bytes: usize,
    limit: usize,
}

impl Compiler {
    fn push(&mut self, inst: Inst) -> Result<usize, Error> {
        self.insts.push(inst);
        if self.insts.len() * INST_COST + self.class_bytes > self.limit {
            return Err(Error::CompiledTooBig(self.limit));
        }
        Ok(self.insts.len() - 1)
    }

    fn add_class(&mut self, class: &ClassSet) -> Result<usize, Error> {
        self.class_bytes += class.byte_size();
        if self.insts.len() * INST_COST + self.class_bytes > self.limit {
            return Err(Error::CompiledTooBig(self.limit));
        }
        self.classes.push(class.clone());
        Ok(self.classes.len() - 1)
    }

    /// Emit `ast` so that control reaches `next` when it succeeds; return the
    /// instruction control enters it at.
    fn emit(&mut self, ast: &Ast, next: usize) -> Result<usize, Error> {
        match ast {
            Ast::Empty => Ok(next),
            Ast::Literal(c) => self.push(Inst::Char { c: *c, next }),
            Ast::Class(set) => {
                let class = self.add_class(set)?;
                self.push(Inst::Class { class, next })
            }
            Ast::Assert(kind) => self.push(Inst::Assert { kind: *kind, next }),
            Ast::Concat(items) => {
                let mut entry = next;
                for item in items.iter().rev() {
                    entry = self.emit(item, entry)?;
                }
                Ok(entry)
            }
            Ast::Alt(branches) => {
                let Some((last, rest)) = branches.split_last() else {
                    return Ok(next);
                };
                let mut entry = self.emit(last, next)?;
                for branch in rest.iter().rev() {
                    let a = self.emit(branch, next)?;
                    entry = self.push(Inst::Split { a, b: entry })?;
                }
                Ok(entry)
            }
            Ast::Repeat {
                node,
                min,
                max,
                greedy,
            } => self.emit_repeat(node, *min, *max, *greedy, next),
        }
    }

    /// Emit a quantified subexpression.
    ///
    /// `x*` takes one of two shapes, chosen by whether the body can match the empty
    /// string, as `regex` does:
    ///
    /// * Non-nullable body: one split is both entry and loop head. Every iteration
    ///   consumes, so the loop never returns to its head without progress.
    /// * Nullable body: an entry split plus the post-body split. With one shared split,
    ///   `(|a)*` returns to the head having consumed nothing, a state already entered at
    ///   this position, so that path dies and the `a` branch wrongly wins; the post-body
    ///   split's exit arm instead reaches `Match` first.
    ///
    /// The two-split shape for every star would be a different bug: the extra hop reorders
    /// the exit against the body's alternatives when a thread resumes mid-body
    /// (`(.*?){2,}\b` on `" ab"` must end at 1, not 3).
    ///
    /// The mandatory copies stop at a fixed point. `size_limit` is charged per push, so a
    /// `min` replaying a body that emits nothing (`(?:){n}`, `(?:a{0}){n}`) would loop
    /// `min` times without consulting it, and `(?:(?:){4294967295}){4294967295}` would
    /// never finish. [`Compiler::emit`] returns `next` unchanged exactly when it emitted
    /// nothing (every push returns a fresh, larger index), so the first no-op copy ends
    /// the loop; the emitted program is the same.
    fn emit_repeat(
        &mut self,
        node: &Ast,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        next: usize,
    ) -> Result<usize, Error> {
        match max {
            // `x{n,}`: one self-re-entering body behind the mandatory copies.
            None => {
                let post = self.push(Inst::Jump { next })?;
                let body = self.emit(node, post)?;
                self.insts[post] = split(body, next, greedy);
                if min > 0 {
                    let mut entry = body;
                    for _ in 0..min - 1 {
                        let copy = self.emit(node, entry)?;
                        // A body that emits nothing is a fixed point; see above.
                        if copy == entry {
                            break;
                        }
                        entry = copy;
                    }
                    return Ok(entry);
                }
                if matches_empty(node) {
                    return self.push(split(body, next, greedy));
                }
                // Non-nullable body: the loop head doubles as the entry, so
                // hand the same split back rather than emitting a second.
                Ok(post)
            }
            // `x{n,m}` — a chain of m-n nested optionals, then the n mandatory
            // copies. Building the chain right to left makes every optional's
            // bail-out land on the same `next`, which is what "skip one, skip
            // all the rest" requires.
            Some(max) => {
                let mut entry = next;
                for _ in 0..max.saturating_sub(min) {
                    let body = self.emit(node, entry)?;
                    entry = self.push(split(body, next, greedy))?;
                }
                for _ in 0..min {
                    let copy = self.emit(node, entry)?;
                    // Same fixed point as the `{n,}` arm above.
                    if copy == entry {
                        break;
                    }
                    entry = copy;
                }
                Ok(entry)
            }
        }
    }
}

/// Can `ast` match the empty string? Decides which of the two `x*` shapes
/// [`Compiler::emit_repeat`] uses, and nothing else.
fn matches_empty(ast: &Ast) -> bool {
    match ast {
        Ast::Empty | Ast::Assert(_) => true,
        Ast::Literal(_) | Ast::Class(_) => false,
        Ast::Concat(items) => items.iter().all(matches_empty),
        Ast::Alt(branches) => branches.iter().any(matches_empty),
        Ast::Repeat { node, min, .. } => *min == 0 || matches_empty(node),
    }
}

/// A two-way branch that prefers `body` when greedy and `exit` when not.
fn split(body: usize, exit: usize, greedy: bool) -> Inst {
    if greedy {
        Inst::Split { a: body, b: exit }
    } else {
        Inst::Split { a: exit, b: body }
    }
}

/// What a match can begin with: the search's fast path over uninteresting text.
///
/// `pcs` are the consuming instructions a match's first code point can come from;
/// `lead_bytes` marks the leading UTF-8 byte of every such code point, so a position is
/// rejected with one array lookup. Both are supersets: the search skips a position only
/// when nothing here could match there, so an over-broad prefilter costs time, never a
/// match.
#[derive(Debug)]
pub(crate) struct Prefilter {
    pub(crate) pcs: Box<[usize]>,
    pub(crate) lead_bytes: [bool; 256],
    /// The same marking for the second code point, when every path has one; `None` for a
    /// pattern that can stop after its first. It pays for a common lead byte: `(?i)ERROR`
    /// marks `E` and `e`, about a tenth of terminal output, and nearly all those positions
    /// fail on the second character, which one more lookup rejects before the simulation
    /// is entered.
    pub(crate) second_bytes: Option<[bool; 256]>,
}

/// Past this many candidate instructions a prefilter costs more than it saves.
const MAX_FIRST: usize = 16;

/// The consuming instructions reachable from `starts` without consuming
/// anything: the epsilon closure, stopped at the first thing that eats a code
/// point.
///
/// `None` when the closure reaches [`Inst::Match`] — the pattern can stop here,
/// so there is no code point to filter on — or when it grows past
/// [`MAX_FIRST`].
fn consuming_closure(insts: &[Inst], starts: &[usize]) -> Option<Vec<usize>> {
    let mut seen = vec![false; insts.len()];
    let mut stack: Vec<usize> = starts.to_vec();
    let mut out: Vec<usize> = Vec::new();
    while let Some(pc) = stack.pop() {
        let slot = seen.get_mut(pc)?;
        if *slot {
            continue;
        }
        *slot = true;
        match insts.get(pc)? {
            Inst::Char { .. } | Inst::Class { .. } => {
                if out.len() == MAX_FIRST {
                    return None;
                }
                out.push(pc);
            }
            Inst::Split { a, b } => {
                stack.push(*a);
                stack.push(*b);
            }
            Inst::Jump { next } | Inst::Assert { next, .. } => stack.push(*next),
            Inst::Match => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Mark the leading UTF-8 byte of every code point `pcs` can consume: a superset (a class
/// marks the non-ASCII side conservatively), which is what makes skipping on it safe.
fn mark_leads(insts: &[Inst], classes: &[ClassSet], pcs: &[usize]) -> Option<[bool; 256]> {
    let mut lead_bytes = [false; 256];
    for &pc in pcs {
        match insts.get(pc) {
            Some(&Inst::Char { c, .. }) => {
                let mut buf = [0u8; 4];
                if let Some(&b) = c.encode_utf8(&mut buf).as_bytes().first() {
                    lead_bytes[b as usize] = true;
                }
            }
            Some(&Inst::Class { class, .. }) => {
                classes.get(class)?.mark_lead_bytes(&mut lead_bytes);
            }
            _ => return None,
        }
    }
    Some(lead_bytes)
}

/// What the code point after the first can begin with, when every path has one.
///
/// The instructions' `next` is reached only once the first code point is consumed, so the
/// closure there holds what is consumed second. If it reaches `Match`, the pattern can
/// stop after one code point: [`consuming_closure`] returns `None` and the filter stays
/// disarmed.
fn second_set(insts: &[Inst], classes: &[ClassSet], first_pcs: &[usize]) -> Option<[bool; 256]> {
    let nexts: Vec<usize> = first_pcs
        .iter()
        .filter_map(|&pc| match insts.get(pc) {
            Some(&Inst::Char { next, .. } | &Inst::Class { next, .. }) => Some(next),
            _ => None,
        })
        .collect();
    if nexts.len() != first_pcs.len() {
        return None;
    }
    let out = consuming_closure(insts, &nexts)?;
    mark_leads(insts, classes, &out)
}

/// Compute [`Program::first`]: what a match can begin with, if that is knowable.
///
/// Walks through assertions: they consume nothing, so assuming each holds only widens the
/// set, and `\bcommit` and `^ERROR` keep a fast path. `None` when the pattern can match
/// the empty string (every position can begin a match) or the set passes [`MAX_FIRST`].
fn first_set(insts: &[Inst], classes: &[ClassSet], start: usize) -> Option<Prefilter> {
    let out = consuming_closure(insts, &[start])?;
    let mut lead_bytes = [false; 256];
    for &pc in &out {
        match insts.get(pc) {
            Some(&Inst::Char { c, .. }) => {
                let mut buf = [0u8; 4];
                if let Some(&b) = c.encode_utf8(&mut buf).as_bytes().first() {
                    lead_bytes[b as usize] = true;
                }
            }
            Some(&Inst::Class { class, .. }) => {
                classes.get(class)?.mark_lead_bytes(&mut lead_bytes);
            }
            _ => return None,
        }
    }
    let second_bytes = second_set(insts, classes, &out);
    Some(Prefilter {
        pcs: out.into_boxed_slice(),
        lead_bytes,
        second_bytes,
    })
}

/// Compute [`Program::start_anchored`]: can this pattern match only at offset 0?
///
/// True when every path out of `start` reaches a `\A` before anything that consumes or
/// accepts; unlike [`first_set`], which walks through assertions, this stops at `\A`.
/// Only [`Assertion::StartText`] anchors: `(?m)^` holds after every newline. Reaching
/// [`Inst::Match`] (`^a|`, the empty pattern) means it can match empty at any offset.
fn start_anchored(insts: &[Inst], start: usize) -> bool {
    let mut seen = vec![false; insts.len()];
    let mut stack = vec![start];
    while let Some(pc) = stack.pop() {
        let Some(slot) = seen.get_mut(pc) else {
            // Out of range: claim nothing.
            return false;
        };
        if *slot {
            continue;
        }
        *slot = true;
        match insts.get(pc) {
            // Anchored on this path: whatever follows runs only at 0.
            Some(&Inst::Assert {
                kind: Assertion::StartText,
                ..
            }) => {}
            Some(&Inst::Split { a, b }) => {
                stack.push(a);
                stack.push(b);
            }
            Some(&Inst::Jump { next } | &Inst::Assert { next, .. }) => stack.push(next),
            // Consumes, or accepts, with no `\A` behind it.
            Some(&Inst::Char { .. } | &Inst::Class { .. } | &Inst::Match) | None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{Flags, parse};

    fn program(pattern: &str) -> Program {
        let ast = parse(pattern, Flags::default()).expect("parses");
        compile(&ast, usize::MAX).expect("compiles")
    }

    /// The second-byte filter EXCLUDES positions, so arming it for a pattern
    /// that can match on one code point would lose matches. Pin both halves.
    ///
    /// Armed means "every path consumes a second code point"; disarmed means
    /// some path can stop after the first, and the disarmed list is the
    /// interesting one — each entry can end after one character by a different
    /// route: it is one character long, its tail is optional, its tail is a
    /// star, or one branch of an alternation is shorter than the other.
    #[test]
    fn the_second_byte_filter_arms_only_when_a_second_code_point_is_certain() {
        let second = |pattern: &str| {
            program(pattern)
                .first
                .and_then(|f| f.second_bytes)
                .map(|bytes| {
                    (0..256usize)
                        .filter(|&b| bytes[b])
                        .filter_map(|b| u8::try_from(b).ok())
                        .map(char::from)
                        .collect::<Vec<char>>()
                })
        };

        assert_eq!(second("ERROR"), Some(vec!['R']));
        assert_eq!(second("(?i)ERROR"), Some(vec!['R', 'r']));
        assert_eq!(second("[ab]c"), Some(vec!['c']));
        assert_eq!(second("ab|cd"), Some(vec!['b', 'd']));
        // Assertions consume nothing, so the second code point is still `o`.
        assert_eq!(second(r"c\bo"), Some(vec!['o']));

        for pattern in [
            "E",    // one code point, so there is no second
            "Ex?",  // the tail is optional
            "Ex*",  // the tail is a star
            "E|Ex", // one branch stops after the first
            "E.*",  // the star can take nothing
            r"E\b", // only an assertion follows, which consumes nothing
        ] {
            assert_eq!(
                second(pattern),
                None,
                "{pattern} can match on one code point, so the filter must stay disarmed",
            );
        }
    }

    /// The ASCII bitmap is consulted instead of its class, so the two must agree over all
    /// of ASCII. The cases cover negation, symbolic perl classes, fold before negate
    /// (`(?i)[^k]`), and classes above or spanning the ASCII boundary.
    #[test]
    fn the_ascii_bitmap_agrees_with_the_class_it_stands_in_for() {
        for pattern in [
            r"[abc]",
            r"[^abc]",
            r"[a-z]",
            r"[^a-z]",
            r"\d",
            r"\D",
            r"\w",
            r"\W",
            r"\s",
            r"\S",
            r"[^\s<>]",
            r"(?i)[^k]",
            r"(?i)[a-z]",
            r".",
            r"(?s).",
            r"[\x{100}-\x{200}]",
            r"[\x{70}-\x{200}]",
            r"[[:alpha:]]",
            r"[[:^digit:]]",
            r"[]a]",
            r"[^]a]",
        ] {
            let prog = program(pattern);
            assert_eq!(
                prog.classes.len(),
                prog.class_ascii.len(),
                "{pattern}: the map must be parallel to the classes",
            );
            for (i, set) in prog.classes.iter().enumerate() {
                let bits = prog.class_ascii[i];
                for code in 0u32..128 {
                    let c = char::from_u32(code).expect("ASCII is a valid scalar value");
                    assert_eq!(
                        bits >> code & 1 != 0,
                        set.matches(c),
                        "{pattern}: class {i} disagrees about {c:?} (U+{code:04X})",
                    );
                }
            }
        }
    }

    /// `start_anchored` lets the search stop walking, so a false positive is a missed
    /// match. Each negative contains a `^` yet has a path to a consuming instruction or
    /// `Match` that does not pass one.
    #[test]
    fn start_anchored_is_exact_about_which_patterns_can_only_match_at_zero() {
        for pattern in [
            r"^a",
            r"^",
            r"^$",
            r"^a$",
            r"\Aa",
            r"\A\Aa",
            r"^a*",
            r"^a|^b",
            r"^(?:a|b)",
            r"^(?:a|ab)+$",
            r"^\d+",
            r"(?i)^ERROR",
        ] {
            assert!(
                program(pattern).start_anchored,
                "{pattern} can only match at offset 0",
            );
        }
        for pattern in [
            // An unanchored branch.
            r"a|^b",
            r"^a|b",
            r"(^|x)y",
            r"(?:^)|a",
            // A loop or an option that can be skipped entirely, so `Match` is
            // reachable without the assertion.
            r"(^a)*",
            r"(?:^a)?b",
            // `(?m)^` is `StartLine`, satisfied after every newline.
            r"(?m)^a",
            r"(?m:^)a",
            r"(?:(?m)^)a",
            // No `^` at all, and the empty pattern, which matches everywhere.
            r"a",
            r"\bcommit",
            r"",
            r"a$",
        ] {
            assert!(
                !program(pattern).start_anchored,
                "{pattern} can match somewhere other than offset 0",
            );
        }
    }

    /// The byte map is the fast half of the prefilter, and its two invariants
    /// are what make a byte-at-a-time scan legal: continuation bytes are never
    /// marked, and every code point a candidate accepts has its lead byte
    /// marked.
    #[test]
    fn the_prefilter_byte_map_marks_lead_bytes_only() {
        let mut buf = [0u8; 4];
        for pattern in [
            "abc",
            r"\bcommit",
            "[a-z]+x",
            "foo|bar",
            r"\d+z",
            r"[^\s]x",
            "\u{4f60}\u{597d}",
            "(?i)Hello",
            r"[\w-]+@",
            ".x",
        ] {
            let prefilter = program(pattern).first.expect("armed");
            for b in 0x80..=0xbfu8 {
                assert!(
                    !prefilter.lead_bytes[b as usize],
                    "{pattern:?} marked continuation byte {b:#04x}"
                );
            }
            // Anything the pattern can start with must survive the byte test.
            for cp in [
                0u32,
                b'a'.into(),
                b'Z'.into(),
                b'0'.into(),
                0xe9,
                0x4f60,
                0x1f600,
            ] {
                let Some(c) = char::from_u32(cp) else {
                    continue;
                };
                let haystack = format!("{c}");
                let re = crate::Regex::new(pattern).expect("compiles");
                if re.is_match(&haystack)
                    && let Some(&lead) = c.encode_utf8(&mut buf).as_bytes().first()
                {
                    assert!(
                        prefilter.lead_bytes[lead as usize],
                        "{pattern:?} can start with {c:?} but its lead byte is unmarked"
                    );
                }
            }
        }
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Ast`] → NFA [`Program`], under a hard ceiling.
//!
//! A textbook Thompson construction, emitted right-to-left: every fragment is
//! compiled knowing the instruction control flows to when it succeeds, so
//! concatenation is a fold from the end and no back-patching table is needed.
//! The two exceptions are the loops (`*`, `+`, `{n,}`), where an [`Inst::Jump`]
//! placeholder is reserved first and overwritten with the real
//! [`Inst::Split`] once the body's entry point is known.
//!
//! ## The ceiling
//!
//! [`Inst::Split`] is the only branch, and there is exactly one instruction per
//! code point a pattern can consume, so `x{n}` costs `n` instructions and
//! `(a{200}){200}` costs forty thousand. That is precisely why the call sites
//! pass `size_limit`: a thirteen-byte pattern can otherwise ask for a
//! multi-megabyte automaton. Every single instruction pushed re-checks the
//! budget, so an over-large pattern fails after a few thousand instructions
//! rather than allocating until it is told to stop.
//!
//! The budget is charged in *bytes*, and it counts the whole cost of the
//! program rather than just its instruction vector: an instruction also forces
//! per-instruction slots in both of the Pike VM's thread lists, and that memory
//! is every bit as real. See [`INST_COST`].

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
    /// Zero width, unconditional. Only ever emitted as the placeholder a loop
    /// reserves before its body's entry point is known, and always overwritten
    /// with a [`Split`](Self::Split) once it is — so a finished program holds
    /// none. Handled everywhere anyway, because a placeholder that leaks would
    /// otherwise be a silent wrong answer rather than a loud one.
    Jump { next: usize },
    /// Zero width, conditional on the surrounding text.
    Assert { kind: Assertion, next: usize },
    /// The pattern is satisfied.
    Match,
}

/// Bytes one instruction is charged against `size_limit`.
///
/// The instruction itself, plus the slots it forces in the Pike VM's two thread
/// lists: a `Thread` in each list's dense array and a generation stamp in each
/// list's visited array. A program of `n` instructions really does cost
/// `n * INST_COST` bytes to hold and to run, so that is what the limit bounds.
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
    Ok(Program { insts: c.insts, classes: c.classes, start, first })
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
            Ast::Repeat { node, min, max, greedy } => {
                self.emit_repeat(node, *min, *max, *greedy, next)
            }
        }
    }

    /// Emit a quantified subexpression.
    ///
    /// `x*` gets one of two shapes, and which one is not a matter of taste.
    ///
    /// When the body cannot match the empty string, a single split serves as
    /// both the entry and the loop head: enter the body or leave, and the body
    /// comes back here. Minimal, and correct — every iteration consumes, so the
    /// loop can never come back to its head without having made progress.
    ///
    /// When the body *can* match nothing, that shape gives the wrong answer.
    /// Take `(|a)*`, whose body prefers to match nothing. The right result (and
    /// the `regex` crate's) is an empty match: the first iteration consumes
    /// nothing, an empty iteration ends the loop, and the loop exits. But "came
    /// back to the loop head having consumed nothing" is a state the simulation
    /// has already entered at this position, so with one shared split that path
    /// simply dies and the only survivor is the `a` branch — which wrongly
    /// consumes a character. Splitting entry from loop head fixes it: the
    /// returning path reaches the *post-body* split instead, takes its exit arm,
    /// and gets to `Match` ahead of the `a` branch, which is the priority it
    /// deserves.
    ///
    /// Using the two-split shape for *every* star would be a different bug. The
    /// extra hop makes the loop head reachable one step earlier from inside the
    /// body, which reorders the exit against the body's own alternatives when a
    /// thread resumes mid-body — `(.*?){2,}\b` on `" ab"` starts matching at 0
    /// either way, but ends at 1 with the right shape and at 3 with the wrong
    /// one. Two shapes, chosen by nullability, is what the oracle does too.
    ///
    /// ## The mandatory copies stop at their fixed point
    ///
    /// `size_limit` is charged inside [`Compiler::push`] and
    /// [`Compiler::add_class`], so a `min` that replays a body emitting *no*
    /// instructions — `(?:){n}`, `(){n}`, `(?:a{0}){n}`, `(?:\b{0}){n}` — would
    /// spin `min` times without ever consulting the ceiling. `(?:){4294967295}`
    /// is sixteen bytes of pattern and passes the length gate at every call
    /// site; nested, `(?:(?:){4294967295}){4294967295}` is quadratic in the two
    /// counts and would not finish this century. The ceiling is the only defence
    /// the call sites have against exactly that, so it must not be bypassable.
    ///
    /// [`Compiler::emit`] returns its `next` argument unchanged **precisely
    /// when** it emitted nothing: every `push` hands back `insts.len() - 1`,
    /// which is strictly greater than any index allocated before it, so a
    /// returned value equal to the argument cannot have come from a push. That
    /// makes "the copy returned what it was given" an exact, O(1) test for "this
    /// body is a no-op", and every further copy is then the same no-op — so the
    /// loop breaks. Behaviour is identical because the emitted program is
    /// identical; only the loop that emits nothing is skipped.
    fn emit_repeat(
        &mut self,
        node: &Ast,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        next: usize,
    ) -> Result<usize, Error> {
        match max {
            // `x{n,}` — one self-re-entering body, with the mandatory copies in
            // front of it. `x*` (n = 0) needs an entry as well as a loop head,
            // because the whole loop has to be skippable.
            None => {
                let post = self.push(Inst::Jump { next })?;
                let body = self.emit(node, post)?;
                self.insts[post] = split(body, next, greedy);
                if min > 0 {
                    let mut entry = body;
                    for _ in 0..min - 1 {
                        let copy = self.emit(node, entry)?;
                        // A body that emits nothing is a fixed point: see
                        // [`Compiler::emit_repeat`]'s note on `(?:){n}`.
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

/// What a match can begin with — the search's fast path over uninteresting text.
///
/// `pcs` are the consuming instructions a match's *first* code point can come
/// from. `lead_bytes` marks the leading UTF-8 byte of every such code point, so
/// a position can be rejected with one array lookup instead of a decode and a
/// set of class tests.
///
/// Both are supersets, never subsets, and that asymmetry is the whole safety
/// argument: the search only skips a position when *nothing* here could match
/// there, so an over-broad prefilter costs time and can never cost a match.
#[derive(Debug)]
pub(crate) struct Prefilter {
    pub(crate) pcs: Box<[usize]>,
    pub(crate) lead_bytes: [bool; 256],
}

/// Compute [`Program::first`]: what a match can begin with, if that is knowable.
///
/// Assertions are walked *through* rather than bailed on. They consume nothing,
/// so whatever a thread does after one, its first consumed code point is still
/// in this set — optimistically assuming every assertion holds only widens it,
/// and widening is safe. That matters: `\bcommit` and `^ERROR` are exactly the
/// shapes a terminal searches for, and bailing on the leading assertion would
/// leave the two most common patterns in the tree with no fast path at all.
///
/// Bails to `None` on [`Inst::Match`] — the pattern can match the empty string,
/// so every position can begin a match and there is nothing to skip — and when
/// the candidate set grows past the point where testing it beats running the
/// simulation.
fn first_set(insts: &[Inst], classes: &[ClassSet], start: usize) -> Option<Prefilter> {
    /// Past this many alternatives the prefilter costs more than it saves.
    const MAX_FIRST: usize = 16;

    let mut seen = vec![false; insts.len()];
    let mut stack = vec![start];
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
    if out.is_empty() {
        return None;
    }
    let mut lead_bytes = [false; 256];
    for &pc in &out {
        match insts.get(pc) {
            Some(&Inst::Char { c, .. }) => {
                let mut buf = [0u8; 4];
                if let Some(&b) = c.encode_utf8(&mut buf).as_bytes().first() {
                    lead_bytes[b as usize] = true;
                }
            }
            // A class's own byte marking is conservative on the non-ASCII side.
            Some(&Inst::Class { class, .. }) => {
                classes.get(class)?.mark_lead_bytes(&mut lead_bytes);
            }
            _ => return None,
        }
    }
    Some(Prefilter { pcs: out.into_boxed_slice(), lead_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{Flags, parse};

    fn program(pattern: &str) -> Program {
        let ast = parse(pattern, Flags::default()).expect("parses");
        compile(&ast, usize::MAX).expect("compiles")
    }

    /// The budget is bytes, and an instruction's real cost includes the slots it
    /// forces in the simulation's two thread lists. Pin the arithmetic so a
    /// change to either structure has to be a deliberate one.
    #[test]
    fn instruction_cost_covers_the_whole_program() {
        assert_eq!(
            INST_COST,
            size_of::<Inst>() + 2 * (size_of::<Thread>() + size_of::<u32>())
        );
        assert!(INST_COST >= size_of::<Inst>());
    }

    /// One instruction per code point a pattern can consume — which is what
    /// makes `(a{200}){200}` a forty-thousand-instruction automaton, and why the
    /// ceiling exists at all.
    #[test]
    fn bounded_repetition_expands_one_instruction_per_code_point() {
        assert_eq!(program("a{200}").insts.len(), 201, "200 chars plus Match");
        let amplifier = program("(a{200}){200}");
        assert_eq!(amplifier.insts.len(), 40_001);
        assert!(
            amplifier.insts.len() * INST_COST > 1 << 20,
            "the amplifier must exceed the callers' 1 MiB ceiling"
        );
        assert!(
            amplifier.insts.len() * INST_COST < 10 * (1 << 20),
            "and must stay under the 10 MiB default, as it does in the `regex` crate"
        );
    }

    /// The ceiling stops emission rather than reporting after the fact.
    #[test]
    fn the_ceiling_stops_emission() {
        let ast = parse("a{100000000}", Flags::default()).expect("parses");
        let limit = 4_096;
        match compile(&ast, limit) {
            Err(Error::CompiledTooBig(reported)) => assert_eq!(reported, limit),
            other => panic!("expected the ceiling, got {other:?}"),
        }
    }

    /// A body that compiles to *no* instructions must not be able to walk past
    /// the ceiling.
    ///
    /// `size_limit` is charged inside [`Compiler::push`] and
    /// [`Compiler::add_class`], so a mandatory-copy loop that replays a body
    /// emitting nothing never consults the budget at all. Measured through the
    /// exact gate the call sites build with — `aterm_observe::row_matcher` and
    /// `SearchIndex::compile_regex` both say
    /// `.size_limit(1 << 20).dfa_size_limit(1 << 20)` — the sixteen-byte
    /// `(?:){4294967295}` returned `Ok` after 6.377 seconds, and the nested form
    /// is quadratic in the two counts and would not have finished this century.
    /// Every pattern here is far under the callers' `MAX_REGEX_PATTERN_LEN` of
    /// 1024 bytes, so the length gate lets them all through and the ceiling is
    /// the only defence there is.
    #[test]
    fn a_body_that_emits_nothing_cannot_outrun_the_ceiling() {
        use std::time::{Duration, Instant};

        // The two call sites' bound, verbatim.
        fn gate(pattern: &str) -> Result<crate::Regex, Error> {
            crate::RegexBuilder::new(pattern)
                .size_limit(1 << 20)
                .dfa_size_limit(1 << 20)
                .build()
        }

        // The real cost of each of these is a parse and two instructions —
        // microseconds. A second is orders of magnitude above that and orders of
        // magnitude below the 6.377 s the flat case took before the fixed-point
        // break, so it separates "fixed" from "broken" without being fragile on
        // a loaded machine.
        let budget = Duration::from_secs(1);
        for pattern in [
            "(?:){4294967295}",                 // the measured case
            "(?:(?:){4294967295}){4294967295}", // nested: quadratic before
            "(?:){4294967295,}",                // the `{n,}` arm's own loop
            "(){4294967295}",
            "(?:a{0}){4294967295}",
            r"(?:\b{0}){4294967295}",
        ] {
            let started = Instant::now();
            let re = gate(pattern).expect("a no-op body is a legal, tiny program");
            let elapsed = started.elapsed();
            assert!(
                elapsed < budget,
                "{pattern:?} took {elapsed:?}: a body that emits nothing is \
                 being replayed instead of recognised as a fixed point"
            );
            assert!(re.is_match(""), "{pattern:?} still matches the empty string");
            assert!(re.is_match("abc"), "{pattern:?} still matches anywhere");
        }

        // And the break must not have disarmed the ceiling: a body that *does*
        // emit still gets stopped, with the ordinary error carrying the limit.
        for pattern in ["(?:a){4294967295}", "(?:a?){4294967295}", "a{100000000}"] {
            match gate(pattern) {
                Err(Error::CompiledTooBig(reported)) => assert_eq!(reported, 1 << 20),
                other => panic!("{pattern:?} must still hit the ceiling, got {other:?}"),
            }
        }
    }

    /// The prefilter must be `Some` only when skipping is sound — never for a
    /// pattern that can match the empty string — and must survive a leading
    /// assertion, because `\bcommit` and `^ERROR` are what a terminal searches
    /// for and they would otherwise get no fast path at all.
    #[test]
    fn the_prefilter_is_armed_exactly_when_skipping_is_sound() {
        assert!(program("abc").first.is_some(), "a literal prefix");
        assert!(program("[a-z]+x").first.is_some());
        assert!(program("foo|bar").first.is_some(), "two alternatives");
        assert!(program(r"\bfoo").first.is_some(), "an assertion consumes nothing");
        assert!(program("^foo").first.is_some());
        assert!(program(r"^\s*\bcommit\b").first.is_some());
        assert!(program("a{0,3}b").first.is_some(), "the `b` is still required");

        assert!(program("a*").first.is_none(), "matches empty");
        assert!(program("").first.is_none(), "matches empty");
        assert!(program("(?:)|a").first.is_none(), "an empty branch");
        assert!(program(r"\b").first.is_none(), "an assertion alone matches empty");
    }

    /// The byte map is the fast half of the prefilter, and its two invariants
    /// are what make a byte-at-a-time scan legal: continuation bytes are never
    /// marked, and every code point a candidate accepts has its lead byte
    /// marked.
    #[test]
    fn the_prefilter_byte_map_marks_lead_bytes_only() {
        let mut buf = [0u8; 4];
        for pattern in [
            "abc", r"\bcommit", "[a-z]+x", "foo|bar", r"\d+z", r"[^\s]x",
            "\u{4f60}\u{597d}", "(?i)Hello", r"[\w-]+@", ".x",
        ] {
            let prefilter = program(pattern).first.expect("armed");
            for b in 0x80..=0xbfu8 {
                assert!(
                    !prefilter.lead_bytes[b as usize],
                    "{pattern:?} marked continuation byte {b:#04x}"
                );
            }
            // Anything the pattern can start with must survive the byte test.
            for cp in [0u32, b'a'.into(), b'Z'.into(), b'0'.into(), 0xe9, 0x4f60, 0x1f600] {
                let Some(c) = char::from_u32(cp) else { continue };
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

    /// Past a handful of alternatives the prefilter costs more than it saves, so
    /// it disarms itself rather than testing a long list per code point.
    #[test]
    fn the_prefilter_disarms_when_it_stops_paying() {
        let wide: Vec<String> = (0..40).map(|i| format!("w{i}x")).collect();
        assert!(program(&wide.join("|")).first.is_none());
    }

    /// A star's shape is chosen by whether its body can match nothing: one
    /// split when it cannot, two when it can. Both halves are load-bearing —
    /// the wrong one in either direction is a wrong answer, not a slower one —
    /// so assert the shape rather than only its effect.
    #[test]
    fn a_star_gets_two_splits_only_when_its_body_is_nullable() {
        let splits = |p: &str| {
            program(p)
                .insts
                .iter()
                .filter(|i| matches!(i, Inst::Split { .. }))
                .count()
        };
        assert_eq!(splits("a*"), 1, "a non-nullable body needs no separate entry");
        assert_eq!(splits(".*?"), 1);
        assert_eq!(splits("a+"), 1, "the mandatory body already precedes the loop");
        assert_eq!(splits("a?"), 1);
        assert_eq!(splits("a{2,}"), 1);
        // `(?:a?)` is nullable, so its star needs the entry split as well — and
        // the body's own `?` accounts for one more.
        assert_eq!(splits("(?:a?)*"), 3);
        assert_eq!(splits("(?:)*"), 2);
    }

    /// Nullability is what picks the shape, so pin the predicate itself.
    #[test]
    fn nullability_is_computed_over_the_whole_tree() {
        let nullable = |p: &str| {
            matches_empty(&parse(p, Flags::default()).expect("parses"))
        };
        assert!(nullable("") && nullable("a*") && nullable("a{0,2}") && nullable("(?:)"));
        assert!(nullable(r"\b") && nullable("^") && nullable("a|") && nullable("a*b*"));
        assert!(!nullable("a") && !nullable("a+") && !nullable("[a-z]{2}"));
        assert!(!nullable("a*b") && !nullable("(?:a|b)") && !nullable("a{1,}"));
    }

    /// Greediness is the order of a split's arms, and nothing else.
    #[test]
    fn greediness_is_the_arm_order() {
        let entry = |p: &str| {
            let prog = program(p);
            prog.insts[prog.start]
        };
        let (Inst::Split { a: ga, b: gb }, Inst::Split { a: la, b: lb }) =
            (entry("a*"), entry("a*?"))
        else {
            panic!("a star starts at a split")
        };
        assert_eq!((ga, gb), (lb, la), "lazy is greedy with the arms swapped");
    }
}

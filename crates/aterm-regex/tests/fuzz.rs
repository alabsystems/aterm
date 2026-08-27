// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Generated differential fuzzing against the `regex` crate.
//!
//! A hand-written corpus tests the shapes someone thought of. This tests the
//! ones nobody did: patterns drawn from a grammar covering the whole supported
//! syntax — every quantifier and its lazy form, alternation with empty branches,
//! all eight assertions, classes with ranges, perl shorthands and POSIX names,
//! and the `i`/`s`/`m`/`U`/`x` group flags — crossed with haystacks drawn from an
//! alphabet that mixes ASCII, a combining mark, CJK, an accented Latin letter
//! and newlines, asserting `find_iter` and `is_match` agree with the oracle on
//! every single case.
//!
//! It has earned its keep. Three real defects in this engine came out of it and
//! nowhere else: a star whose compiled shape made a nullable body prefer the
//! wrong iteration, a prefilter skip that left stale visited marks behind and
//! silently dropped a match, and four separate `(?x)` whitespace rules.
//!
//! Randomness comes from a deterministic LCG rather than `rand` — the crate
//! takes no dependencies, and a fixed seed means a failure is reproducible by
//! re-running the test rather than by hoping. `FUZZ_SEED` and `FUZZ_N` override
//! the defaults for a longer soak.
//!
//! ## Arbitration, because the oracle is not infallible
//!
//! `regex` 1.12 has a defect this fuzzer *does* hit (see
//! `the_oracle_misses_a_leftmost_start_and_this_engine_does_not` in
//! `differential.rs`): its unanchored search can miss the leftmost start. So a
//! disagreement is not automatically a failure here — it is referred to an
//! arbiter, which is the oracle itself driven anchored, through a lazy prefix
//! whose capture group reports the span. Anchored, `regex` is right; that is how
//! we know which engine to believe. If the arbiter sides with the oracle, this
//! engine is wrong and the test fails.
//!
//! An escape hatch that decides its own scope is not an escape hatch, so this
//! one is fenced on three sides. The arbiter's prefix flag is **scoped**, or it
//! rewrites the pattern it is meant to adjudicate (`(?s)` leaking into a bare
//! `.` made a planted bug pass — see [`arbiter_from`]). It arbitrates the
//! **whole `find_iter` sequence**, not just the first span, or a corrupted tail
//! is excusable by construction (see [`arbiter_all`]). And the number of
//! excused cases is **asserted**, not merely printed, so laundering thousands of
//! disagreements fails instead of scrolling past. `is_match` and the
//! char-boundary checks run before arbitration, on every case, rather than
//! behind the `continue` that skipped them exactly when they mattered most.
//! [`the_arbiter_reproduces_find_iter`] then tests the arbiter itself, because
//! at the default seed the engines agree everywhere and nothing else would.

/// Deterministic LCG (the Knuth/MMIX multiplier), so a failing case is
/// reproducible from the seed alone.
struct Lcg(u64);

impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        let i = self.below(xs.len() as u32) as usize;
        &xs[i]
    }
}

/// The haystack alphabet, and the literals the generator draws from: ASCII that
/// the metacharacters interact with, a combining mark (a `\w` that is not
/// alphabetic), a CJK code point, an accented Latin letter, a newline for `.`
/// and `(?m)`, and `K` for the KELVIN-SIGN fold orbit.
const ALPHABET: &[&str] = &[
    "a", "b", "c", "x", " ", "-", ".", "0", "9", "_", "\u{e9}", "\u{4f60}", "\n",
    "\u{301}", "K",
];

fn gen_atom(r: &mut Lcg, out: &mut String, depth: u32) {
    match r.below(10) {
        0 => out.push_str(r.pick(&[".", r"\d", r"\w", r"\s", r"\D", r"\W", r"\S"])),
        1 => out.push_str(r.pick(&["^", "$", r"\b", r"\B", r"\A", r"\z", r"\<", r"\>"])),
        2 => {
            out.push('[');
            if r.below(3) == 0 {
                out.push('^');
            }
            for _ in 0..1 + r.below(3) {
                match r.below(4) {
                    0 => out.push_str(r.pick(&[r"\d", r"\w", r"\s", r"\S"])),
                    1 => out.push_str(r.pick(&[
                        "a-c", "0-9", "x-z", "A-Z", "\u{e0}-\u{e9}",
                    ])),
                    2 => out.push_str(r.pick(&["[:alpha:]", "[:digit:]", "[:^space:]"])),
                    // No bare `]` and no bare `-`: both would compose into the
                    // nested-class and set-operation syntax this engine refuses
                    // by design, which is tested exhaustively elsewhere.
                    _ => out.push_str(r.pick(&["a", "b", r"\.", r"\-", r"\\", r"\]", "^", "0"])),
                }
            }
            out.push(']');
        }
        3 => {
            if r.below(7) == 0 {
                // Unique, because a duplicate group name is an error in both.
                let n = r.next_u32();
                out.push_str(&format!("(?<g{n}>"));
            } else {
                out.push_str(r.pick(&["(", "(?:", "(?i:", "(?s:", "(?m:", "(?U:", "(?x:"]));
            }
            gen_alt(r, out, depth + 1);
            out.push(')');
        }
        _ => {
            let a = r.pick(ALPHABET);
            if a.chars().all(|c| c.is_ascii_alphanumeric()) || r.below(2) == 0 {
                out.push_str(a);
            } else {
                out.push_str(&aterm_regex::escape(a));
            }
        }
    }
}

fn gen_piece(r: &mut Lcg, out: &mut String, depth: u32) {
    if depth > 3 {
        out.push_str(r.pick(ALPHABET));
        return;
    }
    gen_atom(r, out, depth);
    if r.below(3) == 0 {
        out.push_str(r.pick(&["*", "+", "?", "{2}", "{0,2}", "{1,3}", "{2,}", "{0}"]));
        if r.below(3) == 0 {
            out.push('?');
        }
    }
}

fn gen_alt(r: &mut Lcg, out: &mut String, depth: u32) {
    for i in 0..1 + r.below(3) {
        if i > 0 {
            out.push('|');
        }
        if r.below(6) == 0 {
            continue; // an empty branch
        }
        for _ in 0..1 + r.below(4) {
            gen_piece(r, out, depth);
        }
    }
}

fn gen_haystack(r: &mut Lcg) -> String {
    let mut s = String::new();
    for _ in 0..r.below(12) {
        s.push_str(r.pick(ALPHABET));
    }
    s
}

/// The constructs this engine refuses on purpose, by the phrase its message
/// uses. Every one of them is proved to be a refusal — and proved to be
/// something the oracle accepts — in `differential.rs`.
const REFUSALS: &[&str] = &[
    "Unicode property classes",
    "set operations",
    "nested character classes",
    "byte-oriented",
    "CRLF mode",
];

/// The oracle, driven anchored, reporting the leftmost match that starts at or
/// after code point `skip`.
///
/// A lazy prefix consumes the shortest run it can and the capture group reports
/// where the real match lands. Anchored, `regex` finds the leftmost start
/// reliably — and because the haystack is never sliced, `^`, `\b` and `\A`
/// inside the pattern still see their true context.
///
/// ## Why the flag is scoped and the anchor is `\A`
///
/// This used to read `(?s)^(?:.)*?({pattern})`, and both of those choices were
/// wrong in the same way: an unscoped flag and a line anchor are *reinterpreted
/// by the pattern under test*. `(?s)` at the front applies to `{pattern}` too,
/// so every bare `.` in the pattern silently gained the ability to match `\n` —
/// which means the arbiter was answering a different question than the one the
/// two engines had just disagreed about. Since a disagreement is excused when
/// the arbiter agrees with *this* engine, that turned the arbiter into a
/// laundering machine: a real `.`-matches-newline bug planted in `parse.rs` made
/// this test **pass**, reporting 288 excused "oracle defects" where a clean run
/// reports none at all.
///
/// `(?s:…)` fixes that: the flag reaches only the prefix. `^` became `\A` for a
/// weaker but related reason — measured, a pattern's own `(?m)` does *not* reach
/// back and re-point the leading anchor, so this half is hardening rather than a
/// demonstrated defect; `\A` simply has no flag that can reinterpret it, so the
/// arbiter no longer depends on that scoping rule holding.
fn arbiter_from(pattern: &str, haystack: &str, skip: usize) -> Option<Option<(usize, usize)>> {
    let re = regex::Regex::new(&format!(r"\A(?s:(?:.){{{skip}}}(?:.)*?)({pattern})")).ok()?;
    Some(
        re.captures(haystack)
            .and_then(|c| c.get(1))
            .map(|m| (m.start(), m.end())),
    )
}

/// The arbiter's verdict on a whole `find_iter` sequence, not just its head.
///
/// Arbitrating only the first span was the second half of the same defect.
/// `find_iter` disagreements do not have to start at the beginning: an
/// off-by-one in the empty-match advance typically leaves the first span
/// identical and corrupts every span after it, so "the arbiter agrees with our
/// first span" excused exactly the bugs hardest to find by hand. This walks the
/// whole sequence, re-anchoring after each match and applying the documented
/// advance rule from `Matches::next` — step one code point past an empty match,
/// and drop an empty match that lands where the previous match ended.
///
/// `None` means arbitration is unavailable (the anchored form did not compile,
/// or the sequence ran away), not that the sequence is empty.
fn arbiter_all(pattern: &str, haystack: &str) -> Option<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut last_end = 0usize;
    let mut last_match: Option<usize> = None;
    while last_end <= haystack.len() {
        let skip = haystack[..last_end].chars().count();
        let Some((start, end)) = arbiter_from(pattern, haystack, skip)? else {
            break;
        };
        if start == end {
            last_end = end
                + haystack
                    .get(end..)
                    .and_then(|rest| rest.chars().next())
                    .map_or(1, char::len_utf8);
            if last_match == Some(end) {
                continue;
            }
        } else {
            last_end = end;
        }
        last_match = Some(end);
        out.push((start, end));
        if out.len() > 512 {
            return None;
        }
    }
    Some(out)
}

#[test]
fn generated_patterns_match_the_oracle() {
    let seed: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x2026_0822);
    let patterns: u32 = std::env::var("FUZZ_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8_000);

    let mut r = Lcg(seed);
    let mut cases = 0u32;
    let mut both_rejected = 0u32;
    let mut oracle_defects = 0u32;
    let mut refused = 0u32;
    let mut failures: Vec<String> = Vec::new();

    for _ in 0..patterns {
        let mut pattern = String::new();
        gen_alt(&mut r, &mut pattern, 0);
        let haystacks: Vec<String> = (0..5).map(|_| gen_haystack(&mut r)).collect();

        let (mine, theirs) = (
            aterm_regex::Regex::new(&pattern),
            regex::Regex::new(&pattern),
        );
        let (mine, theirs) = match (mine, theirs) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(_), Err(_)) => {
                both_rejected += 1;
                continue;
            }
            // The size ceilings are charged differently, so a pattern near the
            // 10 MiB default may pass one and not the other. Nothing to compare.
            (Ok(_), Err(regex::Error::CompiledTooBig(_)))
            | (Err(aterm_regex::Error::CompiledTooBig(_)), Ok(_)) => continue,
            (Err(e), Ok(_)) => {
                // The generator can still compose one of the constructs this
                // engine refuses by design (the refusals are enumerated and
                // proved against the oracle in `differential.rs`). A refusal
                // that names itself is expected; any other is a failure.
                let msg = e.to_string();
                if REFUSALS.iter().any(|r| msg.contains(r)) {
                    refused += 1;
                } else {
                    failures.push(format!("{pattern:?}: this engine refused it:\n{e}"));
                }
                continue;
            }
            (Ok(_), Err(e)) => {
                failures.push(format!("{pattern:?}: the oracle refused it: {e}"));
                continue;
            }
        };

        for haystack in &haystacks {
            cases += 1;
            let a: Vec<(usize, usize)> =
                mine.find_iter(haystack).map(|m| (m.start(), m.end())).collect();
            let b: Vec<(usize, usize)> =
                theirs.find_iter(haystack).map(|m| (m.start(), m.end())).collect();
            // These two run on EVERY case, including the ones arbitration goes
            // on to excuse. Behind a `continue` they were silently skipped for
            // exactly the cases most likely to be wrong.
            assert_eq!(
                mine.is_match(haystack),
                theirs.is_match(haystack),
                "is_match disagreed for {pattern:?} on {haystack:?}"
            );
            for &(start, end) in &a {
                assert!(
                    haystack.is_char_boundary(start) && haystack.is_char_boundary(end),
                    "{pattern:?} on {haystack:?} produced an offset off a code-point boundary"
                );
            }
            if a != b {
                // Refer it to the arbiter rather than assuming the oracle wins —
                // and arbitrate the WHOLE sequence. Matching only the head let a
                // corrupted tail through by construction.
                match arbiter_all(&pattern, haystack) {
                    Some(ref verdict) if *verdict == a => oracle_defects += 1,
                    verdict => failures.push(format!(
                        "{pattern:?} on {haystack:?}\n     mine = {a:?}\n   oracle = {b:?}\n  arbiter = {verdict:?}"
                    )),
                }
            }
        }
        if failures.len() > 20 {
            break;
        }
    }

    println!(
        "seed {seed:#x}: {cases} cases over {patterns} patterns \
         ({both_rejected} mutually rejected, {refused} documented refusals, \
         {oracle_defects} oracle defects arbitrated)"
    );
    // The disagreements come FIRST, because the loop above stops early once
    // twenty of them pile up — so a run with real failures is also a run with a
    // short corpus, and asserting the size first reports the symptom ("the
    // corpus shrank to 2,375 cases") in place of the cause. Measured: with a
    // `.`-matches-newline bug planted, that ordering hid every disagreement
    // behind a message about the corpus.
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(cases > 20_000, "the corpus shrank to {cases} cases");
    // An excused disagreement is a claim that the ORACLE is wrong, so the number
    // of them has to be a rounding error or the excuse has stopped meaning
    // anything. Unbounded, this counter was a place bugs could hide in the
    // thousands while the test printed a cheerful summary and passed: the
    // byte-vs-code-point empty-match advance bug took it from a handful to
    // 8,697. One in a thousand cases is far above the handful a clean run needs
    // and far below anything systematic.
    assert!(
        u64::from(oracle_defects) * 1_000 <= u64::from(cases),
        "{oracle_defects} of {cases} cases were excused as oracle defects; that is no \
         longer a rounding error, so the arbiter is laundering a real disagreement"
    );
}

/// The arbiter is under test too, because nothing else tests it.
///
/// At the default seed the two engines agree on all 39,650 cases, so the
/// arbitration branch never runs — an arbiter that had rotted into nonsense
/// would sit there undetected until the one day it was needed, which is the day
/// it decides whether a real disagreement is a bug or an excuse. So it is
/// checked directly: first on the exact case that exposed the unscoped flag,
/// then against both engines over generated patterns, where its whole sequence
/// must reproduce `find_iter` — empty-match advance rule included.
#[test]
fn the_arbiter_reproduces_find_iter() {
    // The scoping regression, stated as a fact about `.` and `\n`. The old
    // arbiter answered `Some(vec![(0, 1)])` here while both real engines say
    // there is no match, which is precisely how it laundered a planted bug.
    assert_eq!(arbiter_all(".", "\n"), Some(vec![]), "`.` must not match a newline");
    assert_eq!(arbiter_all("(?s).", "\n"), Some(vec![(0, 1)]), "`(?s).` must");
    // The haystack is never sliced, so the pattern's own anchors see real
    // context: `^` is start-of-text until the pattern itself says otherwise.
    assert_eq!(arbiter_all("^a", "b\na"), Some(vec![]));
    assert_eq!(arbiter_all("(?m)^a", "b\na"), Some(vec![(2, 3)]));
    // Empty matches, where the advance rule lives.
    assert_eq!(arbiter_all("a*", "aab"), Some(vec![(0, 2), (3, 3)]));
    assert_eq!(arbiter_all("", "ab"), Some(vec![(0, 0), (1, 1), (2, 2)]));
    // ...and over a multi-byte code point, where a byte-wise advance would slip.
    assert_eq!(arbiter_all("", "a\u{4f60}"), Some(vec![(0, 0), (1, 1), (4, 4)]));

    let mut r = Lcg(0x2026_0826);
    let mut checked = 0usize;
    let mut with_empty = 0usize;
    for _ in 0..400 {
        let mut pattern = String::new();
        gen_alt(&mut r, &mut pattern, 0);
        let (Ok(mine), Ok(theirs)) = (
            aterm_regex::Regex::new(&pattern),
            regex::Regex::new(&pattern),
        ) else {
            continue;
        };
        for _ in 0..3 {
            let haystack = gen_haystack(&mut r);
            let a: Vec<(usize, usize)> =
                mine.find_iter(&haystack).map(|m| (m.start(), m.end())).collect();
            let b: Vec<(usize, usize)> =
                theirs.find_iter(&haystack).map(|m| (m.start(), m.end())).collect();
            // Where the engines already agree there is a known-good answer, and
            // the arbiter has to produce it.
            if a != b {
                continue;
            }
            let Some(verdict) = arbiter_all(&pattern, &haystack) else {
                continue;
            };
            assert_eq!(
                verdict, a,
                "the arbiter does not reproduce find_iter for {pattern:?} on {haystack:?}"
            );
            if a.iter().any(|&(s, e)| s == e) {
                with_empty += 1;
            }
            checked += 1;
        }
    }
    assert!(checked > 500, "the arbiter corpus shrank to {checked} cases");
    assert!(
        with_empty > 50,
        "only {with_empty} cases exercised the empty-match advance rule"
    );
}

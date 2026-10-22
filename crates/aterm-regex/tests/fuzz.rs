// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Generated fuzzing over the whole supported syntax: every quantifier and its lazy form,
//! alternation with empty branches, all eight assertions, classes with ranges, perl
//! shorthands and POSIX names, and the `i`/`s`/`m`/`U`/`x` group flags, crossed with
//! haystacks mixing ASCII, a combining mark, CJK, an accented letter and newlines.
//!
//! Every answer is folded into a digest, pinned when all ~40k cases matched the `regex`
//! crate. `FUZZ_SEED` and `FUZZ_N` override the defaults for a longer soak, which then
//! checks consistency only, not the digest.

/// Deterministic LCG (the Knuth/MMIX multiplier): a failing case reproduces from the seed.
struct Lcg(u64);

impl Lcg {
    const fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    const fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }

    const fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        let i = self.below(xs.len() as u32) as usize;
        xs[i]
    }
}

/// The haystack alphabet, and the literals the generator draws from: ASCII that
/// the metacharacters interact with, a combining mark (a `\w` that is not
/// alphabetic), a CJK code point, an accented Latin letter, a newline for `.`
/// and `(?m)`, and `K` for the KELVIN-SIGN fold orbit.
const ALPHABET: &[&str] = &[
    "a", "b", "c", "x", " ", "-", ".", "0", "9", "_", "\u{e9}", "\u{4f60}", "\n", "\u{301}", "K",
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
                    1 => out.push_str(r.pick(&["a-c", "0-9", "x-z", "A-Z", "\u{e0}-\u{e9}"])),
                    2 => out.push_str(r.pick(&["[:alpha:]", "[:digit:]", "[:^space:]"])),
                    // No bare `]` or `-`: they compose into the nested-class and
                    // set-operation syntax this engine refuses, tested elsewhere.
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
/// uses (see `refusals_are_refusals` in `differential.rs`).
const REFUSALS: &[&str] = &[
    "Unicode property classes",
    "set operations",
    "nested character classes",
    "byte-oriented",
    "CRLF mode",
];

/// The digest of every verdict and `find_iter` sequence at the default seed
/// and size.
const PINNED: &str = "e3f0529699f47d5f";

#[test]
fn generated_patterns_are_pinned() {
    let seed_env: Option<u64> = std::env::var("FUZZ_SEED").ok().and_then(|v| v.parse().ok());
    let n_env: Option<u32> = std::env::var("FUZZ_N").ok().and_then(|v| v.parse().ok());
    let seed = seed_env.unwrap_or(0x2026_0822);
    let patterns = n_env.unwrap_or(8_000);

    let mut r = Lcg(seed);
    let mut cases = 0u32;
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes.iter().chain(&[0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };

    for _ in 0..patterns {
        let mut pattern = String::new();
        gen_alt(&mut r, &mut pattern, 0);
        let haystacks: Vec<String> = (0..5).map(|_| gen_haystack(&mut r)).collect();
        feed(pattern.as_bytes());

        let re = match aterm_regex::Regex::new(&pattern) {
            Ok(re) => re,
            Err(e) => {
                let msg = e.to_string();
                if REFUSALS.iter().any(|r| msg.contains(r)) {
                    feed(b"REFUSED");
                } else {
                    feed(b"ERR");
                }
                continue;
            }
        };

        for haystack in &haystacks {
            cases += 1;
            let spans: Vec<(usize, usize)> = re
                .find_iter(haystack)
                .map(|m| (m.start(), m.end()))
                .collect();
            assert_eq!(
                re.is_match(haystack),
                !spans.is_empty(),
                "is_match vs find_iter for {pattern:?} on {haystack:?}"
            );
            assert_eq!(
                re.find(haystack).map(|m| (m.start(), m.end())),
                spans.first().copied(),
                "find vs find_iter for {pattern:?} on {haystack:?}"
            );
            for &(start, end) in &spans {
                assert!(
                    haystack.is_char_boundary(start) && haystack.is_char_boundary(end),
                    "{pattern:?} on {haystack:?} produced an offset off a code-point boundary"
                );
                feed(&(start as u64).to_le_bytes());
                feed(&(end as u64).to_le_bytes());
            }
            feed(b"/");
        }
    }

    assert!(
        cases > 20_000 || n_env.is_some(),
        "the corpus shrank to {cases} cases"
    );
    if seed_env.is_none() && n_env.is_none() {
        assert_eq!(format!("{h:016x}"), PINNED, "the pinned corpus changed");
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A deterministic differential fuzz.
//!
//! No `rand`: the generator is a 64-bit LCG seeded from a constant, so the
//! corpus is identical on every machine and every run. A failure here can be
//! reproduced by its seed alone, and the test cannot go green because a random
//! draw happened to miss the bug.
//!
//! Three phases, each comparing against the `toml` oracle:
//!
//! 1. GENERATED DOCUMENTS — well-formed TOML assembled from every construct in
//!    the spec. Both implementations must accept and agree, and the document
//!    half must reproduce the text byte for byte.
//! 2. MUTATIONS — one byte of a generated document replaced with a
//!    syntactically interesting one. Accept/reject must agree, and where both
//!    accept the values must too.
//! 3. NOISE — strings drawn straight from the TOML punctuation alphabet, which
//!    is almost always garbage and is where a parser's error paths actually get
//!    exercised.
//!
//! The alphabet is NOT ASCII-only, and that is load-bearing. It used to be, and
//! the hole was exactly the shape of a real abort: `unicode_escape` sliced the
//! source at `pos + 4`, so a multi-byte character anywhere inside a `\u`
//! window panicked with "not a char boundary" — and no ASCII generator, and no
//! file in the repository corpus (`grep -rlI --include='*.toml' '\\u' .` finds
//! ZERO), could ever produce one. Phases 2 and 3 now mutate and generate whole
//! CHARACTERS, so the input stays valid UTF-8 without needing an ASCII
//! restriction, and every rejection is additionally checked for a span that
//! lands on character boundaries — the invariant `Error::span()` promises its
//! callers, and the one the panic above was a symptom of.

mod common;

use common::{oracle_saturated_a_float, values_agree};

/// A 64-bit linear congruential generator (Knuth's MMIX constants). Chosen over
/// `rand` for exactly one reason: a fixed seed here means a fixed corpus, on
/// every machine, forever.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound.max(1)
    }

    fn pick<'a, T>(&mut self, options: &'a [T]) -> &'a T {
        &options[self.below(options.len() as u32) as usize]
    }

    /// One CHARACTER out of an alphabet, so the alphabet can hold multi-byte
    /// ones and stay readable as a single literal.
    fn pick_char(&mut self, options: &str) -> char {
        let count = options.chars().count() as u32;
        options
            .chars()
            .nth(self.below(count) as usize)
            .expect("the index is drawn below the character count")
    }
}

const BARE_KEYS: &[&str] = &[
    "a",
    "b",
    "key",
    "font_px",
    "net",
    "listen",
    "x1",
    "Y-2",
    "_z",
    "0",
    "007",
    "long_key_name",
];

const QUOTED_KEYS: &[&str] = &[
    "\"quoted key\"",
    "'literal key'",
    "\"with.dot\"",
    "\"\"",
    "''",
    "\"ʞ\"",
];

const SCALARS: &[&str] = &[
    "1",
    "-17",
    "+99",
    "0",
    "1_000",
    "0xDEAD_beef",
    "0o755",
    "0b1010",
    "9223372036854775807",
    "-9223372036854775808",
    "1.0",
    "-0.01",
    "5e+22",
    "1e06",
    "-2E-2",
    "224_617.445_991_228",
    "inf",
    "-inf",
    "nan",
    "true",
    "false",
    "\"basic\"",
    "\"esc \\t \\n \\u00E9 \\\" \\\\\"",
    "'literal \\n not an escape'",
    "\"\"\"multi\nline\"\"\"",
    "'''raw\nmulti'''",
    "\"\"\"trail\\\n  ing\"\"\"",
    "1979-05-27T07:32:00Z",
    "1979-05-27T00:32:00-07:00",
    "1979-05-27T07:32:00.999999",
    "1979-05-27",
    "07:32:00",
    "00:32:00.999999",
    "[]",
    "[1, 2, 3]",
    "[ 'a', \"b\" ]",
    "[\n  1, # comment\n  2,\n]",
    "{}",
    "{ a = 1 }",
    "{ a.b = 2, c = [1] }",
    "[[1], [2, 3]]",
];

const WHITESPACE: &[&str] = &["", " ", "  ", "\t", " \t "];
const LINE_GAPS: &[&str] = &[
    "\n",
    "\n\n",
    "\n# a comment\n",
    "\n  # indented\n\n",
    "\n\t\n",
];

fn generate(rng: &mut Lcg) -> String {
    let mut out = String::new();
    let mut used_root: Vec<String> = Vec::new();
    let mut used_tables: Vec<String> = Vec::new();

    for _ in 0..rng.below(4) {
        emit_pair(rng, &mut out, &mut used_root);
    }

    for _ in 0..rng.below(5) {
        out.push_str(rng.pick(LINE_GAPS));
        let depth = 1 + rng.below(3);
        let mut name = String::new();
        for level in 0..depth {
            if level > 0 {
                name.push('.');
            }
            name.push_str(rng.pick(BARE_KEYS));
            name.push_str(&level.to_string());
        }
        let array = rng.below(4) == 0;
        // A header may only be written once, and an array-of-tables header may
        // only be repeated. Tracking that here keeps the generator inside the
        // spec instead of testing the duplicate-key path by accident.
        if !array && used_tables.contains(&name) {
            continue;
        }
        if !array {
            used_tables.push(name.clone());
        }
        out.push_str(rng.pick(WHITESPACE));
        out.push_str(if array { "[[" } else { "[" });
        out.push_str(rng.pick(WHITESPACE));
        out.push_str(&name);
        out.push_str(rng.pick(WHITESPACE));
        out.push_str(if array { "]]" } else { "]" });
        out.push_str(rng.pick(WHITESPACE));
        out.push('\n');

        let mut used_here: Vec<String> = Vec::new();
        for _ in 0..rng.below(4) {
            emit_pair(rng, &mut out, &mut used_here);
        }
    }

    out.push_str(rng.pick(LINE_GAPS));
    out
}

fn emit_pair(rng: &mut Lcg, out: &mut String, used: &mut Vec<String>) {
    let dotted = rng.below(5) == 0;
    let mut key = String::new();
    if rng.below(6) == 0 {
        key.push_str(rng.pick(QUOTED_KEYS));
    } else {
        key.push_str(rng.pick(BARE_KEYS));
    }
    if dotted {
        key.push_str(rng.pick(WHITESPACE));
        key.push('.');
        key.push_str(rng.pick(WHITESPACE));
        key.push_str(rng.pick(BARE_KEYS));
    }
    // Uniqueness is decided on the SPELLING here, which is coarser than the
    // spec's (two spellings can name one key). Dropping a collision is the
    // conservative side: the generator stays inside valid TOML.
    let identity = key.replace([' ', '\t'], "");
    if used.iter().any(|u| {
        u == &identity
            || u.starts_with(&format!("{identity}."))
            || identity.starts_with(&format!("{u}."))
    }) {
        return;
    }
    used.push(identity);

    out.push_str(rng.pick(WHITESPACE));
    out.push_str(&key);
    out.push_str(rng.pick(WHITESPACE));
    out.push('=');
    out.push_str(rng.pick(WHITESPACE));
    out.push_str(rng.pick(SCALARS));
    out.push_str(rng.pick(WHITESPACE));
    if rng.below(4) == 0 {
        out.push_str("# trailing");
    }
    out.push('\n');
}

#[test]
fn generated_documents_agree_with_the_oracle_and_round_trip() {
    let mut rng = Lcg::new(0x_A7E1_2026);
    let mut accepted = 0usize;
    for case in 0..2_000u32 {
        let source = generate(&mut rng);
        let ours = aterm_toml::from_str::<aterm_toml::Value>(&source);
        let theirs = toml::from_str::<toml::Value>(&source);
        match (&ours, &theirs) {
            (Ok(a), Ok(b)) => {
                values_agree(a, b).unwrap_or_else(|why| panic!("case {case}: {why}\n{source}"));
                let document: aterm_toml::edit::DocumentMut = source.parse().unwrap_or_else(|e| {
                    panic!("case {case}: document half rejected it: {e}\n{source}")
                });
                assert_eq!(
                    document.to_string(),
                    source,
                    "case {case}: round-trip differs\n{source}"
                );
                accepted += 1;
            }
            (Err(a), Ok(_)) => {
                panic!("case {case}: we reject what the oracle accepts: {a}\n{source}")
            }
            (Ok(_), Err(b)) => {
                panic!("case {case}: we accept what the oracle rejects: {b}\n{source}")
            }
            (Err(_), Err(_)) => {}
        }
    }
    assert!(
        accepted > 1_900,
        "the generator produced too little valid TOML: {accepted}/2000"
    );
}

/// The TOML punctuation alphabet, plus the characters a byte-oriented generator
/// can never reach: a 2-, a 3- and a 4-byte one, a BOM, and DEL. Each WIDTH
/// matters separately — a 2-byte character only splits a `\u` window that has
/// three digits before it, a 4-byte one splits a window with one.
const INTERESTING_CHARS: &str =
    "[]{}=.,'\"#\\\n\r \t0189abeExXoO_+-:zZTtuU\u{e9}\u{20ac}\u{1f600}\u{feff}\u{7f}";

/// `Error::span()` is public API, and `crates/aterm-gui` slices the source with
/// ranges derived from it. A span whose end sits inside a character is a panic
/// in the caller, so every rejection this file produces is held to it.
fn span_is_char_aligned(source: &str, error: &aterm_toml::Error) -> Result<(), String> {
    let Some(span) = error.span() else {
        return Ok(());
    };
    if !source.is_char_boundary(span.start) || !source.is_char_boundary(span.end) {
        return Err(format!(
            "span {span:?} splits a character in {source:?} ({error})"
        ));
    }
    Ok(())
}

#[test]
fn single_character_mutations_agree_with_the_oracle() {
    let mut rng = Lcg::new(0x_D15A_57E4);
    let mut divergences = Vec::new();
    for case in 0..4_000u32 {
        // CHARACTERS, not bytes. Substituting a whole character keeps the result
        // valid UTF-8 by construction, which is what lets the alphabet carry
        // multi-byte characters — the ASCII-only restriction this replaced is
        // why the `unicode_escape` char-boundary abort survived four fuzz
        // phases and a 12,113-file differential corpus.
        let mut source: Vec<char> = generate(&mut rng).chars().collect();
        if source.is_empty() {
            continue;
        }
        for _ in 0..1 + rng.below(3) {
            let at = rng.below(source.len() as u32) as usize;
            source[at] = rng.pick_char(INTERESTING_CHARS);
        }
        let text: String = source.into_iter().collect();
        let ours = aterm_toml::from_str::<aterm_toml::Value>(&text);
        let theirs = toml::from_str::<toml::Value>(&text);
        if let Err(e) = &ours
            && let Err(why) = span_is_char_aligned(&text, e)
        {
            divergences.push(format!("case {case}: {why}"));
        }
        match (&ours, &theirs) {
            (Ok(a), Ok(b)) => {
                if let Err(why) = values_agree(a, b) {
                    divergences.push(format!("case {case}: {why}\n{text:?}"));
                }
                let printed = text
                    .parse::<aterm_toml::edit::DocumentMut>()
                    .map(|d| d.to_string());
                assert_eq!(
                    printed.as_deref(),
                    Ok(text.as_str()),
                    "case {case}: round-trip differs\n{text:?}"
                );
            }
            (Err(_), Ok(b)) if oracle_saturated_a_float(b, &text) => {}
            (Err(_), Ok(_)) => {
                divergences.push(format!("case {case}: we reject, oracle accepts\n{text:?}"))
            }
            (Ok(_), Err(_)) => {
                divergences.push(format!("case {case}: we accept, oracle rejects\n{text:?}"))
            }
            (Err(_), Err(_)) => {}
        }
    }
    assert!(
        divergences.is_empty(),
        "{} mutations diverged:\n{}",
        divergences.len(),
        divergences
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn punctuation_noise_never_panics_and_agrees_with_the_oracle() {
    let mut rng = Lcg::new(0x_6E01_5E00);
    let mut divergences = Vec::new();
    let mut accepted = 0usize;
    for case in 0..20_000u32 {
        let len = rng.below(40);
        let text: String = (0..len).map(|_| rng.pick_char(INTERESTING_CHARS)).collect();
        let ours = aterm_toml::from_str::<aterm_toml::Value>(&text);
        let theirs = toml::from_str::<toml::Value>(&text);
        if let Err(e) = &ours
            && let Err(why) = span_is_char_aligned(&text, e)
        {
            divergences.push(format!("case {case}: {why}"));
        }
        match (&ours, &theirs) {
            (Ok(a), Ok(b)) => {
                accepted += 1;
                if let Err(why) = values_agree(a, b) {
                    divergences.push(format!("case {case}: {why}\n{text:?}"));
                }
            }
            (Err(_), Ok(b)) if oracle_saturated_a_float(b, &text) => {}
            (Err(_), Ok(_)) => {
                divergences.push(format!("case {case}: we reject, oracle accepts\n{text:?}"))
            }
            (Ok(_), Err(_)) => {
                divergences.push(format!("case {case}: we accept, oracle rejects\n{text:?}"))
            }
            (Err(_), Err(_)) => {}
        }
    }
    assert!(
        divergences.is_empty(),
        "{} noise inputs diverged:\n{}",
        divergences.len(),
        divergences
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    eprintln!("noise: 20000 inputs, {accepted} of them accidentally valid TOML");
}

/// The abort the ASCII-only alphabet could not reach, pinned as a matrix.
///
/// A `\u`/`\U` escape has a FIXED-WIDTH digit window, so the parser has to
/// look at `pos + 4` (or `pos + 8`) whatever is actually there. Every
/// combination of "how many hex digits before the intruder" and "how wide the
/// intruder is" lands that offset at a different place inside the character —
/// which is why one repro is not enough, and why the 3x12 grid below is spelled
/// out rather than sampled. Before the fix these ABORTED, from every public
/// entry point, on 13 bytes of untrusted config.
#[test]
fn a_multi_byte_character_inside_a_unicode_escape_is_an_error_not_an_abort() {
    let mut checked = 0usize;
    for (marker, digits) in [('u', 4usize), ('U', 8usize)] {
        for lead in 0..digits {
            for intruder in ['\u{e9}', '\u{20ac}', '\u{1f600}'] {
                let mut escape = String::from("\\");
                escape.push(marker);
                escape.extend(core::iter::repeat_n('0', lead));
                escape.push(intruder);
                // Enough trailing digits that the window is never short: the
                // length guard must not be what saves us here.
                escape.push_str("00000041");

                let document = format!("x = \"{escape}\"\n");
                let ours = aterm_toml::from_str::<aterm_toml::Value>(&document);
                let error = ours.expect_err(&format!("{document:?} must be rejected"));
                span_is_char_aligned(&document, &error).expect("boundary-safe span");
                assert!(
                    toml::from_str::<toml::Value>(&document).is_err(),
                    "the oracle accepts {document:?}"
                );

                // The same bytes through every other public entry point: the
                // panic was reachable from all of them, so the fix is proved on
                // all of them.
                assert!(document.parse::<aterm_toml::edit::DocumentMut>().is_err());
                assert!(document.parse::<aterm_toml::Table>().is_err());
                let bare = format!("\"{escape}\"");
                assert!(bare.parse::<aterm_toml::Value>().is_err());
                assert!(bare.parse::<aterm_toml::edit::Value>().is_err());
                assert!(aterm_toml::edit::Key::parse(&bare).is_err());

                checked += 1;
            }
        }
    }
    assert_eq!(
        checked, 36,
        "the matrix is 2 widths x 12 offsets x 3 intruders"
    );
}

/// The other half of the same edit: `u32::from_str_radix` accepted a leading
/// `+`, so these decoded to `A` instead of failing. TOML 1.0 says `4HEXDIG`.
#[test]
fn a_signed_unicode_escape_is_not_four_hex_digits() {
    for document in [
        "x = \"\\u+041\"\n",
        "x = \"\\U+0000041\"\n",
        "x = \"\\u-041\"\n",
        "x = \"\\u 041\"\n",
    ] {
        let error = aterm_toml::from_str::<aterm_toml::Value>(document)
            .err()
            .unwrap_or_else(|| panic!("{document:?} must be rejected"));
        span_is_char_aligned(document, &error).expect("boundary-safe span");
        assert!(
            toml::from_str::<toml::Value>(document).is_err(),
            "the oracle accepts {document:?}"
        );
    }
}

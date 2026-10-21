// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A deterministic fuzz: a 64-bit LCG with fixed seeds instead of `rand`, so the corpus is
//! the same on every machine and run, a failure reproduces from its seed, and a lucky draw
//! cannot turn the test green.
//!
//! 1. Generated documents: well-formed TOML from every construct in the spec. The value
//!    half must accept, and the document half reproduce the text byte for byte.
//! 2. Mutations: one to three characters of a generated document replaced with
//!    interesting ones. Where the value half accepts, the document half must reproduce the
//!    text.
//! 3. Noise: strings drawn from the punctuation alphabet, which exercise the error paths.
//!
//! The alphabet holds multi-byte characters, which can land inside a fixed-width window (a
//! `\u` escape). Phases 2 and 3 work on whole characters so the input stays valid UTF-8,
//! and every rejection's span must sit on character boundaries.

/// A 64-bit linear congruential generator (Knuth's MMIX constants). Chosen over
/// `rand` for exactly one reason: a fixed seed here means a fixed corpus, on
/// every machine, forever. `typed`'s round-trip properties draw from it too.
pub(crate) struct Lcg(u64);

impl Lcg {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub(crate) fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    pub(crate) fn below(&mut self, bound: u32) -> u32 {
        self.next() % bound.max(1)
    }

    fn pick<T: Copy>(&mut self, options: &[T]) -> T {
        options[self.below(options.len() as u32) as usize]
    }

    /// One character out of an alphabet that may hold multi-byte ones.
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
    // Uniqueness is decided on the spelling, which is coarser than the spec's (two
    // spellings can name one key); dropping a collision keeps the generator inside
    // valid TOML.
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
fn generated_documents_parse_and_round_trip() {
    let mut rng = Lcg::new(0x_A7E1_2026);
    let mut accepted = 0usize;
    for case in 0..2_000u32 {
        let source = generate(&mut rng);
        match aterm_toml::from_str::<aterm_toml::Value>(&source) {
            Ok(_) => {
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
            Err(e) => {
                span_is_char_aligned(&source, &e).unwrap_or_else(|why| panic!("case {case}: {why}"))
            }
        }
    }
    assert!(
        accepted > 1_900,
        "the generator produced too little valid TOML: {accepted}/2000"
    );
}

/// The TOML punctuation alphabet, plus a 2-, a 3- and a 4-byte character, a BOM, and DEL.
/// Each width matters: a 2-byte character only splits a `\u` window with three digits
/// before it, a 4-byte one a window with one.
const INTERESTING_CHARS: &str =
    "[]{}=.,'\"#\\\n\r \t0189abeExXoO_+-:zZTtuU\u{e9}\u{20ac}\u{1f600}\u{feff}\u{7f}";

/// `crates/aterm-gui` slices the source with ranges from the public `Error::span()`, so a
/// span ending inside a character panics there; every rejection here is held to it.
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
fn single_character_mutations_round_trip_or_refuse_cleanly() {
    let mut rng = Lcg::new(0x_D15A_57E4);
    let mut divergences = Vec::new();
    for case in 0..4_000u32 {
        // Whole characters, not bytes, so the result stays valid UTF-8 with a multi-byte
        // alphabet.
        let mut source: Vec<char> = generate(&mut rng).chars().collect();
        if source.is_empty() {
            continue;
        }
        for _ in 0..1 + rng.below(3) {
            let at = rng.below(source.len() as u32) as usize;
            source[at] = rng.pick_char(INTERESTING_CHARS);
        }
        let text: String = source.into_iter().collect();
        match aterm_toml::from_str::<aterm_toml::Value>(&text) {
            Err(e) => {
                if let Err(why) = span_is_char_aligned(&text, &e) {
                    divergences.push(format!("case {case}: {why}"));
                }
            }
            Ok(_) => {
                let printed = text
                    .parse::<aterm_toml::edit::DocumentMut>()
                    .map(|d| d.to_string());
                assert_eq!(
                    printed.as_deref(),
                    Ok(text.as_str()),
                    "case {case}: round-trip differs\n{text:?}"
                );
            }
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
fn punctuation_noise_never_panics() {
    let mut rng = Lcg::new(0x_6E01_5E00);
    let mut divergences = Vec::new();
    let mut accepted = 0usize;
    for case in 0..20_000u32 {
        let len = rng.below(40);
        let text: String = (0..len).map(|_| rng.pick_char(INTERESTING_CHARS)).collect();
        match aterm_toml::from_str::<aterm_toml::Value>(&text) {
            Ok(_) => accepted += 1,
            Err(e) => {
                if let Err(why) = span_is_char_aligned(&text, &e) {
                    divergences.push(format!("case {case}: {why}"));
                }
            }
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

/// A multi-byte character inside a `\u`/`\U` escape's fixed-width digit window. Each count
/// of hex digits before the intruder and each intruder width lands `pos + 4` (or `pos + 8`)
/// at a different place inside the character, so the grid is spelled out, not sampled.
#[test]
fn a_multi_byte_character_inside_a_unicode_escape_is_an_error_not_an_abort() {
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

                // The same bytes through every other public entry point.
                assert!(document.parse::<aterm_toml::edit::DocumentMut>().is_err());
                assert!(document.parse::<aterm_toml::Table>().is_err());
                let bare = format!("\"{escape}\"");
                assert!(bare.parse::<aterm_toml::Value>().is_err());
                assert!(bare.parse::<aterm_toml::edit::Value>().is_err());
                assert!(aterm_toml::edit::Key::parse(&bare).is_err());
            }
        }
    }
}

/// `u32::from_str_radix` accepts a leading `+`; TOML 1.0's `4HEXDIG` does not.
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
    }
}

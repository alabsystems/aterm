// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential tests against the `regex` crate, kept as a dev-dependency
//! ORACLE for exactly this purpose.
//!
//! The contract this crate has to meet is not "close enough". It is: for a
//! pattern both engines compile, `find_iter` yields the identical sequence of
//! `(start, end)` pairs and `is_match` returns the identical bool. Everything
//! here is that assertion over a corpus — the patterns aterm ships, a
//! hand-written syntax corpus, and the perl classes across all of Unicode.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// The eleven patterns `aterm-selection`'s `builtin_patterns` ships. These are
/// the specification: whatever else this engine can or cannot do, it has to do
/// these, because they are compiled on every terminal that opens.
const BUILTIN: &[(&str, &str)] = &[
    (
        "url",
        r#"(?i)(?:https?|ftp|file)://[^\s<>\[\](){}'"`,;|\\^]+[^\s<>\[\](){}'"`,;|\\^.!?:]"#,
    ),
    (
        "file_path",
        r"(?:/(?:[a-zA-Z0-9._-]+/)*[a-zA-Z0-9._-]+|\.{1,2}/(?:[a-zA-Z0-9._-]+/)*[a-zA-Z0-9._-]+|[A-Za-z]:[/\\](?:[a-zA-Z0-9._-]+[/\\])*[a-zA-Z0-9._-]+)",
    ),
    (
        "email",
        r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?)*\.[a-zA-Z]{2,}",
    ),
    (
        "ipv4",
        r"(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)(?::\d{1,5})?",
    ),
    (
        "ipv6",
        r"\[?(?:(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,7}:|(?:[0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|::(?:[0-9a-fA-F]{1,4}:){0,5}[0-9a-fA-F]{1,4}|::)\]?(?::\d{1,5})?",
    ),
    ("git_hash", r"\b[0-9a-fA-F]{7,40}\b"),
    ("double_quoted", r#""(?:[^"\\]|\\.)*""#),
    ("single_quoted", r"'(?:[^'\\]|\\.)*'"),
    ("backtick_quoted", r"`(?:[^`\\]|\\.)*`"),
    (
        "uuid",
        r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
    ),
    (
        "semver",
        r"v?\d+\.\d+\.\d+(?:-[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?(?:\+[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?",
    ),
];

/// Terminal lines of the kind selection actually runs over.
const HAYSTACKS: &[&str] = &[
    "see https://example.com/a/b?c=d#e for details.",
    "ftp://user.host:21/pub/file.tar.gz, and file:///Users//example/aterm/Cargo.toml",
    "HTTPS://EXAMPLE.COM/Path?q=1 (an uppercase scheme)",
    "curl -sSL 'https://raw.example.org/x.sh' | sh   # quoted URL",
    "wrapped <https://example.com/x> and [https://example.com/y] and (https://z.io)",
    "/usr/local/bin/aterm ./rel/path ../up/one C:\\Users\\a\\b.txt .\\rel\\x",
    "error at crates/aterm-regex/src/pikevm.rs:118:9",
    "mail trex.m21@proton.me or a+b%c@sub.domain.co.uk!",
    "not-an-email@localhost and @nope and a@b.c",
    "127.0.0.1:8080 255.255.255.255 256.1.1.1 10.0.0.1 1.2.3.4:65535",
    "[::1]:8080 fe80::1 2001:0db8:85a3:0000:0000:8a2e:0370:7334 :: ::ffff:1",
    "commit 66390b5c8f2a1b3c4d5e6f7a8b9c0d1e2f3a4b5c reverts efca01fc",
    "deadbeef cafebabe 0123456 abcdefg 1234567890abcdef1234567890abcdef12345678",
    "\"quoted \\\" str\" and 'single \\' one' and `tick` and \"\"",
    "550e8400-e29b-41d4-a716-446655440000 and 550e8400e29b41d4a716446655440000",
    "v1.2.3 2.0.0-rc.1+build.7 0.47.0 1.2 1.2.3.4",
    "caf\u{e9} na\u{ef}ve e\u{301}accent \u{4f60}\u{597d} \u{1F600} 42",
    "\u{4f60}\u{597d}/a/b \u{e9}mail@ex\u{e9}mple.com 1\u{661}2.3.4.5",
    "",
    " ",
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    // NEWLINE-BEARING. The corpus carried `(?m)^a`, `(?m)a$`, `(?m)^` and
    // `(?m)$` from the start and not one haystack above holds a `\n`, so on
    // every one of them `(?m)^` is indistinguishable from `^` and the multiline
    // half of the corpus was agreeing about nothing. These are what tell
    // `StartText` and `StartLine` apart — and what a scrollback export, a
    // pasted diff or a bracketed-paste payload actually looks like.
    "a\nb\nc",
    "ERROR one\nERROR two\nok three",
    "\nleading",
    "trailing\n",
    "\n",
    "\n\n",
    "one\r\ntwo\r\n",
    "  indented\n\tTabbed\n",
];

/// A pattern's `find_iter` spans, or `None` when it does not compile.
fn spans_mine(p: &str, h: &str) -> Option<Vec<(usize, usize)>> {
    let re = aterm_regex::Regex::new(p).ok()?;
    Some(re.find_iter(h).map(|m| (m.start(), m.end())).collect())
}

fn spans_oracle(p: &str, h: &str) -> Option<Vec<(usize, usize)>> {
    let re = regex::Regex::new(p).ok()?;
    Some(re.find_iter(h).map(|m| (m.start(), m.end())).collect())
}

/// Assert both engines compile `p` and agree on `h`, for `find_iter` *and*
/// `is_match`.
#[track_caller]
fn agree(p: &str, h: &str) {
    let mine =
        aterm_regex::Regex::new(p).unwrap_or_else(|e| panic!("this engine rejected {p:?}:\n{e}"));
    let oracle = regex::Regex::new(p).unwrap_or_else(|e| panic!("the oracle rejected {p:?}: {e}"));
    let a: Vec<(usize, usize)> = mine.find_iter(h).map(|m| (m.start(), m.end())).collect();
    let b: Vec<(usize, usize)> = oracle.find_iter(h).map(|m| (m.start(), m.end())).collect();
    assert_eq!(a, b, "find_iter disagreed for {p:?} on {h:?}");
    assert_eq!(
        mine.is_match(h),
        oracle.is_match(h),
        "is_match disagreed for {p:?} on {h:?}"
    );
    assert_eq!(
        mine.find(h).map(|m| (m.start(), m.end())),
        oracle.find(h).map(|m| (m.start(), m.end())),
        "find disagreed for {p:?} on {h:?}"
    );
    // Every offset must land on a code-point boundary, or the call sites'
    // `&text[m.start()..m.end()]` would panic.
    for (start, end) in a {
        assert!(
            h.is_char_boundary(start) && h.is_char_boundary(end),
            "{p:?} on {h:?}"
        );
    }
}

#[test]
fn builtin_selection_patterns_match_the_oracle() {
    for &(name, pattern) in BUILTIN {
        for haystack in HAYSTACKS {
            // Name the rule in the panic so a failure says which one broke.
            let mine = spans_mine(pattern, haystack);
            let oracle = spans_oracle(pattern, haystack);
            assert_eq!(mine, oracle, "builtin rule {name:?} on {haystack:?}");
            assert!(mine.is_some(), "builtin rule {name:?} must compile");
        }
    }
}

/// The specific spans the built-in rules are supposed to pick out, spelled out
/// rather than only cross-checked, so a change of behaviour is legible.
#[test]
fn builtin_patterns_select_what_they_are_for() {
    let find = |p: &str, h: &str| {
        aterm_regex::Regex::new(p)
            .expect("compiles")
            .find(h)
            .map(|m| m.as_str().to_string())
    };
    let by_name = |n: &str| {
        BUILTIN
            .iter()
            .find(|&&(k, _)| k == n)
            .expect("known rule")
            .1
    };

    assert_eq!(
        find(
            by_name("url"),
            "open https://example.com/a?b=c#d, then stop"
        ),
        Some("https://example.com/a?b=c#d".to_string())
    );
    assert_eq!(
        find(
            by_name("file_path"),
            "at ../crates/aterm-regex/src/lib.rs line 3"
        ),
        Some("../crates/aterm-regex/src/lib.rs".to_string())
    );
    assert_eq!(
        find(by_name("file_path"), "open C:\\Users\\dev\\notes.txt now"),
        Some("C:\\Users\\dev\\notes.txt".to_string())
    );
    assert_eq!(
        find(by_name("email"), "write to trex.m21@proton.me today"),
        Some("trex.m21@proton.me".to_string())
    );
    assert_eq!(
        find(by_name("ipv4"), "bound 127.0.0.1:8080 ok"),
        Some("127.0.0.1:8080".to_string())
    );
    assert_eq!(
        find(
            by_name("ipv6"),
            "peer 2001:0db8:85a3:0000:0000:8a2e:0370:7334 up"
        ),
        Some("2001:0db8:85a3:0000:0000:8a2e:0370:7334".to_string())
    );
    assert_eq!(
        find(by_name("git_hash"), "revert 66390b5c8f please"),
        Some("66390b5c8f".to_string())
    );
    assert_eq!(
        find(by_name("uuid"), "id 550e8400-e29b-41d4-a716-446655440000."),
        Some("550e8400-e29b-41d4-a716-446655440000".to_string())
    );
    assert_eq!(
        find(by_name("semver"), "aterm v0.47.0 shipped"),
        Some("v0.47.0".to_string())
    );
    assert_eq!(
        find(by_name("double_quoted"), "say \"hi \\\" there\" ok"),
        Some("\"hi \\\" there\"".to_string())
    );
}

#[test]
fn syntax_corpus_matches_the_oracle() {
    const PATTERNS: &[&str] = &[
        // Quantifiers, greedy and lazy.
        r"a",
        r"a*",
        r"a+",
        r"a?",
        r"a*?",
        r"a+?",
        r"a??",
        r"a{2}",
        r"a{2,}",
        r"a{0,3}",
        r"a{2,}?",
        r"a{0,3}?",
        r"a{0}",
        r"a**",
        r"a{2}{3}",
        r"(?U)a*",
        r"(?U)a*?",
        // Alternation, including empty branches.
        r"a|b",
        r"(a|b)c",
        r"(?:a|b)*",
        r"a|",
        r"|a",
        r"a||b",
        r"a|ab",
        r"(a|ab)c",
        r"(|a)",
        r"(a|)",
        r"(|a)*",
        r"(a|)*",
        r"(|a)+",
        r"(a*)*",
        r"(a*)+",
        r"(?:)",
        r"()",
        // Assertions.
        r"^a",
        r"a$",
        r"^a$",
        r"^",
        r"$",
        r"^$",
        r"\ba\b",
        r"\Ba\B",
        r"\b",
        r"\B",
        r"\Aa",
        r"a\z",
        r"\<a",
        r"a\>",
        r"\bx*\b",
        r"\b|a",
        r"(?m)^a",
        r"(?m)a$",
        r"(?m)^",
        r"(?m)$",
        // START-ANCHOR ADVERSARIES. `Program::start_anchored` claims a pattern
        // can only match at offset 0, and the search then declines to walk the
        // rest of the haystack. Every one of these CONTAINS a `^` and is
        // nonetheless NOT anchored — an alternation with a bare branch, a loop
        // that can exit without entering, an optional group — so a walk that
        // merely spotted the `^` and stopped would report a missed match here
        // rather than a slow one.
        r"a|^b",
        r"^a|b",
        r"(^a)*",
        r"(^a)+",
        r"(?:^a)?b",
        r"(^|x)y",
        r"^a|^b",
        r"\A\Aa",
        r"^a*",
        r"(?:^)|a",
        r"(?m:^)a",
        r"(?:(?m)^)a",
        // Classes.
        r"[abc]",
        r"[^abc]",
        r"[a-z]",
        r"[^a-z]",
        r"[a-zA-Z0-9._-]",
        r"[]a]",
        r"[a-]",
        r"[^]a]",
        r"[\d]",
        r"[\D]",
        r"[\w\s]",
        r"[^\w]",
        r"[^\D]",
        r"[\x41-\x43]",
        r"[[:alpha:]]",
        r"[[:^digit:]]",
        r"[[:punct:]]",
        r"[[:space:][:upper:]]",
        r"[\\]",
        r"[\^]",
        r"[a^]",
        r"[.!?]",
        r"[^\s<>]",
        r"[\u{e0}-\u{ff}]",
        // Perl classes and dot.
        r"\d+",
        r"\w+",
        r"\s+",
        r"\W",
        r"\S",
        r"\D",
        r".",
        r".*",
        r"(?s).",
        r"(?s).*",
        // Flags.
        r"(?i)abc",
        r"(?i)[a-z]",
        r"(?i)[^k]",
        r"(?i)k",
        r"(?i)s",
        r"a(?i)b",
        r"(a(?i)b)c",
        r"(?i:a)b",
        r"(?i-s:a)",
        r"(?x) a  b ",
        r"(?x)a#c",
        r"(?u)a",
        // Escapes.
        r"\.",
        r"\\",
        r"\-",
        r"\+",
        r"\t",
        r"\n",
        r"\x41",
        r"\x{1F600}",
        r"\u{4f60}",
        r"\U{41}",
        r"e\u{301}",
        r"(?i)\u{3c3}",
        r"(?i)stra\u{df}e",
        // Groups.
        r"(?<name>a)b",
        r"(?P<other>a)b",
        r"((a))",
        r"(a)(b)",
        // Shapes that stress the simulation.
        r"(a+)+b",
        r"(a|a)*b",
        r"(a*)*b",
        r"\bfoo\b|\bbar\b",
        r"^(?:a|ab)+$",
        r"(?:ab|a)(?:c|bc)",
        r"x*y*z*",
        r"(?:a?){4}a{4}",
    ];
    const HAY: &[&str] = &[
        "",
        "a",
        "aa",
        "aaa",
        "ab",
        "abc",
        "abcabc",
        "a b c",
        "  ",
        "\n",
        "a\nb",
        "\r\n",
        "aab",
        "ba",
        "Hello World",
        "K\u{212a}k",
        "\u{3c3}\u{3c2}\u{3a3}",
        "\u{df}",
        "e\u{301}x",
        "\u{4f60}\u{597d}",
        "42 and 0x2A",
        "_foo_bar",
        "a-b-c",
        "foo.bar",
        "foo bar baz",
        "A",
        "z",
        "\u{661}\u{662}",
        "!?.",
        "\u{1F600}!",
        "stra\u{df}e",
        "aaaaaaaaaaaaaaaaaaaaab",
        "xxxxx",
        "abababab",
        "<a> [b] (c)",
        "\u{e0}\u{ff}",
    ];
    for p in PATTERNS {
        for h in HAY {
            agree(p, h);
        }
    }
}

/// `\d`, `\w` and `\s` over every code point.
///
/// `\s` and `\d` must agree exactly. `\w` is allowed to disagree in one
/// direction only — code points the *toolchain's* `char::is_alphabetic` knows
/// about and the oracle's older bundled tables do not. Asserting that shape is
/// what keeps this a recorded version skew rather than a place a real bug could
/// hide.
#[test]
fn perl_classes_match_the_oracle() {
    let mine_d = aterm_regex::Regex::new(r"^\d$").expect("compiles");
    let mine_w = aterm_regex::Regex::new(r"^\w$").expect("compiles");
    let mine_s = aterm_regex::Regex::new(r"^\s$").expect("compiles");
    let their_d = regex::Regex::new(r"^\d$").expect("compiles");
    let their_w = regex::Regex::new(r"^\w$").expect("compiles");
    let their_s = regex::Regex::new(r"^\s$").expect("compiles");

    let mut buf = [0u8; 4];
    let mut word_skew = 0usize;
    for cp in 0u32..0x11_0000 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let s: &str = c.encode_utf8(&mut buf);
        assert_eq!(mine_d.is_match(s), their_d.is_match(s), "\\d at U+{cp:04X}");
        assert_eq!(mine_s.is_match(s), their_s.is_match(s), "\\s at U+{cp:04X}");
        if mine_w.is_match(s) != their_w.is_match(s) {
            assert!(
                mine_w.is_match(s) && c.is_alphabetic(),
                "\\w disagreed at U+{cp:04X} in a direction that is not Unicode-version skew"
            );
            word_skew += 1;
        }
    }
    // Non-zero and bounded: the skew is real, known, and small next to the
    // ~140k code points `\w` covers.
    assert!(
        word_skew < 10_000,
        "unexpectedly large \\w skew: {word_skew}"
    );
}

/// The `\w` skew reaches `\b`, and therefore a rule aterm actually ships.
///
/// `perl_classes_match_the_oracle` bounds the `\w` divergence but stops there,
/// which understates it: `\b` is *defined* from `\w`, so a code point the
/// toolchain calls alphabetic and the oracle does not is a code point where the
/// two engines draw the word boundary in different places. That is not an
/// abstract difference — `aterm-selection`'s `git_hash` rule is `\b`-delimited,
/// so a hex run touching one of those 4,662 code points was selectable under the
/// retired crate and is not under this one.
///
/// This test exists to make that consequence *visible and pinned* rather than
/// discovered later from a bug report. It asserts the shape, not the verdict:
/// the divergence is one-directional (the oracle selects, we do not, never the
/// reverse), it is confined to `\b`-delimited rules, and the rules without `\b`
/// are untouched. If a future toolchain or a baked `Alphabetic` table changes
/// any of that, this fails and says so.
#[test]
fn the_word_class_skew_moves_word_boundaries_in_a_shipped_rule() {
    let git_hash = BUILTIN
        .iter()
        .find(|(name, _)| *name == "git_hash")
        .expect("the git_hash rule is part of the specification")
        .1;
    assert!(
        git_hash.contains(r"\b"),
        "this test is about the \\b delimiters"
    );

    let mine = aterm_regex::Regex::new(git_hash).expect("compiles");
    let oracle = regex::Regex::new(git_hash).expect("compiles");

    // U+088F: `Alphabetic` to the current toolchain, not to the oracle's
    // bundled tables. Adjacent to a hex run it removes the trailing boundary.
    //
    // The concrete spans are asserted only while the two engines still disagree
    // about U+088F being a word character, and a live probe decides that rather
    // than a hard-coded expectation. Pinning `theirs == [(0, 8)]` outright would
    // make a newer oracle — one whose tables have caught up — fail this test
    // with a message about `\b` when the real news is that the divergence
    // closed. That is the same trap
    // `this_engine_finds_the_leftmost_start_whichever_oracle_vintage_resolves`
    // was renamed and rewritten out of — it used to be called
    // `the_oracle_misses_a_leftmost_start_and_this_engine_does_not` and to
    // `assert_ne!` that the oracle stayed wrong — and it is not worth
    // re-digging one test over.
    let mut buf = [0u8; 4];
    let sep = char::from_u32(0x088F).expect("scalar");
    let probe_mine = aterm_regex::Regex::new(r"^\w$").expect("compiles");
    let probe_oracle = regex::Regex::new(r"^\w$").expect("compiles");
    let sep_str: &str = sep.encode_utf8(&mut buf);
    if probe_mine.is_match(sep_str) && !probe_oracle.is_match(sep_str) {
        assert!(
            sep.is_alphabetic(),
            "the divergence rests on `std` calling U+088F alphabetic"
        );
        let haystack = format!("deadbeef{sep}");
        let ours: Vec<(usize, usize)> = mine
            .find_iter(&haystack)
            .map(|m| (m.start(), m.end()))
            .collect();
        let theirs: Vec<(usize, usize)> = oracle
            .find_iter(&haystack)
            .map(|m| (m.start(), m.end()))
            .collect();
        assert_eq!(
            theirs,
            vec![(0, 8)],
            "the retired crate selected the hash here"
        );
        assert_eq!(
            ours,
            Vec::new(),
            "and this engine does not, because U+088F is a \\w"
        );
    }

    // The direction is fixed: the skew only ever *removes* a selection, because
    // `\w` here is a superset. A rule matching where the oracle does not would
    // be a different bug wearing this one's clothes.
    let mut suppressed = 0usize;
    for cp in 0u32..0x11_0000 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        if !c.is_alphabetic() || c.is_ascii() {
            continue;
        }
        let s: &str = c.encode_utf8(&mut buf);
        let haystack = format!("deadbeef{s}");
        let ours = mine.find(&haystack).is_some();
        let theirs = oracle.find(&haystack).is_some();
        if ours != theirs {
            assert!(
                theirs && !ours,
                "U+{cp:04X}: this engine selected a git hash the oracle did not"
            );
            suppressed += 1;
        }
    }
    // If this ever reaches zero the divergence has closed — which is good news,
    // and news the crate docs are then WRONG about. The failure says so rather
    // than demanding the divergence survive.
    assert!(
        suppressed > 0,
        "no `\\b` boundary skew left: the oracle's Unicode has caught up with the \
         toolchain's, so the divergence note in lib.rs is now false and this test \
         has nothing to pin — update both together"
    );
    assert!(
        suppressed < 10_000,
        "unexpectedly large boundary skew: {suppressed}"
    );

    // The rules with no `\b` are untouched, which is what confines the blast
    // radius to word-boundary rules rather than to selection generally.
    for name in ["url", "email", "semver"] {
        let pattern = BUILTIN
            .iter()
            .find(|(n, _)| *n == name)
            .expect("built-in")
            .1;
        assert!(!pattern.contains(r"\b"), "{name} is not \\b-delimited");
    }
}

/// Case folding, over the whole cased range, one code point at a time.
///
/// Two things went wrong in the first version of this test and both are fixed
/// here, because between them they made an entire class of fold bug invisible.
///
/// **The candidate window was arithmetic.** It asked both engines about
/// everything within `±0x40` of the code point. A fold orbit is not a
/// neighbourhood: `\u{390}` folds to `\u{1fd3}`, 0x1c43 away, and no window
/// reaches that. Candidates are now *bucketed by case mapping* — every code
/// point whose `to_lowercase`/`to_uppercase` starts with the same code point as
/// this one's, in both directions — which is what actually finds a distant
/// orbit partner. `\u{390}` and `\u{1fd3}` both uppercase to a string starting
/// `\u{399}`, so each is in the other's bucket; `\u{fb05}` and `\u{fb06}` both
/// uppercase to `"ST"`.
///
/// **The skip discarded exactly the population that was broken.** It skipped
/// any code point whose whole `to_uppercase()` *string* the oracle's single-char
/// `(?i)` pattern did not match — which is every code point with a multi-code-
/// point full case mapping, i.e. precisely `\u{390}`, `\u{3b0}` and `\u{fb05}`.
/// There is no skip now. Every disagreement is *classified* instead, the way
/// `perl_classes_match_the_oracle` classifies `\w`: a disagreement is tolerated
/// only when this engine matches, the oracle does not, and `std`'s own
/// single-code-point case mappings link the pair — the recorded
/// Unicode-version skew. A disagreement the other way, where the *oracle*
/// folds two code points together and this engine does not, is a hard failure
/// with no escape hatch. That is the direction the three missing
/// `CaseFolding.txt` `C` edges failed in.
#[test]
fn case_folding_matches_the_oracle() {
    fn single_lower(c: char) -> Option<char> {
        let mut it = c.to_lowercase();
        let first = it.next()?;
        it.next().is_none().then_some(first)
    }
    fn single_upper(c: char) -> Option<char> {
        let mut it = c.to_uppercase();
        let first = it.next()?;
        it.next().is_none().then_some(first)
    }

    // `std`'s own view of which code points are case-equivalent: the connected
    // components of the single-code-point mapping graph. A disagreement is only
    // excusable as version skew if `std` links the pair and the oracle's older
    // tables do not, so this is the arbiter of "skew" rather than our say-so.
    let mut parent: BTreeMap<char, char> = BTreeMap::new();
    fn root(parent: &mut BTreeMap<char, char>, x: char) -> char {
        let mut r = x;
        while let Some(&p) = parent.get(&r) {
            if p == r {
                break;
            }
            r = p;
        }
        r
    }

    // Buckets keyed by the FIRST code point of each case mapping, in both
    // directions. This is the part that reaches a far-away orbit partner.
    let mut lower_bucket: BTreeMap<char, Vec<char>> = BTreeMap::new();
    let mut upper_bucket: BTreeMap<char, Vec<char>> = BTreeMap::new();
    let mut cased: Vec<char> = Vec::new();
    for cp in 0u32..0x11_0000 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let lo = c.to_lowercase().next().expect("non-empty mapping");
        let up = c.to_uppercase().next().expect("non-empty mapping");
        lower_bucket.entry(lo).or_default().push(c);
        upper_bucket.entry(up).or_default().push(c);
        if !c.to_lowercase().eq([c]) || !c.to_uppercase().eq([c]) {
            cased.push(c);
        }
        for mapped in [single_lower(c), single_upper(c)].into_iter().flatten() {
            if mapped != c {
                parent.entry(c).or_insert(c);
                parent.entry(mapped).or_insert(mapped);
                let (ra, rb) = (root(&mut parent, c), root(&mut parent, mapped));
                if ra != rb {
                    parent.insert(ra, rb);
                }
            }
        }
    }

    let mut buf = [0u8; 4];
    let mut checked = 0usize;
    let mut skew = 0usize;
    for &c in &cased {
        let cp = c as u32;
        let escaped = regex::escape(&c.to_string());
        let Ok(oracle) = regex::Regex::new(&format!("(?i){escaped}")) else {
            continue;
        };
        let Ok(oracle_class) = regex::Regex::new(&format!("(?i)[{escaped}]")) else {
            continue;
        };
        let mine_src = aterm_regex::escape(&c.to_string());
        let mine = aterm_regex::Regex::new(&format!("(?i){mine_src}")).expect("compiles");
        let mine_class = aterm_regex::Regex::new(&format!("(?i)[{mine_src}]")).expect("compiles");

        let mut candidates: BTreeSet<char> = ('a'..='z').chain('A'..='Z').collect();
        candidates.insert(c);
        candidates.extend(c.to_lowercase().chain(c.to_uppercase()));
        // Both directions, and both "who maps to where I map" and "who maps
        // to me": U+1FD3 is found from U+0390 through the shared U+0399.
        for key in [
            c,
            c.to_lowercase().next().expect("non-empty"),
            c.to_uppercase().next().expect("non-empty"),
        ] {
            for bucket in [&lower_bucket, &upper_bucket] {
                if let Some(v) = bucket.get(&key) {
                    candidates.extend(v.iter().copied());
                }
            }
        }
        // Everything this engine folds `c` onto must be in the comparison, or a
        // spurious orbit member could slip through unexamined.
        for other in &candidates.clone() {
            let s: &str = other.encode_utf8(&mut buf);
            if mine.is_match(s) {
                candidates.insert(*other);
            }
        }

        for other in &candidates {
            let s: &str = other.encode_utf8(&mut buf);
            for (got, want, shape) in [
                (mine.is_match(s), oracle.is_match(s), "(?i)X"),
                (mine_class.is_match(s), oracle_class.is_match(s), "(?i)[X]"),
            ] {
                if got == want {
                    continue;
                }
                // The ONLY tolerated direction: we fold, the oracle does not,
                // and `std` says the pair is case-equivalent. Anything else --
                // above all the oracle folding a pair we miss -- is a bug here.
                assert!(
                    got && !want,
                    "{shape} U+{cp:04X}: the oracle folds U+{:04X} onto it and this engine \
                     does not. This is not version skew; it is a missing fold edge.",
                    *other as u32
                );
                let (ra, rb) = (root(&mut parent, c), root(&mut parent, *other));
                assert!(
                    parent.contains_key(&c) && parent.contains_key(other) && ra == rb,
                    "{shape} U+{cp:04X} folds onto U+{:04X} but `std`'s case mappings do \
                     not link them, so this is not Unicode-version skew either",
                    *other as u32
                );
                skew += 1;
            }
        }
        checked += 1;
    }
    assert!(
        checked > 2_000,
        "expected the whole cased range, checked {checked}"
    );
    // Non-zero and bounded, exactly as `perl_classes_match_the_oracle` bounds
    // the `\w` skew: real, known, and small.
    assert!(skew < 5_000, "unexpectedly large case-fold skew: {skew}");
}

/// The exotic orbits that make case folding more than `to_lowercase`.
#[test]
fn exotic_fold_orbits_match_the_oracle() {
    for (p, h) in [
        ("(?i)k", "kK\u{212a}"),
        ("(?i)[a-z]", "K\u{212a}\u{17f}Sz"),
        ("(?i)s", "sS\u{17f}"),
        ("(?i)\u{3c3}", "\u{3c3}\u{3c2}\u{3a3}"),
        ("(?i)\u{df}", "\u{df}\u{1e9e}ss"),
        ("(?i)a\u{30a}", "A\u{30a}"),
        ("(?i)\u{e5}", "\u{e5}\u{c5}\u{212b}"),
        ("(?i)i", "iI\u{131}\u{130}"),
        ("(?i)[^k]", "kK\u{212a}z"),
        ("(?i)\u{3a9}", "\u{3a9}\u{3c9}\u{2126}"),
    ] {
        agree(p, h);
    }
}

/// Zero-width matching, where `find_iter`'s advance rule lives. `aterm-search`
/// filters these out, but only after this engine has produced exactly the same
/// ones the oracle would.
#[test]
fn zero_width_matches_match_the_oracle() {
    for p in [
        r"", r"x*", r"a*", r"\b", r"\B", r"^", r"$", r"(?m)^", r"(?m)$", r"()", r"(?:)", r"a{0}",
        r"\bx*\b", r"(|a)*", r"\b|a", r"a*?", r"(?:a|)*",
    ] {
        for h in [
            "",
            "a",
            "aa",
            "aab",
            "hello",
            "ab cd",
            "a\nb",
            "\u{4f60}\u{597d}",
            "a\u{301}b c",
            "  x  ",
        ] {
            agree(p, h);
        }
    }
}

/// Repetitions whose body can match nothing, and repetitions whose body is
/// itself a lazy loop. This is the corner where the compiled shape decides the
/// answer — `(|a)*` has to prefer the empty iteration, and `(.*?){2,}\b` has to
/// stop at the first word boundary rather than run to the end — and both of
/// those broke, in opposite directions, before the star grew two shapes.
#[test]
fn nullable_and_lazy_repetitions_match_the_oracle() {
    const PATTERNS: &[&str] = &[
        r"(|a)*",
        r"(a|)*",
        r"(|a)+",
        r"(a|)+",
        r"(|a){2,}",
        r"(|a){0,3}",
        r"(|a){2}",
        r"(?:)*",
        r"()*",
        r"()+",
        r"(?:a?)*",
        r"(?:a*)*",
        r"(?:a*)+",
        r"(?:a*?)*",
        r"(a*)*b",
        r"(a*)+b",
        r"((a*)*)*b",
        r"(?:a*|b*)*c",
        r"(.*?){2,}\b",
        r"(.*?){2,}",
        r"(.*?)*\b",
        r"(.*?)+\b",
        r"(.*?){1,3}\b",
        r"(?U:.*){2,}\b",
        r"(?U:.*)*\b",
        r"(?U:a*)+b",
        r"(.*?b)*c",
        r"([^b]*?b)*c",
        r"(\b)*a",
        r"(\b|a)*",
        r"(^)*a",
        r"(a|\b)*b",
        r"(?:\b)+",
        r"(x*?){2,}y",
        r"(x*){2,}y",
        r"(x+?){2,}y",
        r"(x??){3,}y",
    ];
    const HAY: &[&str] = &[
        "",
        " ",
        "a",
        "aa",
        "aaa",
        "ab",
        " ab",
        "ab ",
        "aab",
        "b",
        "bb",
        "abc",
        "xxy",
        "xy",
        "y",
        " a b ",
        "a\nb",
        "\u{4f60}b\u{4f60}",
        "aa\u{e9}b\u{e9}",
        "K0\u{4f60}b\u{4f60}",
        "aab\u{e9}b",
        "c",
        "aaac",
        "abababc",
    ];
    for p in PATTERNS {
        for h in HAY {
            agree(p, h);
        }
    }
}

/// `(?x)` — extended mode, where whitespace is insignificant and `#` runs to
/// end of line. It is the flag with the most parser-level quirks, and they are
/// not symmetric: whitespace may separate `(` from the `?` that qualifies it,
/// and `[` from the `^` that negates it, and it may sit between a *counted*
/// repetition and its `?` laziness marker — but not between `*`, `+` or `?` and
/// theirs, where `a? ?` is `(a?)?` and not `a??`. Every one of those is checked
/// here, because every one of them was wrong first.
#[test]
fn extended_mode_matches_the_oracle() {
    const PATTERNS: &[&str] = &[
        r"(?x)a b c",
        r"(?x) a  b ",
        r"(?x)a#comment",
        "(?x)a#comment\nb",
        r"(?x)a? ?",
        r"(?x)a* ?",
        r"(?x)a+ ?",
        r"(?x)a{1,2} ?",
        r"(?x)a{2,} ?",
        r"(?x)a{1} ?",
        r"(?x)a?  ?",
        "(?x)a?\n?",
        r"(?x)a??",
        r"(?x)a*?",
        r"(?x)( ?:a)b",
        r"(?x)( ?i)abc",
        r"(?x)(  ?:a|b)",
        r"(?x)( ?<n>a)b",
        r"(?x)( ?P<n>a)b",
        "(?x)( #c\n?:a)b",
        r"(?x)[ ^a]",
        r"(?x)[ ]]",
        "(?x)[ #c\n^a]",
        r"(?x)[a b]",
        r"(?x)[a-  z]",
        r"(?x)a\ b",
        r"(?x)a{ 1 , 2 }",
        r"(?x)(a | b)+",
        r"(?x)a |b",
        r"(?x)\d {2}",
        r"(?x)x{2} ?",
        r"(?x:a b)c",
        r"a(?x:b c)d",
        r"(?x)(?-x:a b)",
        r"(?x)a(?-x)b c",
        r"(?x)\S{0,2}? \w",
        r"(?x) \b a \b ",
    ];
    const HAY: &[&str] = &[
        "",
        " ",
        "a",
        "aa",
        "ab",
        "abc",
        "a b c",
        "abcabc",
        "^a ",
        "]",
        "b",
        "aab",
        "A",
        "ABC",
        "12",
        "xx",
        "a#b",
        "a\nb",
        "\u{4f60} a",
        "  a  b  ",
        "za bz",
    ];
    for p in PATTERNS {
        for h in HAY {
            agree(p, h);
        }
    }
}

/// The prefilter — the byte-at-a-time scan that skips text no match can begin
/// in — is the one place a walk could stop inside a multi-byte code point, and
/// the one optimisation that could cost a match outright. Drive it at every
/// alignment over text where the skipped runs are multi-byte, and check the
/// answer against the oracle rather than against itself.
#[test]
fn the_prefilter_never_changes_the_answer() {
    const PATTERNS: &[&str] = &[
        r"zebra",
        r"\bzebra\b",
        r"^zebra",
        r"[z]ebra",
        r"z|Q",
        r"(?i)ZEBRA",
        r"\u{4f60}z",
        r"\d+z",
        r"[^\s]z",
        r"\bz",
        r"\<z",
        r"(?m)^z",
        r"\bz+\b",
        r"[a-z\u{4f60}]+",
        r"(?i)[k]\u{e9}",
        r"\u{1F600}+",
        // A failing assertion *mid*-pattern empties the thread list without
        // emptying its visited marks, and the next skip has to retire them.
        r"ab\Bc",
        r"x\B\d",
        r"\-?\B\d",
        r"z\b\d",
        r"a\bz|q",
        r"[a-c]\B[0-9]",
        r"\u{4f60}\Bz",
        r"q\B\u{4f60}",
        r"(?:ab|az)\Bc",
    ];
    let filler = "\u{4f60}\u{597d}\u{e9}\u{1F600}e\u{301}";
    for pattern in PATTERNS {
        for pad in 0..12usize {
            let prefix: String = filler.chars().cycle().take(pad).collect();
            for body in [
                "zebra",
                "\u{4f60}zebra",
                "Qx",
                "nothing here",
                "",
                "zz z",
                "K\u{e9}",
                "\u{1F600}\u{1F600}",
                "abxabc",
                "xy x1",
                "\u{e9}-bx\u{4f60}0\u{4f60}",
                "ab qz",
                "az0 ab1",
            ] {
                agree(pattern, &format!("{prefix}{body}{prefix}"));
            }
        }
    }
}

/// Every construct this engine deliberately refuses, each one proved to be a
/// refusal the oracle does *not* share — so the list is honest about the cost,
/// and so a future change that starts accepting one of these is visible.
#[test]
fn refusals_are_refusals_and_the_oracle_accepts_them() {
    for (pattern, expected) in [
        (r"\p{L}", "Unicode property classes"),
        (r"\pL", "Unicode property classes"),
        (r"[a&&b]", "set operations"),
        (r"[a--b]", "set operations"),
        (r"[a~~b]", "set operations"),
        (r"[[a][b]]", "nested character classes"),
        (r"(?-u)a", "byte-oriented"),
        (r"(?R)^", "CRLF mode"),
    ] {
        assert!(
            regex::Regex::new(pattern).is_ok(),
            "{pattern:?} is supposed to be something the oracle accepts"
        );
        let err = aterm_regex::Regex::new(pattern)
            .expect_err("this engine must refuse it, not reinterpret it")
            .to_string();
        assert!(
            err.contains(expected),
            "{pattern:?} was refused, but the message does not say why: {err}"
        );
    }
}

/// Malformed patterns both engines reject. The messages differ in wording; what
/// matters is that neither compiles something bogus into a matcher.
#[test]
fn malformed_patterns_are_rejected_by_both() {
    for pattern in [
        r"(unclosed",
        r"a)",
        r"[a",
        r"[z-a]",
        r"a{2,1}",
        r"a{,3}",
        r"a{",
        r"a{}",
        r"a{b}",
        r"*a",
        r"+a",
        r"?a",
        r"{2}",
        r"\",
        r"\q",
        r"[\q]",
        r"[\b]",
        r"\1",
        r"(?=a)",
        r"(?!a)",
        r"(?<=a)",
        r"(?<!a)",
        r"(?z)",
        r"(?i-i)",
        r"[]",
        r"[^]",
        r"\x{110000}",
        r"\uD800",
        r"[\d-a]",
        r"[a-\d]",
        r"\Q..\E",
        r"\Z",
        r"(?P=n)",
        r"(?<n>a)(?<n>b)",
        r"(?<n >a)",
        r"(?<n-x>a)",
        r"(?<1n>a)",
        r"(?<>a)",
        r"(?P<n >a)",
    ] {
        assert!(
            regex::Regex::new(pattern).is_err(),
            "{pattern:?} is supposed to be something the oracle rejects"
        );
        assert!(
            aterm_regex::Regex::new(pattern).is_err(),
            "this engine accepted {pattern:?}, which the oracle rejects"
        );
    }
}

/// This engine finds a leftmost start that some oracle versions miss.
///
/// `regex` 1.12's unanchored search misses the leftmost start when a repetition
/// whose body can consume a multi-byte code point is followed by a multi-byte
/// literal: `(.*b)*é` on `"aaébé"` reports `2..7`, though a match plainly begins
/// at 0. The oracle contradicts itself there — anchor the same pattern, or drive
/// it with a lazy prefix and read the capture, and it agrees with this engine
/// that the match starts at 0. The fuzz loop arbitrates disagreements that way
/// rather than trusting the oracle blindly, and this case is the reason it has
/// to.
///
/// ## Why this no longer asserts the defect is still there
///
/// It used to `assert_ne!` that the oracle still gets these wrong — a tripwire
/// meant to say "delete this case once upstream fixes it". That made a
/// third-party bug into a *requirement*, and it was pinned to the wrong crate.
/// The behaviour lives in `regex-automata`, not `regex`: holding the oracle at
/// `regex = "=1.12.3"` and moving only the transitive `regex-automata` 0.4.14 →
/// 0.4.18 flips all three cases to agree with this engine, and the `assert_ne!`
/// fails. Since the dev-dependency is `regex = "1"` and `regex` itself takes a
/// caret range on `regex-automata`, nothing pinned the crate that carries the
/// behaviour — so a plain `cargo update` would turn this crate red with a
/// message blaming the oracle rather than naming the cause.
///
/// The claim worth keeping is the unconditional one: **this engine finds the
/// leftmost match**, whichever oracle vintage resolves. The oracle half is now
/// conditional — when it disagrees, the anchored arbiter has to confirm us, so
/// the disagreement is still proved to be the oracle's and not ours; when it
/// agrees, there is simply nothing to arbitrate.
///
/// The NAME changed with the assertion, because the name was half the defect.
/// The old one — `the_oracle_misses_a_leftmost_start_and_this_engine_does_not`
/// — put a third-party bug where the property under test belongs, so an
/// upstream fix read as a local failure. A test that requires a defect to be
/// present punishes whoever fixes the defect.
///
/// This repo has retired that shape twice already, which is why it is not
/// re-litigated here. `aterm-forge`'s
/// `an_unpatched_sibling_version_reds_the_forge_verb` used to be pointed at the
/// real dependency graph's live `winnow` duplication and now plants the same
/// shape in a synthesized fixture, so resolving the duplicate upstream cannot
/// turn the gate red. And
/// `the_word_class_skew_moves_word_boundaries_in_a_shipped_rule` above pins its
/// concrete `\b` spans only while a live probe says the two engines still
/// disagree about U+088F, rather than hard-coding the oracle's answer and then
/// failing with a message about `\b` when the real news is that the divergence
/// closed.
#[test]
fn this_engine_finds_the_leftmost_start_whichever_oracle_vintage_resolves() {
    for (pattern, haystack, correct) in [
        (r"(.*b)*\u{e9}", "aa\u{e9}b\u{e9}", (0usize, 7usize)),
        (r"(.*?b)*\u{4f60}", "K0\u{4f60}b\u{4f60}", (0, 9)),
        (r"(.*?\u{e9})*b", "aab\u{e9}b", (0, 6)),
    ] {
        let mine = aterm_regex::Regex::new(pattern).expect("compiles");
        let oracle = regex::Regex::new(pattern).expect("compiles");
        let got = mine.find(haystack).map(|m| (m.start(), m.end()));
        assert_eq!(
            got,
            Some(correct),
            "this engine must find the leftmost match"
        );

        // The oracle contradicting itself is the proof, not our own say-so:
        // anchored, it agrees the match starts where we say it does. The prefix
        // flag is scoped so it cannot rewrite `{pattern}`'s own `.` — these
        // patterns are all built from `.`, so an unscoped `(?s)` would be
        // arbitrating a different pattern than the one under test.
        let arbiter = regex::Regex::new(&format!(r"\A(?s:(?:.)*?)({pattern})")).expect("compiles");
        let span = arbiter
            .captures(haystack)
            .and_then(|c| c.get(1))
            .map(|m| (m.start(), m.end()));
        assert_eq!(span, Some(correct), "the arbiter must confirm this engine");

        // Version-agnostic: whichever `regex-automata` resolves, either the
        // oracle agrees with us or the arbiter says it should have.
        let oracle_span = oracle.find(haystack).map(|m| (m.start(), m.end()));
        if oracle_span != Some(correct) {
            assert_eq!(
                span,
                Some(correct),
                "the oracle reported {oracle_span:?} for {pattern:?} on {haystack:?}; \
                 driven anchored it must still confirm {correct:?}"
            );
        }
    }
}

/// `escape` must round-trip: an escaped string matches itself and nothing it
/// should not, and means the same thing to both engines.
#[test]
fn escape_round_trips() {
    let mut seen = BTreeSet::new();
    for s in [
        r"a.b",
        r"a*b",
        r"[x]",
        r"(y)",
        r"a|b",
        r"a\b",
        r"^$",
        r"{2}",
        r"a-b",
        r"a&b",
        r"a~b",
        r"#c",
        r"+?",
        r"a<b>c",
        "\u{4f60}.*",
        "e\u{301}",
        "",
        " ",
        r"\\",
    ] {
        let escaped = aterm_regex::escape(s);
        seen.insert(escaped.clone());
        let mine = aterm_regex::Regex::new(&escaped)
            .unwrap_or_else(|e| panic!("escape({s:?}) = {escaped:?} did not compile:\n{e}"));
        let haystack = format!("<{s}>");
        assert_eq!(
            mine.find(&haystack).map(|m| m.as_str()),
            Some(if s.is_empty() { "" } else { s }),
            "escape({s:?}) must match {s:?} literally"
        );
        agree(&escaped, &haystack);
    }
    assert!(seen.len() > 15);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Matching behaviour over corpora: the patterns aterm ships, a hand-written
//! syntax corpus, the perl classes and case folding across all of Unicode,
//! zero-width and nullable repetitions, and the prefilter at every alignment.
//!
//! Each corpus is pinned by an FNV-1a digest of what this engine reports —
//! `find_iter` spans, `is_match` and `find` — recorded when every one of those
//! answers matched the `regex` crate this engine replaced. A changed digest is
//! a changed answer somewhere in that corpus.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

thread_local! {
    static GOLDEN: Cell<u64> = const { Cell::new(0xcbf2_9ce4_8422_2325) };
}

/// Fold `bytes` (and a separator) into this test thread's digest.
fn feed(bytes: &[u8]) {
    GOLDEN.with(|g| {
        let mut h = g.get();
        for &b in bytes.iter().chain(&[0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        g.set(h);
    });
}

fn feed_spans(spans: &[(usize, usize)]) {
    for &(a, b) in spans {
        feed(&(a as u64).to_le_bytes());
        feed(&(b as u64).to_le_bytes());
    }
    feed(b"/");
}

/// Assert this thread's digest, then reset it.
#[track_caller]
fn pin(expected: &str) {
    let got = GOLDEN.with(|g| g.replace(0xcbf2_9ce4_8422_2325));
    assert_eq!(format!("{got:016x}"), expected, "the pinned corpus changed");
}

/// The eleven `aterm-selection` built-in patterns: the specification this engine must
/// meet. `file_path` is an earlier form of the shipped rule; `step_budget_tests` in
/// `src/lib.rs` carries the current one.
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
    "mail trex.m21@example.test or a+b%c@sub.domain.co.uk!",
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
    // Newline-bearing: these tell `StartText` from `StartLine`.
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

/// Compile `p`, run it over `h`, hold `find_iter`, `is_match` and `find` to
/// each other, and fold the answer into the corpus digest.
#[track_caller]
fn record(p: &str, h: &str) {
    let re =
        aterm_regex::Regex::new(p).unwrap_or_else(|e| panic!("this engine rejected {p:?}:\n{e}"));
    let spans: Vec<(usize, usize)> = re.find_iter(h).map(|m| (m.start(), m.end())).collect();
    let first = re.find(h).map(|m| (m.start(), m.end()));
    assert_eq!(
        first,
        spans.first().copied(),
        "find vs find_iter for {p:?} on {h:?}"
    );
    assert_eq!(
        re.is_match(h),
        first.is_some(),
        "is_match vs find for {p:?} on {h:?}"
    );
    // Every offset must land on a code-point boundary, or the call sites'
    // `&text[m.start()..m.end()]` would panic.
    for &(start, end) in &spans {
        assert!(
            h.is_char_boundary(start) && h.is_char_boundary(end),
            "{p:?} on {h:?}"
        );
    }
    feed(p.as_bytes());
    feed(h.as_bytes());
    feed_spans(&spans);
}

#[test]
fn builtin_selection_patterns_are_pinned() {
    for &(name, pattern) in BUILTIN {
        for haystack in HAYSTACKS {
            let spans = spans_mine(pattern, haystack)
                .unwrap_or_else(|| panic!("builtin rule {name:?} must compile"));
            record(pattern, haystack);
            feed_spans(&spans);
        }
    }
    pin(BUILTIN_DIGEST);
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
        find(by_name("email"), "write to trex.m21@example.test today"),
        Some("trex.m21@example.test".to_string())
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
fn syntax_corpus_is_pinned() {
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
        // Start-anchor adversaries: each contains a `^` but is not anchored, so a
        // `Program::start_anchored` walk that stopped at the `^` would miss a match.
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
            record(p, h);
        }
    }
    pin(SYNTAX_DIGEST);
}

/// `\d`, `\w` and `\s` over every code point. Only the baked part of `\w` is pinned, not
/// `char::is_alphabetic`, so a toolchain bump cannot move the digest. Recorded when `\d`,
/// `\s` and the non-alphabetic `\w` matched `regex` exactly.
#[test]
fn perl_classes_are_pinned() {
    let d = aterm_regex::Regex::new(r"^\d$").expect("compiles");
    let w = aterm_regex::Regex::new(r"^\w$").expect("compiles");
    let sp = aterm_regex::Regex::new(r"^\s$").expect("compiles");
    let mut buf = [0u8; 4];
    for cp in 0u32..0x11_0000 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let s: &str = c.encode_utf8(&mut buf);
        let word = w.is_match(s);
        assert!(
            word || !c.is_alphabetic(),
            "\\w misses alphabetic U+{cp:04X}"
        );
        let baked_word = word && !c.is_alphabetic();
        let bits =
            u8::from(d.is_match(s)) | u8::from(baked_word) << 1 | u8::from(sp.is_match(s)) << 2;
        if bits != 0 {
            feed(&cp.to_le_bytes());
            feed(&[bits]);
        }
    }
    pin(PERL_DIGEST);
}

/// Case folding over the whole cased range, one code point at a time. Candidates are
/// bucketed by case mapping in both directions, which finds distant orbit partners
/// (`\u{390}` and `\u{1fd3}`, 0x1c43 apart, both uppercase to a string starting
/// `\u{399}`). Recorded when every answer matched `regex` except pairs `std`'s own case
/// mappings link (Unicode-version skew, folding more).
#[test]
fn case_folding_is_pinned() {
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
    }

    let mut buf = [0u8; 4];
    for &c in &cased {
        let src = aterm_regex::escape(&c.to_string());
        let single = aterm_regex::Regex::new(&format!("(?i){src}")).expect("compiles");
        let class = aterm_regex::Regex::new(&format!("(?i)[{src}]")).expect("compiles");

        let mut candidates: BTreeSet<char> = ('a'..='z').chain('A'..='Z').collect();
        candidates.insert(c);
        candidates.extend(c.to_lowercase().chain(c.to_uppercase()));
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
        feed(&(c as u32).to_le_bytes());
        for other in &candidates {
            let s: &str = other.encode_utf8(&mut buf);
            let (a, b) = (single.is_match(s), class.is_match(s));
            assert_eq!(a, b, "(?i)X and (?i)[X] disagree for U+{:04X}", c as u32);
            if a {
                feed(&(*other as u32).to_le_bytes());
            }
        }
    }
    assert!(cased.len() > 2_000, "expected the whole cased range");
    pin(FOLD_DIGEST);
}

/// The exotic orbits that make case folding more than `to_lowercase`.
#[test]
fn exotic_fold_orbits_are_pinned() {
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
        record(p, h);
    }
    pin(EXOTIC_DIGEST);
}

/// Zero-width matching, where `find_iter`'s advance rule lives.
#[test]
fn zero_width_matches_are_pinned() {
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
            record(p, h);
        }
    }
    pin(ZERO_WIDTH_DIGEST);
}

/// Repetitions whose body can match nothing, and repetitions whose body is
/// itself a lazy loop. This is the corner where the compiled shape decides the
/// answer — `(|a)*` has to prefer the empty iteration, and `(.*?){2,}\b` has to
/// stop at the first word boundary rather than run to the end.
#[test]
fn nullable_and_lazy_repetitions_are_pinned() {
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
            record(p, h);
        }
    }
    pin(NULLABLE_DIGEST);
}

/// `(?x)` extended mode, the flag with the most parser quirks: whitespace may separate
/// `(` from its `?`, `[` from its `^`, and a counted repetition from its lazy `?`, but not
/// `*`, `+` or `?` from theirs (`a? ?` is `(a?)?`).
#[test]
fn extended_mode_is_pinned() {
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
            record(p, h);
        }
    }
    pin(EXTENDED_DIGEST);
}

/// The prefilter is the one place a walk could stop inside a multi-byte code point and
/// the one optimisation that could cost a match: drive it at every alignment over
/// multi-byte filler.
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
                record(pattern, &format!("{prefix}{body}{prefix}"));
            }
        }
    }
    pin(PREFILTER_DIGEST);
}

/// Every construct this engine deliberately refuses (each one a construct the
/// `regex` crate accepted), so a change that starts accepting one is visible.
#[test]
fn refusals_are_refusals() {
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
        let err = aterm_regex::Regex::new(pattern)
            .expect_err("this engine must refuse it, not reinterpret it")
            .to_string();
        assert!(
            err.contains(expected),
            "{pattern:?} was refused, but the message does not say why: {err}"
        );
    }
}

/// Malformed patterns are rejected rather than compiled into something bogus.
#[test]
fn malformed_patterns_are_rejected() {
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
            aterm_regex::Regex::new(pattern).is_err(),
            "this engine accepted {pattern:?}"
        );
    }
}

/// A negated POSIX item is folded before it is negated, as a bracketed class is:
/// `(?i)[[:^lower:]]` is "not a letter of either case". Folding the complement of `[a-z]`
/// (which holds `A-Z`, `ſ` and KELVIN SIGN) would yield every code point instead. The
/// expected spans are what `regex` 1.12.4 reports; `aterm-search`'s default `(?i)` makes
/// them the find bar's answers.
#[test]
fn negated_posix_items_fold_before_they_negate() {
    // a Z ␠ k K ␠ s S ſ K ␠ 1 é _ -
    let haystack = "aZ kK sS\u{17f}\u{212a} 1\u{e9}_-";
    let not_a_letter = vec![
        (2, 3),
        (5, 6),
        (13, 14),
        (14, 15),
        (15, 17),
        (17, 18),
        (18, 19),
    ];
    let letters = vec![
        (0, 1),
        (1, 2),
        (3, 4),
        (4, 5),
        (6, 7),
        (7, 8),
        (8, 10),
        (10, 13),
    ];
    for (pattern, expected) in [
        (r"(?i)[[:^lower:]]", not_a_letter.clone()),
        (r"(?i)[[:^upper:]]", not_a_letter.clone()),
        (r"(?i)[[:^alpha:]]", not_a_letter.clone()),
        (r"(?i)[[:^lower:]0-9]", not_a_letter.clone()),
        (r"(?i)[^[:^lower:]]", letters),
        (
            r"(?i)[[:^alnum:]]",
            vec![(2, 3), (5, 6), (13, 14), (15, 17), (17, 18), (18, 19)],
        ),
        (
            r"(?i)[[:^word:]]",
            vec![(2, 3), (5, 6), (13, 14), (15, 17), (18, 19)],
        ),
        (r"(?i)[[:^print:]]", vec![(15, 17)]),
        // Controls: the same items without `(?i)`, and class-level negation.
        (
            r"[[:^lower:]]",
            vec![
                (1, 2),
                (2, 3),
                (4, 5),
                (5, 6),
                (7, 8),
                (8, 10),
                (10, 13),
                (13, 14),
                (14, 15),
                (15, 17),
                (17, 18),
                (18, 19),
            ],
        ),
        (r"(?i)[^[:lower:]]", not_a_letter),
    ] {
        let spans = spans_mine(pattern, haystack).expect("compiles");
        assert_eq!(spans, expected, "{pattern:?} on {haystack:?}");
    }
}

/// A quantifier right after a flag directive has nothing to repeat and is refused, as in
/// `regex`, not applied to the item before the directive.
#[test]
fn a_quantifier_after_a_flag_directive_is_refused() {
    for pattern in [
        r"a(?i)*",
        r"a(?i)+",
        r"a(?i)?",
        r"a(?i)*?",
        r"x(?s){2}",
        r"ab(?i)+c",
        r"a(?i)(?s)*",
        r"(?:a(?-i)*)",
        r"a(?x) *",
    ] {
        let err = aterm_regex::Regex::new(pattern)
            .expect_err("a quantifier after a flag directive repeats nothing")
            .to_string();
        assert!(
            err.contains("repetition operator missing expression"),
            "{pattern:?}: {err}"
        );
    }
    // The item after the directive is still repeatable, and a group after it is an
    // item like any other.
    assert_eq!(spans_mine(r"a(?i)b*", "aBB"), Some(vec![(0, 3)]));
    assert_eq!(spans_mine(r"a(?i)(?:b)*", "aBB"), Some(vec![(0, 3)]));
    assert_eq!(spans_mine(r"a(?i)(?:)*", "aa"), Some(vec![(0, 1), (1, 2)]));
}

/// The leftmost start when a repetition over multi-byte code points precedes a multi-byte
/// literal; some `regex-automata` versions report `2..7` for `(.*b)*é` on `"aaébé"`.
#[test]
fn this_engine_finds_the_leftmost_start() {
    for (pattern, haystack, correct) in [
        (r"(.*b)*\u{e9}", "aa\u{e9}b\u{e9}", (0usize, 7usize)),
        (r"(.*?b)*\u{4f60}", "K0\u{4f60}b\u{4f60}", (0, 9)),
        (r"(.*?\u{e9})*b", "aab\u{e9}b", (0, 6)),
    ] {
        let re = aterm_regex::Regex::new(pattern).expect("compiles");
        assert_eq!(
            re.find(haystack).map(|m| (m.start(), m.end())),
            Some(correct),
            "this engine must find the leftmost match"
        );
    }
}

/// `escape` must round-trip: an escaped string matches itself and nothing it
/// should not.
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
        record(&escaped, &haystack);
    }
    assert!(seen.len() > 15);
    pin(ESCAPE_DIGEST);
}

const BUILTIN_DIGEST: &str = "e064f89e51c0542c";
const SYNTAX_DIGEST: &str = "d9526bd496b9392f";
const PERL_DIGEST: &str = "6212cb3a3e2938ec";
const FOLD_DIGEST: &str = "b91b2d673464fac8";
const EXOTIC_DIGEST: &str = "033f0d201ef6505a";
const ZERO_WIDTH_DIGEST: &str = "c6278ff2ce852f5c";
const NULLABLE_DIGEST: &str = "75190a174fd22dac";
const EXTENDED_DIGEST: &str = "5addd2360f0054b0";
const PREFILTER_DIGEST: &str = "dc3f23d94d077828";
const ESCAPE_DIGEST: &str = "0fd8a26440ba8875";

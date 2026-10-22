// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compile and scan bounds, and the API contract the call sites rely on. Patterns are
//! user input, so no pattern or pattern-haystack pair may take unbounded memory, stack or
//! time.

use std::time::{Duration, Instant};

use aterm_regex::{Error, Regex, RegexBuilder};

/// The limits `aterm-observe` and `aterm-search` pass. 128 KiB (~2,048 instructions)
/// because the program size is also the per-row scan constant; it refuses the amplifiers
/// outright (see [`the_worst_program_under_the_ceiling_still_scans_a_row_promptly`]).
const SIZE_LIMIT: usize = 128 * 1024;
const DFA_SIZE_LIMIT: usize = 1 << 20;
/// The pattern-length ceiling both entry points enforce ahead of compilation.
const MAX_PATTERN_LEN: usize = 1024;

/// A bound on work that must finish, where the regression it catches never
/// does (an exponential search, four billion instructions emitted, a quadratic
/// rescan of megabytes): a hang detector of a minute, never a latency budget
/// (AGENTS.md): a whole-workspace test run is a loaded machine.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

fn bounded(pattern: &str) -> Result<Regex, Error> {
    RegexBuilder::new(pattern)
        .size_limit(SIZE_LIMIT)
        .dfa_size_limit(DFA_SIZE_LIMIT)
        .build()
}

/// `(a{200}){200}`: thirteen bytes, forty thousand instructions. Compiles under the
/// default limit and is refused at 128 KiB, as `aterm-observe`'s own test expects.
#[test]
fn the_amplifier_pattern_is_rejected_under_the_bounded_builder() {
    let amplifier = "(a{200}){200}";
    assert!(amplifier.len() <= MAX_PATTERN_LEN, "under the length gate");
    assert!(
        Regex::new(amplifier).is_ok(),
        "the default 10 MiB limit accepts this pattern, as the `regex` crate's does"
    );
    match bounded(amplifier) {
        Err(Error::CompiledTooBig(limit)) => assert_eq!(limit, SIZE_LIMIT),
        other => panic!("the 128 KiB builder must reject the amplifier, got {other:?}"),
    }
}

/// A pattern at the callers' length ceiling still compiles under their size
/// ceiling — the two bounds have to leave a usable pattern space between them.
#[test]
fn a_pattern_at_the_length_ceiling_still_compiles() {
    let at_cap = "a".repeat(MAX_PATTERN_LEN);
    assert_eq!(at_cap.len(), MAX_PATTERN_LEN);
    let re = bounded(&at_cap).expect("1024 literals fit comfortably under 128 KiB");
    assert!(re.is_match(&at_cap));

    // So does the alternation-heavy shape `aterm-search`'s size-limit test uses.
    let alternatives: Vec<String> = (0..100).map(|i| format!("alt{i:05}x")).collect();
    let pattern = alternatives.join("|");
    assert!(pattern.len() <= MAX_PATTERN_LEN);
    let re = bounded(&pattern).expect("100 alternations fit under 128 KiB");
    assert!(re.is_match("prefix alt00042x suffix"));
    assert!(!re.is_match("prefix alt00042y suffix"));
}

/// A counted repeat whose body emits nothing stops at its fixed point instead of replaying
/// `min` times without charging the ceiling (`(?:(?:){4294967295}){4294967295}` would
/// never finish). The collapse keeps the answer: an empty match at every boundary.
#[test]
fn an_empty_body_repeat_collapses_instead_of_spinning() {
    let started = Instant::now();
    for pattern in [
        "(?:){4294967295}",
        "(?:){4000000000}",
        "(?:){4294967295,}",
        "(){4294967295}",
        "(?:a{0}){4294967295}",
        r"(?:\b{0}){4294967295}",
        "(?:(?:){65535}){65535}",
        "(?:(?:){4294967295}){4294967295}",
    ] {
        assert!(
            pattern.len() <= MAX_PATTERN_LEN,
            "{pattern:?} has to be a pattern the call sites would actually accept"
        );
        let re = bounded(pattern).unwrap_or_else(|e| {
            panic!("{pattern:?} collapses to an empty match, not an error: {e}")
        });
        let haystack = "ab\u{e9}";
        let mine: Vec<(usize, usize)> = re
            .find_iter(haystack)
            .map(|m| (m.start(), m.end()))
            .collect();
        // The empty match at every code-point boundary.
        assert_eq!(
            mine,
            [(0, 0), (1, 1), (2, 2), (4, 4)],
            "{pattern:?} collapsed to a different program"
        );
    }
    assert!(
        started.elapsed() < HANG_DETECTOR,
        "an empty-bodied repeat must collapse at its fixed point rather than \
         replaying a no-op four billion times: {:?}",
        started.elapsed()
    );
}

/// The ceiling stops the compiler while it emits: a pattern asking for four billion
/// instructions fails promptly.
#[test]
fn an_unbounded_repetition_count_fails_fast_instead_of_allocating() {
    let started = Instant::now();
    for pattern in [
        "a{4000000000}",
        "(?:ab){999999999}",
        "a{1,4000000000}",
        "(?:a?){4294967295}",
    ] {
        assert!(
            matches!(bounded(pattern), Err(Error::CompiledTooBig(_))),
            "{pattern:?} must hit the ceiling"
        );
    }
    assert!(
        started.elapsed() < HANG_DETECTOR,
        "the ceiling must stop emission, not merely report afterwards: {:?}",
        started.elapsed()
    );
}

/// The size limit decides what compiles, in both directions.
#[test]
fn the_size_limit_is_the_thing_that_decides() {
    let pattern = "(a{50}){50}"; // 2,500 instructions
    assert!(
        RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .build()
            .is_ok(),
        "2,500 instructions fit in 1 MiB"
    );
    assert!(
        matches!(
            RegexBuilder::new(pattern).size_limit(1 << 10).build(),
            Err(Error::CompiledTooBig(1024))
        ),
        "and do not fit in 1 KiB"
    );
}

/// `dfa_size_limit` is accepted and changes nothing about what compiles.
#[test]
fn dfa_size_limit_is_accepted_and_inert() {
    let pattern = "(a{50}){50}";
    let builder = RegexBuilder::new(pattern).size_limit(1 << 20);
    assert!(builder.clone().dfa_size_limit(1).build().is_ok());
    assert!(builder.dfa_size_limit(usize::MAX).build().is_ok());
}

/// Deep nesting is an error, not a stack overflow: the parser is iterative, but the tree
/// and its `Drop` are not.
#[test]
fn deeply_nested_patterns_are_rejected_rather_than_overflowing() {
    for depth in [300usize, 5_000, 100_000] {
        let pattern = format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
        let err = Regex::new(&pattern)
            .expect_err("a pattern nested past the cap must be refused")
            .to_string();
        assert!(err.contains("nests more than"), "depth {depth}: {err}");
    }
    // Just under the cap still compiles and still matches.
    let pattern = format!("{}a{}", "(".repeat(200), ")".repeat(200));
    let re = Regex::new(&pattern).expect("200 levels is inside the cap");
    assert_eq!(re.find("xax").map(|m| m.range()), Some(1..2));

    // Stacked quantifiers nest the tree too, and are capped the same way.
    let stars = format!("a{}", "*".repeat(100_000));
    let err = Regex::new(&stars).expect_err("100k stacked stars must be refused");
    assert!(err.to_string().contains("nests more than"));
}

/// Deep nesting is dropped without overflow on a 256 KiB stack, where a recursive `Drop`
/// would fail.
#[test]
fn nesting_is_bounded_on_a_small_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            for depth in [249usize, 250, 251, 10_000] {
                let pattern = format!("{}a{}", "(?:".repeat(depth), ")".repeat(depth));
                let _ = Regex::new(&pattern);
            }
            "survived"
        })
        .expect("spawn");
    assert_eq!(handle.join().expect("no stack overflow"), "survived");
}

/// No exponential blow-up: `(a|a)*b` and friends finish against long runs of `a`.
#[test]
fn pathological_patterns_finish_promptly() {
    for (pattern, n) in [
        ("(a|a)*b", 20_000usize),
        ("(a*)*b", 20_000),
        ("(a+)+b", 20_000),
        ("(a|aa)+b", 20_000),
        // The textbook exponential case: 199 `a`s can never satisfy 200 + 200.
        ("(?:a?){200}a{200}", 199),
        ("a*a*a*a*a*b", 20_000),
    ] {
        let re = Regex::new(pattern).expect("compiles");
        let haystack = "a".repeat(n);
        let started = Instant::now();
        assert!(
            !re.is_match(&haystack),
            "{pattern:?} must not match a run of `a`"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < HANG_DETECTOR,
            "{pattern:?} over {n} chars took {elapsed:?} — the linear-time bound is gone"
        );
    }
}

/// Nested quantifiers over a run that does eventually match are linear too.
#[test]
fn adversarial_matching_input_is_also_linear() {
    let re = Regex::new("(a+)+b+").expect("compiles");
    for n in [1_000usize, 4_000, 16_000] {
        let haystack = format!("{}{}", "a".repeat(n), "b".repeat(n));
        let started = Instant::now();
        let m = re.find(&haystack).expect("matches");
        assert_eq!((m.start(), m.end()), (0, 2 * n));
        assert!(
            started.elapsed() < HANG_DETECTOR,
            "n={n} took {:?}",
            started.elapsed()
        );
    }
}

/// The thread-local scratch cache must not leak state between patterns, between
/// haystacks, or between threads.
#[test]
fn the_scratch_cache_is_reused_without_leaking_state() {
    let a = Regex::new(r"\d+").expect("compiles");
    let b = Regex::new(r"[a-z]+").expect("compiles");
    for _ in 0..1_000 {
        assert_eq!(a.find("xx 42 yy").map(|m| m.as_str()), Some("42"));
        assert_eq!(b.find("xx 42 yy").map(|m| m.as_str()), Some("xx"));
    }
    // Interleaved iterators over different programs share one cache.
    let mut ia = a.find_iter("1 22 333");
    let mut ib = b.find_iter("a bb ccc");
    assert_eq!(ia.next().map(|m| m.as_str()), Some("1"));
    assert_eq!(ib.next().map(|m| m.as_str()), Some("a"));
    assert_eq!(ia.next().map(|m| m.as_str()), Some("22"));
    assert_eq!(ib.next().map(|m| m.as_str()), Some("bb"));
    assert_eq!(ia.count(), 1);
    assert_eq!(ib.count(), 1);
}

/// `find_iter` over a long haystack is linear, not quadratic: over 2.8 MB a rescan per
/// match would be ~7 × 10^10 bytes, minutes even on a fast machine, so the hang detector
/// tells the two apart.
#[test]
fn scanning_a_long_haystack_is_linear() {
    let re = Regex::new(r"\b[0-9a-f]{7,40}\b").expect("compiles");
    let row = "commit 66390b5c8f2a1b3c and some ordinary words here; ";
    for repeats in [200usize, 800, 3_200, 51_200] {
        let haystack = row.repeat(repeats);
        let started = Instant::now();
        assert_eq!(re.find_iter(&haystack).count(), repeats);
        assert!(
            started.elapsed() < HANG_DETECTOR,
            "{repeats} rows took {:?}",
            started.elapsed()
        );
    }
}

/// Multi-byte haystacks: offsets are bytes, always on code-point boundaries, and
/// `.` never matches half a code point.
#[test]
fn offsets_are_byte_offsets_on_code_point_boundaries() {
    let text = "e\u{301}\u{4f60}\u{597d}\u{1F600}z";
    for pattern in [r".", r"\w", r"[^z]", r".+", r"\W", r"(?s).", r"\b", r"x*"] {
        let re = Regex::new(pattern).expect("compiles");
        for m in re.find_iter(text) {
            assert!(
                text.is_char_boundary(m.start()) && text.is_char_boundary(m.end()),
                "{pattern:?} produced {:?}, which splits a code point",
                m.range()
            );
        }
    }
    // `.` is one code point, not one byte.
    let dot = Regex::new(".").expect("compiles");
    assert_eq!(
        dot.find_iter(text).map(|m| m.len()).collect::<Vec<_>>(),
        vec![1, 2, 3, 3, 4, 1]
    );
}

/// The 128 KiB ceiling bounds the per-row scan cost (program size × row length): it
/// refuses the amplifiers outright, and the heaviest programs it admits still finish a
/// full row. The step budget (`step_budget_tests` in `src/lib.rs`) bounds the rest.
#[test]
fn the_worst_program_under_the_ceiling_still_scans_a_row_promptly() {
    // Refused outright under 128 KiB.
    for pattern in ["(?:x?){2000}z", "(?:x|x|x|x|x|x|x|x){400}z"] {
        assert!(
            pattern.len() <= MAX_PATTERN_LEN,
            "these pass the length gate"
        );
        assert!(
            matches!(bounded(pattern), Err(Error::CompiledTooBig(_))),
            "{pattern:?} costs tens of milliseconds per row and must not compile"
        );
    }

    // Admitted: just under the ceiling, on a full row.
    let row = "x".repeat(4096);
    for pattern in ["(?:x?){1020}z", "(?:x|x|x|x|x|x|x|x){100}z"] {
        let Ok(re) = bounded(pattern) else { continue };
        let started = Instant::now();
        assert!(
            !re.is_match(&row),
            "{pattern:?} must not match a run of `x`"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed < HANG_DETECTOR,
            "{pattern:?} took {elapsed:?} on one 4,096-column row — the compile \
             ceiling has stopped bounding the scan"
        );
    }
}

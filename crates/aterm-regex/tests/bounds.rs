// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compile and scan bound tests for `aterm-regex`.

use std::time::{Duration, Instant};

use aterm_regex::{Error, Regex, RegexBuilder};

const SIZE_LIMIT: usize = 128 * 1024;
const MAX_PATTERN_LEN: usize = 1024;
const HANG_DETECTOR: Duration = Duration::from_secs(60);

fn bounded(pattern: &str) -> Result<Regex, Error> {
    RegexBuilder::new(pattern).size_limit(SIZE_LIMIT).build()
}

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

#[test]
fn a_pattern_at_the_length_ceiling_still_compiles() {
    let at_cap = "a".repeat(MAX_PATTERN_LEN);
    assert_eq!(at_cap.len(), MAX_PATTERN_LEN);
    let re = bounded(&at_cap).expect("1024 literals fit comfortably under 128 KiB");
    assert!(re.is_match(&at_cap));

    let alternatives: Vec<String> = (0..100).map(|i| format!("alt{i:05}x")).collect();
    let pattern = alternatives.join("|");
    assert!(pattern.len() <= MAX_PATTERN_LEN);
    let re = bounded(&pattern).expect("100 alternations fit under 128 KiB");
    assert!(re.is_match("prefix alt00042x suffix"));
    assert!(!re.is_match("prefix alt00042y suffix"));
}

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

#[test]
fn deeply_nested_patterns_are_rejected_rather_than_overflowing() {
    for depth in [300usize, 5_000, 100_000] {
        let pattern = format!("{}a{}", "(".repeat(depth), ")".repeat(depth));
        let err = Regex::new(&pattern)
            .expect_err("a pattern nested past the cap must be refused")
            .to_string();
        assert!(err.contains("nests more than"), "depth {depth}: {err}");
    }
    let pattern = format!("{}a{}", "(".repeat(200), ")".repeat(200));
    let re = Regex::new(&pattern).expect("200 levels is inside the cap");
    assert_eq!(re.find("xax").map(|m| m.range()), Some(1..2));

    let stars = format!("a{}", "*".repeat(100_000));
    let err = Regex::new(&stars).expect_err("100k stacked stars must be refused");
    assert!(err.to_string().contains("nests more than"));
}

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

#[test]
fn pathological_patterns_finish_promptly() {
    for (pattern, n) in [
        ("(a|a)*b", 20_000usize),
        ("(a*)*b", 20_000),
        ("(a+)+b", 20_000),
        ("(a|aa)+b", 20_000),
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

#[test]
fn the_scratch_cache_is_reused_without_leaking_state() {
    let a = Regex::new(r"\d+").expect("compiles");
    let b = Regex::new(r"[a-z]+").expect("compiles");
    for _ in 0..1_000 {
        assert_eq!(a.find("xx 42 yy").map(|m| m.as_str()), Some("42"));
        assert_eq!(b.find("xx 42 yy").map(|m| m.as_str()), Some("xx"));
    }
    let mut ia = a.find_iter("1 22 333");
    let mut ib = b.find_iter("a bb ccc");
    assert_eq!(ia.next().map(|m| m.as_str()), Some("1"));
    assert_eq!(ib.next().map(|m| m.as_str()), Some("a"));
    assert_eq!(ia.next().map(|m| m.as_str()), Some("22"));
    assert_eq!(ib.next().map(|m| m.as_str()), Some("bb"));
    assert_eq!(ia.count(), 1);
    assert_eq!(ib.count(), 1);
}

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
    let dot = Regex::new(".").expect("compiles");
    assert_eq!(
        dot.find_iter(text).map(|m| m.len()).collect::<Vec<_>>(),
        vec![1, 2, 3, 3, 4, 1]
    );
}

#[test]
fn the_worst_program_under_the_ceiling_still_scans_a_row_promptly() {
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

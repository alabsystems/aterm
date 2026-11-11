// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn one_mib_gate(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .expect("the amplifiers compile under a 1 MiB ceiling — that is the problem")
}

const AMPLIFIERS: [&str; 2] = ["(?:x?){2000}z", "(?:x|x|x|x|x|x|x|x){400}z"];

const CALL_SITE_STEP_LIMIT: u64 = 1 << 22;

fn row() -> String {
    "x".repeat(4096)
}
fn scrollback() -> String {
    "x".repeat(3 * 1024 * 1024)
}

fn completes_within(pattern: &str, haystack: &str, limit: u64) -> bool {
    let re = RegexBuilder::new(pattern)
        .step_limit(limit)
        .build()
        .expect("compiles");
    let mut it = re.find_iter(haystack);
    while it.next().is_some() {}
    !it.step_limit_exceeded()
}

fn budget_needed(pattern: &str, haystack: &str) -> u64 {
    let mut hi = 1u64;
    while !completes_within(pattern, haystack, hi) {
        hi = hi.checked_mul(2).expect("a scan this expensive is the bug");
    }
    let mut lo = hi / 2;
    while lo / 64 + 1 < hi - lo {
        let mid = lo + (hi - lo) / 2;
        if completes_within(pattern, haystack, mid) {
            hi = mid
        } else {
            lo = mid
        }
    }
    hi
}

#[test]
fn the_amplifiers_terminate_promptly_instead_of_hanging() {
    let row = row();
    let scrollback = scrollback();
    for pattern in AMPLIFIERS {
        let re = RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .step_limit(CALL_SITE_STEP_LIMIT)
            .build()
            .expect("the amplifiers compile under a 1 MiB ceiling — that is the problem");
        for (what, haystack) in [("row", &row), ("3 MB scrollback", &scrollback)] {
            assert_eq!(
                re.try_is_match(haystack),
                Err(StepLimitExceeded::new(CALL_SITE_STEP_LIMIT)),
                "{pattern:?} on the {what} must report exhaustion"
            );
            assert!(!re.is_match(haystack), "the infallible form fails closed");
        }
    }
}

#[test]
fn the_default_budget_stops_the_runaway_scan() {
    let scrollback = scrollback();
    for pattern in AMPLIFIERS {
        let re = one_mib_gate(pattern);
        assert_eq!(
            re.try_is_match(&scrollback),
            Err(StepLimitExceeded::new(DEFAULT_STEP_LIMIT)),
            "{pattern:?} over 3 MB took 30.25 s before the budget existed"
        );
    }
}

#[test]
fn exhaustion_never_masquerades_as_no_match() {
    let haystack = format!("{}z", "x".repeat(4096));
    let pattern = AMPLIFIERS[0];

    let starved = RegexBuilder::new(pattern)
        .step_limit(10_000)
        .build()
        .expect("compiles");
    assert_eq!(
        starved.try_find(&haystack).map(|m| m.map(|m| m.range())),
        Err(StepLimitExceeded::new(10_000)),
        "a starved search must refuse, not answer"
    );
    assert!(
        !starved.is_match(&haystack),
        "the infallible form fails closed"
    );
    assert!(
        starved.step_limit_exceeded(),
        "and records that it did, so failing closed is not the same as being silent"
    );

    let fed = RegexBuilder::new(pattern)
        .step_limit(u64::MAX)
        .build()
        .expect("compiles");
    assert_eq!(
        fed.try_find(&haystack)
            .expect("no exhaustion")
            .map(|m| m.range()),
        Some(2096..4097),
        "the match the starved search refused to guess at is a real one"
    );
    assert!(!fed.step_limit_exceeded());
}

#[test]
fn find_iter_reports_truncation_and_fuses() {
    let haystack = format!("ab ab ab {}", "x".repeat(4096));
    let re = RegexBuilder::new("ab|(?:x?){2000}z")
        .size_limit(1 << 20)
        .step_limit(200_000)
        .build()
        .expect("compiles");
    let mut it = re.find_iter(&haystack);
    let found: Vec<_> = it.by_ref().map(|m| m.range()).collect();
    assert_eq!(
        found,
        vec![0..2, 3..5, 6..8],
        "the cheap matches are still reported"
    );
    assert!(
        it.step_limit_exceeded(),
        "and the truncation is on the record"
    );
    for _ in 0..1_000 {
        assert!(it.next().is_none(), "a truncated iterator is fused");
    }
    assert!(
        re.step_limit_exceeded(),
        "the regex carries the fact for infallible callers"
    );
}

#[test]
fn a_find_iter_is_held_to_one_budget_not_one_per_match() {
    let row = row();
    let n = row.len();
    let one_search = budget_needed_for_one_search(r"\w+=|\w", &row);
    let shared_limit = one_search.saturating_mul(4);
    let re = RegexBuilder::new(r"\w+=|\w")
        .size_limit(128 * 1024)
        .step_limit(shared_limit)
        .build()
        .expect("compiles");

    assert_eq!(
        re.try_find(&row).map(|m| m.map(|m| m.range())),
        Ok(Some(0..1))
    );

    let mut it = re.find_iter(&row);
    let found = it.by_ref().count();
    assert!(
        it.step_limit_exceeded(),
        "{found} of {n} matches yielded and no exhaustion reported against shared budget {shared_limit}"
    );
    assert!(
        (2..n).contains(&found),
        "shared 4-search budget must yield multiple matches before truncating, got {found} of {n}"
    );
    assert!(
        re.step_limit_exceeded(),
        "and records it for infallible callers"
    );

    let linear = RegexBuilder::new(r"\w")
        .size_limit(128 * 1024)
        .step_limit(CALL_SITE_STEP_LIMIT)
        .build()
        .expect("compiles");
    let mut it = linear.find_iter(&row);
    assert_eq!(it.by_ref().count(), n);
    assert!(!it.step_limit_exceeded());
}

fn budget_needed_for_one_search(pattern: &str, haystack: &str) -> u64 {
    let fits = |limit: u64| {
        RegexBuilder::new(pattern)
            .step_limit(limit)
            .build()
            .expect("compiles")
            .try_find(haystack)
            .is_ok()
    };
    let mut hi = 1u64;
    while !fits(hi) {
        hi = hi
            .checked_mul(2)
            .expect("a search this expensive is the bug");
    }
    let mut lo = hi / 2;
    while lo / 64 + 1 < hi - lo {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) { hi = mid } else { lo = mid }
    }
    hi
}

#[test]
fn every_builtin_selection_rule_stays_far_inside_the_default() {
    let rules: [(&str, &str); 11] = [
        (
            r#"(?i)(?:https?|ftp|file)://[^\s<>\[\](){}'"`,;|\\^]+[^\s<>\[\](){}'"`,;|\\^.!?:]"#,
            "see https://example.com/path/to/page?q=1&r=2 and ftp://host/file ",
        ),
        (
            r"(?:~|\.{1,2}|[a-zA-Z0-9._-]+)?(?:/[a-zA-Z0-9._-]+)+/?|[A-Za-z]:[/\\](?:[a-zA-Z0-9._-]+[/\\])*[a-zA-Z0-9._-]+",
            "/usr/local/share/aterm/crates/aterm-regex/src/pikevm.rs ",
        ),
        (
            r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?)*\.[a-zA-Z]{2,}",
            "first.last@example.test and someone.else@example.co.uk ",
        ),
        (
            r"(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)(?::\d{1,5})?",
            "192.168.001.254:8080 10.0.0.1 255.255.255.255 ",
        ),
        (
            r"\[?(?:(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,7}:|(?:[0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|::(?:[0-9a-fA-F]{1,4}:){0,5}[0-9a-fA-F]{1,4}|::)\]?(?::\d{1,5})?",
            "[2001:0db8:85a3:0000:0000:8a2e:0370:7334]:443 fe80::1 ",
        ),
        (
            r"\b[0-9a-fA-F]{7,40}\b",
            "commit 66390b5c8f2a1b3c4d5e6f7a8b9c0d1e2f3a4b5c landed ",
        ),
        (
            r#""(?:[^"\\]|\\.)*""#,
            "echo \"a quoted \\\"string\\\" here\" | cat ",
        ),
        (
            r"'(?:[^'\\]|\\.)*'",
            "echo 'a quoted \\'string\\' here' | cat ",
        ),
        (
            r"`(?:[^`\\]|\\.)*`",
            "run `git log --oneline` and `cargo test` ",
        ),
        (
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
            "id 123e4567-e89b-12d3-a456-426614174000 done ",
        ),
        (
            r"v?\d+\.\d+\.\d+(?:-[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?(?:\+[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?",
            "aterm v0.47.0-rc.1+build.1787445038 released ",
        ),
    ];

    for (pattern, sample) in rules {
        let row: String = sample.repeat(4096 / sample.len() + 1);
        let per_row = budget_needed(pattern, &row);
        println!("{per_row:>12} units/row  {pattern}");
        assert!(
            per_row <= CALL_SITE_STEP_LIMIT / 8,
            "{pattern:?} needs {per_row} units to find every match in a full row; the \
             docs claim every built-in rule is far inside even the call sites' budget of \
             {CALL_SITE_STEP_LIMIT}, let alone the default of {DEFAULT_STEP_LIMIT}"
        );
        let scrollback: String = sample.repeat(1024 * 1024 / sample.len() + 1);
        let re = Regex::new(pattern).expect("compiles");
        for haystack in [&row, &scrollback] {
            let mut it = re.find_iter(haystack);
            let count = it.by_ref().count();
            assert!(
                count > 0 && !it.step_limit_exceeded(),
                "{pattern:?} matched nothing"
            );
        }
        assert!(!re.step_limit_exceeded());
    }
}

#[test]
fn the_worst_admitted_program_still_fits_the_default_budget() {
    for pattern in ["(?:x?){1020}z", "(?:x|x|x|x|x|x|x|x){100}z"] {
        let Ok(_) = RegexBuilder::new(pattern).size_limit(128 * 1024).build() else {
            continue;
        };
        let needed = budget_needed(pattern, &row());
        println!("{needed:>12} row  {pattern}  (heaviest admitted)");
        assert!(
            needed <= DEFAULT_STEP_LIMIT,
            "{pattern:?} needs {needed} units on one row, over the default budget of \
             {DEFAULT_STEP_LIMIT}: a pattern the 128 KiB ceiling admits must still run"
        );
    }
}

#[test]
fn the_step_limit_is_the_thing_that_decides() {
    let pattern = AMPLIFIERS[0];
    let row = row();
    assert!(
        RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .step_limit(u64::MAX)
            .build()
            .expect("compiles")
            .try_is_match(&row)
            .is_ok(),
        "an unbounded budget still answers — slowly, which is the caller's choice"
    );
    assert!(
        RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .step_limit(1_000)
            .build()
            .expect("compiles")
            .try_is_match(&row)
            .is_err(),
        "a small one refuses"
    );
    assert_eq!(
        RegexBuilder::new("a")
            .step_limit(7)
            .build()
            .expect("compiles")
            .step_limit(),
        7
    );
    assert_eq!(
        RegexBuilder::new("a")
            .build()
            .expect("compiles")
            .step_limit(),
        DEFAULT_STEP_LIMIT
    );
    assert_eq!(
        Regex::new("a").expect("compiles").step_limit(),
        DEFAULT_STEP_LIMIT
    );
}

#[test]
fn ordinary_patterns_are_unaffected() {
    let text = "commit 66390b5c8f is the release; see http://example.com/x?y=1 (v1.2.3)";
    for pattern in [
        r"\b[0-9a-f]{7,40}\b",
        r"\w+",
        "a|ab",
        "(?i)COMMIT",
        r"\d+\.\d+\.\d+",
    ] {
        let unbounded = RegexBuilder::new(pattern)
            .step_limit(u64::MAX)
            .build()
            .expect("ok");
        let defaulted = Regex::new(pattern).expect("compiles");
        let tight = RegexBuilder::new(pattern)
            .step_limit(1 << 16)
            .build()
            .expect("ok");
        let spans = |re: &Regex| -> Vec<std::ops::Range<usize>> {
            re.find_iter(text).map(|m| m.range()).collect()
        };
        assert_eq!(spans(&defaulted), spans(&unbounded), "{pattern:?}");
        assert_eq!(spans(&tight), spans(&unbounded), "{pattern:?}");
        assert!(!defaulted.step_limit_exceeded() && !tight.step_limit_exceeded());
    }
}

#[test]
fn the_cut_short_flag_is_shared_with_clones() {
    let re = RegexBuilder::new(AMPLIFIERS[0])
        .size_limit(1 << 20)
        .step_limit(1_000)
        .build()
        .expect("compiles");
    let clone = re.clone();
    assert!(!clone.step_limit_exceeded());
    assert!(!re.is_match(&row()));
    assert!(
        clone.step_limit_exceeded(),
        "the clone sees what the original learned"
    );
}

#[test]
fn find_iter_with_limit_spends_the_budget_it_is_handed() {
    let text = format!("{} needle", "x".repeat(10_000));
    let needed = budget_needed("needle", &text);
    let tight = RegexBuilder::new("needle")
        .step_limit(needed / 2)
        .build()
        .expect("compiles");
    let mut it = tight.find_iter_with_limit(&text, needed);
    assert_eq!(it.next().map(|m| m.range()), Some(10_001..10_007));
    assert_eq!(it.next(), None);
    assert!(!it.step_limit_exceeded(), "the larger budget finishes");

    let roomy = Regex::new("needle").expect("compiles");
    let mut it = roomy.find_iter_with_limit(&text, needed / 2);
    assert_eq!(it.next(), None);
    assert!(it.step_limit_exceeded(), "the smaller budget does not");
    assert!(roomy.step_limit_exceeded(), "and the pattern records it");

    let mut it = roomy.find_iter_with_limit("a needle", needed / 2);
    assert_eq!(it.next().map(|m| m.range()), Some(2..8));
    assert_eq!(it.next(), None);
    assert!(!it.step_limit_exceeded());
}

#[test]
fn step_budget_and_span_const_helpers_invariants() {
    let err = StepLimitExceeded::new(100);
    assert_eq!(err.limit(), 100);
    assert!(err.within_budget(100));
    assert!(!err.within_budget(101));
    assert_eq!(err.remaining_budget(40), 60);
    assert_eq!(err.remaining_budget(200), 0);
    assert_eq!(default_size_limit(), 10 * (1 << 20));
    assert_eq!(default_step_limit(), 1 << 26);
    assert_eq!(clamp_size_limit(100, 1024), 1024);
    assert_eq!(clamp_size_limit(usize::MAX, 1024), default_size_limit());
    assert_eq!(clamp_step_limit(10, 1000), 1000);
    assert_eq!(clamp_step_limit(u64::MAX, 1000), default_step_limit());
    assert_eq!(debit_step_budget(50, 20), Some(30));
    assert_eq!(debit_step_budget(50, 60), None);
    assert!(valid_match_span(2, 8, 10));
    assert!(!valid_match_span(8, 2, 10));
    assert_eq!(match_span_len(2, 8), 6);
    assert!(disjoint_ordered_spans(0, 3, 3, 7));
    assert!(!disjoint_ordered_spans(0, 4, 3, 7));
    assert!(is_ascii_meta_byte(b'*'));
    assert!(!is_ascii_meta_byte(b'a'));
    let sp = encode_match_span_le(123, 456);
    assert_eq!(decode_match_span_le(sp), (123, 456));
}

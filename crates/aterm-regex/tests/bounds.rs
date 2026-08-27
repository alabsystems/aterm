// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bounds, and the API contract the call sites rely on.
//!
//! Patterns reaching this engine are user input — a selection rule, an
//! `await match <re>` argument, a search box — so "it compiles and matches" is
//! only half the requirement. The other half is that no pattern, and no
//! pattern-plus-haystack pair, can be made to eat unbounded memory, unbounded
//! stack or unbounded time.

use std::time::Instant;

use aterm_regex::{Error, Regex, RegexBuilder};

/// The limits `aterm-observe` and `aterm-search` pass at their entry points.
///
/// 128 KiB, not the 1 MiB these call sites started with, and the reason is the
/// *scan* rather than the compile. A Pike VM's search is linear in the haystack,
/// but the constant of that linearity is the program size — so `size_limit` is
/// the only thing bounding per-row match cost as well as compile cost. At 1 MiB
/// a thirteen-byte pattern could compile to ~16k instructions and cost 37 ms on
/// a single 4,096-column row; 128 KiB caps that at ~2,048 instructions and
/// ~18 ms, refusing the amplifiers outright. See
/// [`the_worst_program_under_the_ceiling_still_scans_a_row_promptly`].
const SIZE_LIMIT: usize = 128 * 1024;
const DFA_SIZE_LIMIT: usize = 1 << 20;
/// The pattern-length ceiling both entry points enforce ahead of compilation.
const MAX_PATTERN_LEN: usize = 1024;

fn bounded(pattern: &str) -> Result<Regex, Error> {
    RegexBuilder::new(pattern)
        .size_limit(SIZE_LIMIT)
        .dfa_size_limit(DFA_SIZE_LIMIT)
        .build()
}

/// `(a{200}){200}` — thirteen bytes of pattern, forty thousand instructions of
/// automaton. `aterm-observe` has a test asserting this exact pattern compiles
/// under the default limit and is rejected under its 1 MiB one, and that test
/// has to keep passing after the migration.
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
        other => panic!("the 1 MiB builder must reject the amplifier, got {other:?}"),
    }
}

/// A pattern at the callers' length ceiling still compiles under their size
/// ceiling — the two bounds have to leave a usable pattern space between them.
#[test]
fn a_pattern_at_the_length_ceiling_still_compiles() {
    let at_cap = "a".repeat(MAX_PATTERN_LEN);
    assert_eq!(at_cap.len(), MAX_PATTERN_LEN);
    let re = bounded(&at_cap).expect("1024 literals fit comfortably under 1 MiB");
    assert!(re.is_match(&at_cap));

    // So does the alternation-heavy shape `aterm-search`'s size-limit test uses.
    let alternatives: Vec<String> = (0..100).map(|i| format!("alt{i:05}x")).collect();
    let pattern = alternatives.join("|");
    assert!(pattern.len() <= MAX_PATTERN_LEN);
    let re = bounded(&pattern).expect("100 alternations fit under 1 MiB");
    assert!(re.is_match("prefix alt00042x suffix"));
    assert!(!re.is_match("prefix alt00042y suffix"));
}

/// A counted repeat whose body emits *nothing* must collapse, not spin.
///
/// `size_limit` is charged per instruction emitted, so a body that emits no
/// instructions replays `min` times without ever consulting the ceiling — and
/// the ceiling is the only thing standing between the call sites and a
/// user-supplied pattern. `(?:){4294967295}` is sixteen bytes, passes the
/// 1024-byte length gate at every call site, and used to take **6.4 seconds**
/// in release through this exact builder. Nested, the two counts multiply:
/// `(?:(?:){4294967295}){4294967295}` is thirty-two bytes and 1.8e19 steps,
/// which is not a slow compile but an unkillable hang — no allocation, no
/// error, nothing for `CompiledTooBig` to fire on.
///
/// Both `{n}` and `{n,}` reach it, through five spellings. The fix is a fixed
/// point rather than a new limit: a body that emitted nothing emits nothing
/// every subsequent time, so the replay loop stops at the first no-op copy. The
/// resulting program is *identical*, which is why every case here is also
/// checked against the oracle — a collapse that changed the answer would be a
/// worse bug than the hang.
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
        let re = bounded(pattern)
            .unwrap_or_else(|e| panic!("{pattern:?} collapses to an empty match, not an error: {e}"));
        let haystack = "ab\u{e9}";
        let mine: Vec<(usize, usize)> =
            re.find_iter(haystack).map(|m| (m.start(), m.end())).collect();
        let theirs: Vec<(usize, usize)> = regex::Regex::new(pattern)
            .expect("the oracle compiles all of these in microseconds")
            .find_iter(haystack)
            .map(|m| (m.start(), m.end()))
            .collect();
        assert_eq!(mine, theirs, "{pattern:?} collapsed to a different program");
    }
    assert!(
        started.elapsed().as_secs() < 5,
        "an empty-bodied repeat must collapse at its fixed point rather than \
         replaying a no-op four billion times: {:?}",
        started.elapsed()
    );
}

/// The ceiling has to stop the compiler *while* it is emitting, not after: a
/// pattern asking for four billion instructions must fail promptly.
#[test]
fn an_unbounded_repetition_count_fails_fast_instead_of_allocating() {
    let started = Instant::now();
    for pattern in ["a{4000000000}", "(?:ab){999999999}", "a{1,4000000000}"] {
        assert!(
            matches!(bounded(pattern), Err(Error::CompiledTooBig(_))),
            "{pattern:?} must hit the ceiling"
        );
    }
    assert!(
        started.elapsed().as_secs() < 5,
        "the ceiling must stop emission, not merely report afterwards: {:?}",
        started.elapsed()
    );
}

/// Raising the limit raises what compiles, and lowering it lowers it — the knob
/// is real, not decorative.
#[test]
fn the_size_limit_is_the_thing_that_decides() {
    let pattern = "(a{50}){50}"; // 2,500 instructions
    assert!(
        RegexBuilder::new(pattern).size_limit(1 << 20).build().is_ok(),
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

/// `dfa_size_limit` is accepted and recorded, and — deliberately — changes
/// nothing about what compiles. A Pike VM has no DFA to bound, and quietly
/// repurposing the setting as a second cap on the program would reject patterns
/// the caller expected to work.
#[test]
fn dfa_size_limit_is_accepted_and_inert() {
    let pattern = "(a{50}){50}";
    let builder = RegexBuilder::new(pattern).size_limit(1 << 20);
    assert!(builder.clone().dfa_size_limit(1).build().is_ok());
    assert!(builder.clone().dfa_size_limit(usize::MAX).build().is_ok());
    assert_eq!(builder.dfa_size_limit(4096).get_dfa_size_limit(), 4096);
}

/// Deep nesting must return an error, not overflow the stack. The parser is
/// iterative, but the tree it builds is not, so the depth cap is what keeps
/// even `Drop` bounded.
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

/// A deeply nested pattern must not overflow the stack even while being torn
/// down — run it on a thread with a small stack so a recursive `Drop` would be
/// caught rather than tolerated.
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

/// The linear-time property, which is the entire reason for the Pike VM.
///
/// `(a|a)*b` against a long run of `a`s is the textbook exponential blow-up: a
/// backtracker explores `2^n` paths and never returns. Here the work is bounded
/// by program size times input length, so it finishes in milliseconds — and the
/// test asserts that, rather than merely asserting the answer.
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
        assert!(!re.is_match(&haystack), "{pattern:?} must not match a run of `a`");
        let elapsed = started.elapsed();
        assert!(
            elapsed.as_secs() < 5,
            "{pattern:?} over {n} chars took {elapsed:?} — the linear-time bound is gone"
        );
    }
}

/// `a^n b^n`-shaped adversarial input: nested quantifiers over a long run that
/// *does* eventually match. Still linear.
#[test]
fn adversarial_matching_input_is_also_linear() {
    let re = Regex::new("(a+)+b+").expect("compiles");
    for n in [1_000usize, 4_000, 16_000] {
        let haystack = format!("{}{}", "a".repeat(n), "b".repeat(n));
        let started = Instant::now();
        let m = re.find(&haystack).expect("matches");
        assert_eq!((m.start(), m.end()), (0, 2 * n));
        assert!(
            started.elapsed().as_secs() < 5,
            "n={n} took {:?}",
            started.elapsed()
        );
    }
}

/// The three things every call site does with a `Match`.
#[test]
fn match_accessors() {
    let re = Regex::new(r"\w+").expect("compiles");
    let text = "caf\u{e9} au lait";
    let m = re.find(text).expect("matches");
    assert_eq!((m.start(), m.end()), (0, 5));
    assert_eq!(m.as_str(), "caf\u{e9}");
    assert_eq!(m.range(), 0..5);
    assert_eq!(m.len(), 5);
    assert!(!m.is_empty());
    assert_eq!(&text[m.start()..m.end()], "caf\u{e9}");

    let empty = Regex::new("x*").expect("compiles").find("y").expect("matches");
    assert_eq!(empty.len(), 0);
    assert!(empty.is_empty() && empty.as_str().is_empty());
}

/// The traits the call sites need: `aterm-selection` clones a rule and prints
/// its pattern, `aterm-observe` derives `Debug` over a struct holding one.
#[test]
fn regex_is_cloneable_debuggable_and_shareable() {
    let re = Regex::new(r"^ab+c$").expect("compiles");
    assert_eq!(re.as_str(), r"^ab+c$");
    assert_eq!(format!("{re:?}"), r"^ab+c$");
    assert_eq!(format!("{re}"), r"^ab+c$");

    let clone = re.clone();
    assert!(clone.is_match("abbbc") && !clone.is_match("ac "));
    assert_eq!(clone.as_str(), re.as_str());

    assert_eq!(r"a+b".parse::<Regex>().expect("compiles").as_str(), "a+b");

    // Send + Sync: the watcher stack stores one behind an `Arc<dyn RowMatch>`.
    let shared = std::sync::Arc::new(re);
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let re = std::sync::Arc::clone(&shared);
            std::thread::spawn(move || re.find_iter("abc abbc abbbc").count())
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().expect("no panic"), 0, "the pattern is anchored");
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

/// `Error::Syntax` is constructible by dependents: `aterm-observe` builds one
/// directly to report its own pattern-length ceiling in the same shape as a
/// compile failure, and displays it verbatim.
#[test]
fn syntax_errors_are_constructible_and_intelligible() {
    let mine = Error::Syntax(format!(
        "pattern exceeds maximum length ({MAX_PATTERN_LEN} bytes)"
    ));
    assert_eq!(mine.to_string(), "pattern exceeds maximum length (1024 bytes)");

    let err = Regex::new("(unclosed").expect_err("must fail").to_string();
    assert!(err.contains("unclosed group"), "{err}");
    assert!(err.contains("(unclosed"), "the message must quote the pattern: {err}");
    assert!(err.contains('^'), "the message must point at the position: {err}");

    assert_eq!(
        Error::CompiledTooBig(1 << 20).to_string(),
        "Compiled regex exceeds size limit of 1048576 bytes."
    );

    // Every refusal says something a person can act on.
    for pattern in ["[a", "a{2,1}", "\\", "(?=a)", "\\p{L}", "[a&&b]", "(?-u)x"] {
        let err = Regex::new(pattern).expect_err("must fail").to_string();
        assert!(err.len() > 20, "{pattern:?} produced a useless message: {err}");
        assert!(err.contains("error: "), "{pattern:?}: {err}");
    }
}

/// A long haystack scanned with `find_iter` — the shape `aterm-search` runs over
/// scrollback — must be linear in the haystack too, not quadratic.
#[test]
fn scanning_a_long_haystack_is_linear() {
    let re = Regex::new(r"\b[0-9a-f]{7,40}\b").expect("compiles");
    let row = "commit 66390b5c8f2a1b3c and some ordinary words here; ";
    for repeats in [200usize, 800, 3_200] {
        let haystack = row.repeat(repeats);
        let started = Instant::now();
        assert_eq!(re.find_iter(&haystack).count(), repeats);
        assert!(
            started.elapsed().as_secs() < 5,
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

/// The compile ceiling has to bound the *scan*, because nothing else does.
///
/// `pathological_patterns_finish_promptly` proves there is no exponential
/// blow-up, which is the guarantee a backtracking engine cannot give. It does
/// not prove the scan is cheap: every pattern in it compiles to a handful of
/// instructions, so it cannot see the cost that actually matters here. A Pike VM
/// advances *every live thread* at every position, so a scan costs
/// `O(|haystack| x program size)` — linear in the input, with the program as the
/// constant — and `size_limit` is the only bound on that constant.
///
/// That is the whole reason the call sites dropped from 1 MiB to 128 KiB. Under
/// 1 MiB, `(?:x?){2000}z` — thirteen bytes, inside every length gate in the
/// tree — compiled to ~16k instructions and took 37 ms on one 4,096-column row
/// and 28.5 s on a 3 MB scrollback, against 24 us and 74 us for the oracle.
/// Under 128 KiB it does not compile at all.
///
/// The precise half of this test is the pair of **refusals**: those are
/// deterministic, and they are what the 128 KiB ceiling actually buys. The
/// timing half is a coarse backstop for the shapes the ceiling still admits —
/// ~18 ms per row in release and ~0.7 s in an unoptimised test build on the m21
/// box, bounded here at five seconds to match the rest of this file, because a
/// wall-clock assertion tight enough to be interesting is also tight enough to
/// be flaky on a loaded CI box. It catches a regression of orders of magnitude,
/// which is the only kind that has ever mattered here.
///
/// What this test does **not** claim: that the scan is bounded in absolute
/// terms. It is not. At ~18 ms per row an adversarial pattern still costs
/// minutes over a 20,000-row scrollback — bounded and interruptible, since
/// `BudgetedSearch` feeds rows in budgeted batches, but slow. Bounding that
/// needs a step budget in the VM that the call sites opt into; see the crate
/// docs, which say so rather than implying the ceiling is a complete answer.
#[test]
fn the_worst_program_under_the_ceiling_still_scans_a_row_promptly() {
    // Refused outright: the two amplifiers that motivated the 128 KiB ceiling.
    for pattern in ["(?:x?){2000}z", "(?:x|x|x|x|x|x|x|x){400}z"] {
        assert!(pattern.len() <= MAX_PATTERN_LEN, "these pass the length gate");
        assert!(
            matches!(bounded(pattern), Err(Error::CompiledTooBig(_))),
            "{pattern:?} costs tens of milliseconds per row and must not compile"
        );
    }

    // Admitted, and therefore pinned: just under the ceiling, on a full row.
    let row = "x".repeat(4096);
    for pattern in ["(?:x?){1020}z", "(?:x|x|x|x|x|x|x|x){100}z"] {
        let Ok(re) = bounded(pattern) else { continue };
        let started = Instant::now();
        assert!(!re.is_match(&row), "{pattern:?} must not match a run of `x`");
        let elapsed = started.elapsed();
        assert!(
            elapsed.as_secs() < 5,
            "{pattern:?} took {elapsed:?} on one 4,096-column row — the compile \
             ceiling has stopped bounding the scan"
        );
    }
}

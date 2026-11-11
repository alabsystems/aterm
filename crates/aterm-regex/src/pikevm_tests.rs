// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::compile::compile;
use crate::parse::{Flags, parse};

#[test]
fn assertions_read_the_surrounding_code_points() {
    let text = "ab cd";
    let positions = |kind: Assertion| -> Vec<usize> {
        (0..=text.len())
            .filter(|&at| satisfied(kind, text, at))
            .collect()
    };
    assert_eq!(positions(Assertion::StartText), vec![0]);
    assert_eq!(positions(Assertion::EndText), vec![5]);
    assert_eq!(positions(Assertion::WordBoundary), vec![0, 2, 3, 5]);
    assert_eq!(positions(Assertion::NotWordBoundary), vec![1, 4]);
    assert_eq!(positions(Assertion::WordStart), vec![0, 3]);
    assert_eq!(positions(Assertion::WordEnd), vec![2, 5]);

    let lines = "a\nb";
    assert!(satisfied(Assertion::StartLine, lines, 0));
    assert!(satisfied(Assertion::StartLine, lines, 2));
    assert!(!satisfied(Assertion::StartLine, lines, 1));
    assert!(satisfied(Assertion::EndLine, lines, 1));
    assert!(satisfied(Assertion::EndLine, lines, 3));
    assert!(!satisfied(Assertion::EndLine, lines, 2));

    let marked = "a\u{301}b c";
    let bounds: Vec<usize> = (0..=marked.len())
        .filter(|&at| marked.is_char_boundary(at) && satisfied(Assertion::WordBoundary, marked, at))
        .collect();
    assert_eq!(bounds, vec![0, 4, 5, 6]);
}

#[test]
fn a_cache_survives_being_resized_between_programs() {
    let small =
        compile(&parse("a", Flags::default()).expect("parses"), usize::MAX).expect("compiles");
    let large = compile(
        &parse("(?:abc){20}", Flags::default()).expect("parses"),
        usize::MAX,
    )
    .expect("compiles");
    let mut cache = Cache::new();
    let big = "abc".repeat(20);
    for _ in 0..50 {
        assert_eq!(
            search(&small, &mut cache, "zza", 0, false, &mut { u64::MAX }),
            Ok(Some((2, 3)))
        );
        assert_eq!(
            search(&large, &mut cache, &big, 0, false, &mut { u64::MAX }),
            Ok(Some((0, 60)))
        );
        assert_eq!(
            search(&small, &mut cache, "", 0, false, &mut { u64::MAX }),
            Ok(None)
        );
    }
}

#[test]
fn the_step_budget_stops_the_simulation() {
    let ast = parse("a+b", Flags::default()).expect("parses");
    let prog = compile(&ast, usize::MAX).expect("compiles");
    let text = "a".repeat(10_000);
    let mut cache = Cache::new();

    assert_eq!(
        search(&prog, &mut cache, &text, 0, false, &mut { u64::MAX }),
        Ok(None),
        "with budget to spare the answer is a completed no-match"
    );
    assert_eq!(
        search(&prog, &mut cache, &text, 0, false, &mut 100),
        Err(Exhausted),
        "and a hundred units cannot cross ten thousand positions"
    );
    let matching = format!("{}b", "a".repeat(10_000));
    assert_eq!(
        search(&prog, &mut cache, &matching, 0, false, &mut 100),
        Err(Exhausted)
    );
    assert_eq!(
        search(&prog, &mut cache, &matching, 0, false, &mut { u64::MAX }),
        Ok(Some((0, 10_001)))
    );
}

#[test]
fn the_prefilter_skip_is_charged_by_the_byte() {
    let ast = parse("needle", Flags::default()).expect("parses");
    let prog = compile(&ast, usize::MAX).expect("compiles");
    assert!(prog.first.is_some(), "this pattern arms the prefilter");
    let text = "x".repeat(100_000);
    let mut cache = Cache::new();
    assert_eq!(
        search(&prog, &mut cache, &text, 0, false, &mut 1_000),
        Err(Exhausted),
        "100,000 skipped bytes are 100,000 units of real work"
    );
    assert_eq!(
        search(&prog, &mut cache, &text, 0, false, &mut { u64::MAX }),
        Ok(None)
    );
}

#[test]
fn the_visited_generation_wraps_cleanly() {
    let mut list = ThreadList::default();
    list.resize(4);
    list.generation = u32::MAX;
    list.visited.fill(u32::MAX);
    list.begin();
    assert_eq!(list.generation, 1);
    assert!(
        !list.mark(0),
        "a wrapped generation must not report stale marks"
    );
    assert!(list.mark(0), "and must still deduplicate");
    assert!(!list.mark(3));
    assert!(
        list.mark(9),
        "an out-of-range pc is treated as already seen"
    );
}

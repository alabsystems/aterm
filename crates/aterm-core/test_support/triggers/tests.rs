// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for trigger evaluation engine.

use super::*;
use std::time::Duration;

#[test]
fn trigger_new_valid_pattern() {
    let trigger = Trigger::new(r"\d+", TriggerAction::Bell)
        .expect("valid regex pattern should create trigger");
    // Verify the trigger was created with the correct action and defaults
    assert_eq!(trigger.action, TriggerAction::Bell);
    assert!(trigger.enabled, "new trigger should be enabled by default");
}

#[test]
fn trigger_new_invalid_pattern() {
    let err = Trigger::new(r"[invalid", TriggerAction::Bell).unwrap_err();
    match err {
        TriggerError::InvalidPattern { pattern, .. } => assert_eq!(pattern, "[invalid"),
        other => panic!("Expected InvalidPattern, got: {:?}", other),
    }
}

#[test]
fn trigger_matches_simple() {
    let mut trigger = Trigger::new(r"error", TriggerAction::Bell).unwrap();
    let result = trigger.matches("An error occurred");
    let m = result.unwrap();
    assert_eq!(m.text, "error");
    assert_eq!(m.start, 3);
    assert_eq!(m.end, 8);
}

#[test]
fn trigger_matches_no_match() {
    let mut trigger = Trigger::new(r"error", TriggerAction::Bell).unwrap();
    let result = trigger.matches("All is well");
    assert!(
        result.is_none(),
        "non-error text should not match error trigger"
    );
}

#[test]
fn trigger_find_all() {
    let mut trigger = Trigger::new(r"\d+", TriggerAction::Bell).unwrap();
    let results = trigger.find_all("foo 123 bar 456 baz");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].text, "123");
    assert_eq!(results[1].text, "456");
}

#[test]
fn trigger_set_operations() {
    let mut set = TriggerSet::new();
    assert!(set.is_empty());

    let t1 = Trigger::new(r"error", TriggerAction::Bell).unwrap();
    let t2 = Trigger::new(r"warn", TriggerAction::Bell).unwrap();

    let idx0 = set.add_with_index(t1);
    let idx = set.add_with_index(t2);
    assert_eq!(idx0, 0);
    assert_eq!(idx, 1);
    assert_eq!(set.len(), 2);
    assert_eq!(set.iter().count(), 2);

    let t = set
        .get(0)
        .expect("index 0 should exist after adding 2 triggers");
    assert_eq!(t.pattern(), r"error");
    assert_eq!(t.action, TriggerAction::Bell);

    let t = set
        .get(1)
        .expect("index 1 should exist after adding 2 triggers");
    assert_eq!(t.pattern(), r"warn");
    assert!(
        set.get(2).is_none(),
        "index 2 should be out of bounds for 2-element set"
    );

    let removed = set.remove(0).expect("index 0 should be removable");
    assert_eq!(removed.pattern(), r"error");
    assert_eq!(set.len(), 1);
    assert_eq!(
        set.get(0)
            .expect("remaining trigger should shift to index 0")
            .pattern(),
        r"warn"
    );

    set.clear();
    assert!(set.is_empty());
}

#[test]
fn evaluator_basic() {
    let mut eval = TriggerEvaluator::new();
    eval.add_trigger(
        Trigger::new(
            r"error",
            TriggerAction::Highlight {
                foreground: Some([255, 0, 0]),
                background: None,
            },
        )
        .unwrap(),
    );

    let results = eval.evaluate("An error occurred", 1, false);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].trigger_index, 0);
    assert_eq!(results[0].match_info.text, "error");
    assert_eq!(
        results[0].action,
        TriggerAction::Highlight {
            foreground: Some([255, 0, 0]),
            background: None,
        }
    );
}

#[test]
fn evaluator_rate_limiting() {
    let mut eval = TriggerEvaluator::new();
    eval.set_rate_limit(Duration::from_secs(10)); // Long rate limit for test
    eval.add_trigger(Trigger::new(r"test", TriggerAction::Bell).unwrap());

    // First evaluation should work
    let results = eval.evaluate("test", 1, false);
    assert_eq!(results.len(), 1);

    // Second evaluation with same line_id should be rate limited
    let results = eval.evaluate("test", 1, false);
    assert!(results.is_empty());

    // Different line_id should work
    let results = eval.evaluate("test", 2, false);
    assert_eq!(results.len(), 1);
}

#[test]
fn evaluator_partial_line() {
    let mut eval = TriggerEvaluator::new();

    let mut t1 = Trigger::new(r"error", TriggerAction::Bell).unwrap();
    t1.partial_line = true;
    eval.add_trigger(t1);

    let mut t2 = Trigger::new(r"warn", TriggerAction::Bell).unwrap();
    t2.partial_line = false;
    eval.add_trigger(t2);

    // Partial line: only t1 should match
    let results = eval.evaluate("error warn", 1, true);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].trigger_index, 0);
    assert_eq!(results[0].match_info.text, "error");
    assert_eq!(results[0].action, TriggerAction::Bell);

    // Complete line: both should match
    eval.clear_rate_limit_state();
    let results = eval.evaluate("error warn", 2, false);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].trigger_index, 0);
    assert_eq!(results[1].trigger_index, 1);
    assert_eq!(results[0].action, TriggerAction::Bell);
    assert_eq!(results[1].action, TriggerAction::Bell);
}

#[test]
fn evaluator_disabled_trigger() {
    let mut eval = TriggerEvaluator::new();

    let mut trigger = Trigger::new(r"test", TriggerAction::Bell).unwrap();
    trigger.enabled = false;
    eval.add_trigger(trigger);

    let results = eval.evaluate("test", 1, false);
    assert!(results.is_empty());
}

#[test]
fn builtin_patterns() {
    // URL pattern
    let mut trigger = Trigger::new(patterns::URL, TriggerAction::Bell).unwrap();
    let m = trigger
        .matches("Visit https://example.com/path?query=1")
        .unwrap();
    assert_eq!(m.text, "https://example.com/path?query=1");

    // Email pattern
    let mut trigger = Trigger::new(patterns::EMAIL, TriggerAction::Bell).unwrap();
    let m = trigger
        .matches("Contact test@example.com for help")
        .unwrap();
    assert_eq!(m.text, "test@example.com");

    // Error keywords
    let mut trigger = Trigger::new(patterns::ERROR_KEYWORDS, TriggerAction::Bell).unwrap();
    let m = trigger.matches("An ERROR occurred").unwrap();
    assert_eq!(m.text, "ERROR");
    let m = trigger.matches("FATAL: disk full").unwrap();
    assert_eq!(m.text, "FATAL");
    assert!(
        trigger.matches("All is well").is_none(),
        "benign text should not match error keywords"
    );
}

#[test]
fn post_process_trailing_punctuation() {
    assert_eq!(
        post_process_match("https://example.com."),
        "https://example.com"
    );
    assert_eq!(post_process_match("path/to/file,"), "path/to/file");
    assert_eq!(post_process_match("word;"), "word");
}

#[test]
fn post_process_balanced_brackets() {
    // Balanced - don't remove
    assert_eq!(post_process_match("func(arg)"), "func(arg)");
    assert_eq!(post_process_match("array[0]"), "array[0]");

    // Unbalanced - remove
    assert_eq!(post_process_match("word)"), "word");
    assert_eq!(post_process_match("url]"), "url");
}

#[test]
fn trigger_action_clone() {
    let action = TriggerAction::Alert {
        title: "Test".to_string(),
        message: "Message".to_string(),
    };
    let cloned = action.clone();
    assert_eq!(action, cloned);
}

#[test]
fn trigger_with_builders() {
    let trigger = Trigger::new(r"test", TriggerAction::Bell)
        .unwrap()
        .with_name("test_trigger")
        .with_partial_line(true)
        .with_idempotent(false);

    assert_eq!(trigger.name, "test_trigger");
    assert!(trigger.partial_line);
    assert!(!trigger.idempotent);
}

#[test]
fn evaluator_evaluate_lines() {
    let mut eval = TriggerEvaluator::new();
    eval.add_trigger(Trigger::new(r"error", TriggerAction::Bell).unwrap());

    let lines = vec![
        (1, "line one", false),
        (2, "error here", false),
        (3, "line three", false),
    ];

    let results = eval.evaluate_lines(&lines);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 2);
    assert_eq!(results[0].1.len(), 1);
    assert_eq!(results[0].1[0].trigger_index, 0);
    assert_eq!(results[0].1[0].action, TriggerAction::Bell);
}

#[test]
fn evaluator_with_triggers_and_line_id_allocator() {
    let mut set = TriggerSet::new();
    set.add(Trigger::new(r"error", TriggerAction::Bell).unwrap());

    let mut eval = TriggerEvaluator::with_triggers(set);
    assert_eq!(eval.triggers().len(), 1);

    let results = eval.evaluate("error", 42, false);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].trigger_index, 0);
    assert_eq!(results[0].action, TriggerAction::Bell);

    assert_eq!(eval.allocate_line_id(), 0);
    assert_eq!(eval.allocate_line_id(), 1);

    eval.next_line_id = u64::MAX;
    assert_eq!(eval.allocate_line_id(), u64::MAX);
    assert_eq!(eval.allocate_line_id(), 0);
}

#[test]
fn trigger_error_display() {
    let err = TriggerError::InvalidPattern {
        pattern: "[bad".to_string(),
        reason: "unclosed bracket".to_string(),
    };
    let msg = err.to_string();
    assert!(msg.contains("[bad"));
    assert!(msg.contains("unclosed bracket"));

    let err = TriggerError::MissingPattern;
    assert!(err.to_string().contains("pattern"));

    let err = TriggerError::MissingAction;
    assert!(err.to_string().contains("action"));
}

#[test]
fn trigger_builder_basic() {
    let trigger = TriggerBuilder::new()
        .pattern(r"\d+")
        .action(TriggerAction::Bell)
        .build();
    let t = trigger.expect("valid trigger should build successfully");
    assert_eq!(t.pattern(), r"\d+");
    assert!(t.enabled);
    assert!(t.idempotent);
    assert!(!t.partial_line);
}

#[test]
fn trigger_builder_all_options() {
    let trigger = TriggerBuilder::new()
        .pattern(r"error")
        .action(TriggerAction::Bell)
        .name("error_alert")
        .partial_line(true)
        .idempotent(false)
        .enabled(false)
        .build()
        .unwrap();

    assert_eq!(trigger.name, "error_alert");
    assert_eq!(trigger.pattern(), "error");
    assert!(trigger.partial_line);
    assert!(!trigger.idempotent);
    assert!(!trigger.enabled);
}

#[test]
fn trigger_builder_missing_pattern() {
    let result = TriggerBuilder::new().action(TriggerAction::Bell).build();
    assert!(matches!(result, Err(TriggerError::MissingPattern)));
}

#[test]
fn trigger_builder_missing_action() {
    let result = TriggerBuilder::new().pattern(r"test").build();
    assert!(matches!(result, Err(TriggerError::MissingAction)));
}

#[test]
fn trigger_builder_invalid_pattern() {
    let result = TriggerBuilder::new()
        .pattern(r"[invalid")
        .action(TriggerAction::Bell)
        .build();
    assert!(matches!(result, Err(TriggerError::InvalidPattern { .. })));
}

#[test]
fn trigger_builder_default() {
    let builder = TriggerBuilder::default();
    // Default builder has no pattern or action, so build should fail
    assert_eq!(builder.build().unwrap_err(), TriggerError::MissingPattern);
}

#[test]
fn trigger_builder_clone() {
    let builder = TriggerBuilder::new()
        .pattern(r"test")
        .action(TriggerAction::Bell)
        .name("cloned");

    let cloned = builder.clone();
    let t1 = builder.build().unwrap();
    let t2 = cloned.build().unwrap();
    assert_eq!(t1.name, t2.name);
    assert_eq!(t1.pattern(), t2.pattern());
}

/// Verify that `post_process_match` does not exhibit O(n^2) behavior.
///
/// The current implementation calls `.matches('(').count()` (and other bracket
/// variants) inside a while loop that strips one trailing character per
/// iteration. Each `.matches().count()` is O(n), and the loop can run O(n)
/// times for input consisting entirely of trailing punctuation, giving O(n^2)
/// worst-case behavior.
///
/// This test constructs worst-case input (all closing brackets) at two sizes
/// and checks that the scaling ratio stays below the quadratic threshold.
/// A 5x input increase should give ~5x time for O(n) but ~25x for O(n^2).
///
/// Bug: triggers/mod.rs:774-802 — see `balance_delimiter_end` in
/// `perception/detect/url.rs:436-449` for the correct O(n) depth-tracking
/// approach.
#[test]
fn post_process_match_scaling() {
    // Worst case: unbalanced closing parens are trailing punctuation,
    // and each iteration calls .matches('(').count() + .matches(')').count()
    // on the remaining string.
    let small_input: String = "x".repeat(200) + &")".repeat(200);
    let large_input: String = "x".repeat(1_000) + &")".repeat(1_000);

    // Verify correctness first — the function must strip trailing unbalanced
    // parens, not no-op.  A regression that short-circuits to `return text`
    // would make timing meaningless.
    let small_result = post_process_match(&small_input);
    assert_eq!(
        small_result,
        "x".repeat(200),
        "post_process_match should strip trailing unbalanced ')'"
    );

    // Warm up
    let _ = post_process_match(&large_input);

    let iters = 50;
    let start = std::time::Instant::now();
    for _ in 0..iters {
        let _ = post_process_match(&small_input);
    }
    let small_dur = start.elapsed();

    let start = std::time::Instant::now();
    for _ in 0..iters {
        let _ = post_process_match(&large_input);
    }
    let large_dur = start.elapsed();

    assert!(
        small_dur.as_nanos() > 0,
        "small input measurement was zero at 400 chars × {iters} iterations"
    );

    let ratio = large_dur.as_nanos() as f64 / small_dur.as_nanos() as f64;

    // 5x input increase:
    //   O(n)   → ~5x ratio
    //   O(n^2) → ~25x ratio
    // Threshold of 10x catches quadratic behavior with margin for CI variance.
    assert!(
        ratio < 10.0,
        "post_process_match scaling ratio {ratio:.1}x for 5x input suggests \
         O(n^2) behavior (400 chars: {small_dur:?}, 2000 chars: {large_dur:?}). \
         Fix: replace .matches().count() bracket-balance loop with single-pass \
         depth tracking (see balance_delimiter_end in perception/detect/url.rs)."
    );
}

/// Verify post_process_match correctness on balanced nested brackets.
#[test]
fn post_process_match_nested_balanced_brackets() {
    // Nested balanced brackets should be preserved
    assert_eq!(post_process_match("func(a(b))"), "func(a(b))");
    assert_eq!(post_process_match("a[b[c]]"), "a[b[c]]");
    assert_eq!(post_process_match("a{b{c}}"), "a{b{c}}");

    // Mixed balanced brackets
    assert_eq!(post_process_match("func(a[0])"), "func(a[0])");
}

/// Verify post_process_match strips all trailing punctuation correctly.
#[test]
fn post_process_match_multiple_trailing_punctuation() {
    assert_eq!(post_process_match("url.,;"), "url");
    assert_eq!(post_process_match("word..."), "word");
    assert_eq!(post_process_match("text?!"), "text");
}

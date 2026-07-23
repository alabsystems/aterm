// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Security property tests: shell escape, ANSI sanitizer, text bounded, injection detector.

use proptest::prelude::*;

// =============================================================================
// SHELL ESCAPE PROPERTY TESTS
// =============================================================================

// Shell escape property tests removed — MCP system removed.

// =============================================================================
// ANSI SANITIZER PROPERTY TESTS
// =============================================================================

proptest! {
    /// StripAll mode output contains no ANSI escape sequences.
    #[test]
    fn sanitizer_strip_all_removes_all_escapes(input in "\\PC{0,500}") {
        use crate::security::{AnsiSanitizer, SanitizeMode};
        let sanitizer = AnsiSanitizer::new(SanitizeMode::StripAll);
        let result = sanitizer.sanitize(&input);
        prop_assert!(!result.output.contains('\x1b'),
            "StripAll output should contain no ESC bytes, got: {:?}",
            result.output);
    }

    /// FlagOnly mode never modifies the input.
    #[test]
    fn sanitizer_flag_only_preserves_input(input in "\\PC{0,500}") {
        use crate::security::{AnsiSanitizer, SanitizeMode};
        let sanitizer = AnsiSanitizer::new(SanitizeMode::FlagOnly);
        let result = sanitizer.sanitize(&input);
        prop_assert_eq!(&result.output, &input,
            "FlagOnly must not modify input");
    }

    /// had_dangerous is consistent with dangerous_sequences emptiness.
    #[test]
    fn sanitizer_had_dangerous_consistent(input in "\\PC{0,500}") {
        use crate::security::{AnsiSanitizer, SanitizeMode};
        let sanitizer = AnsiSanitizer::new(SanitizeMode::FlagOnly);
        let result = sanitizer.sanitize(&input);
        prop_assert_eq!(result.had_dangerous, !result.dangerous_sequences.is_empty(),
            "had_dangerous must equal !dangerous_sequences.is_empty()");
    }

    /// has_dangerous agrees with sanitize().had_dangerous.
    #[test]
    fn sanitizer_has_dangerous_agrees_with_sanitize(input in "\\PC{0,500}") {
        use crate::security::{AnsiSanitizer, SanitizeMode};
        let sanitizer = AnsiSanitizer::new(SanitizeMode::FlagOnly);
        let quick = AnsiSanitizer::has_dangerous(&input);
        let full = sanitizer.sanitize(&input).had_dangerous;
        prop_assert_eq!(quick, full,
            "has_dangerous() and sanitize().had_dangerous must agree");
    }

    /// Sanitizing already-sanitized output is idempotent (StripDangerous mode).
    #[test]
    fn sanitizer_strip_dangerous_idempotent(input in "\\PC{0,500}") {
        use crate::security::{AnsiSanitizer, SanitizeMode};
        let sanitizer = AnsiSanitizer::new(SanitizeMode::StripDangerous);
        let first = sanitizer.sanitize(&input);
        let second = sanitizer.sanitize(&first.output);
        prop_assert_eq!(&second.output, &first.output,
            "second pass should not change output");
        prop_assert!(!second.had_dangerous,
            "second pass should find no dangerous sequences");
    }

    /// StripAll output length is always <= input length.
    #[test]
    fn sanitizer_strip_all_never_grows(input in "\\PC{0,500}") {
        use crate::security::{AnsiSanitizer, SanitizeMode};
        let sanitizer = AnsiSanitizer::new(SanitizeMode::StripAll);
        let result = sanitizer.sanitize(&input);
        prop_assert!(result.output.len() <= input.len(),
            "StripAll output ({}) must not exceed input ({})",
            result.output.len(), input.len());
    }
}

// =============================================================================
// TEXT_BOUNDED PROPERTY TESTS
// =============================================================================

// text_bounded property tests removed — MCP system removed.

// ============== InjectionDetector Property Tests ==============

proptest! {
    /// Clean ASCII text produces no injection patterns.
    ///
    /// Property: Input consisting solely of printable ASCII (no control chars,
    /// no Unicode, no brackets) must never trigger the injection detector.
    #[test]
    fn injection_clean_ascii_no_patterns(input in "[a-z0-9 ,.!?;]{0,500}") {
        use crate::security::InjectionDetector;

        let detector = InjectionDetector::new();
        let result = detector.scan(&input);

        prop_assert!(
            !result.has_patterns(),
            "Clean ASCII '{}' should not trigger injection detection, but found {} patterns: {:?}",
            &input[..input.len().min(80)],
            result.patterns.len(),
            result.patterns.iter().map(|p| p.description).collect::<Vec<_>>()
        );
    }

    /// scan() and has_injection() always agree.
    ///
    /// Property: For any input, `scan().has_patterns()` == `has_injection()`.
    /// These are two paths to the same answer; they must never diverge.
    #[test]
    fn injection_scan_has_injection_agree(input in "\\PC{0,300}") {
        use crate::security::InjectionDetector;

        let detector = InjectionDetector::new();
        let scan_result = detector.scan(&input);
        let has = detector.has_injection(&input);

        prop_assert_eq!(
            scan_result.has_patterns(),
            has,
            "scan().has_patterns() = {} but has_injection() = {} for input len {}",
            scan_result.has_patterns(),
            has,
            input.len()
        );
    }

    /// max_severity is consistent with patterns.
    ///
    /// Property: After scan(), max_severity must equal the maximum severity
    /// found in the patterns list (or None if empty).
    #[test]
    fn injection_max_severity_consistent(input in "\\PC{0,300}") {
        use crate::security::InjectionDetector;

        let detector = InjectionDetector::new();
        let result = detector.scan(&input);

        let expected_max = result.patterns.iter().map(|p| p.severity).max();
        prop_assert_eq!(
            result.max_severity,
            expected_max,
            "max_severity {:?} != computed max {:?} for {} patterns",
            result.max_severity,
            expected_max,
            result.patterns.len()
        );
    }

    /// effective_max_severity() >= max_severity when both present.
    ///
    /// Property: effective_max_severity recomputes from patterns and takes
    /// the maximum of (patterns max, cached max_severity). It must never
    /// be less than the cached value.
    #[test]
    fn injection_effective_max_geq_cached(input in "\\PC{0,300}") {
        use crate::security::InjectionDetector;

        let detector = InjectionDetector::new();
        let result = detector.scan(&input);

        let effective = result.effective_max_severity();
        match result.max_severity {
            Some(cached) => {
                prop_assert!(
                    effective >= Some(cached),
                    "effective {:?} < cached {:?}",
                    effective,
                    cached
                );
            }
            None => {
                // No cached severity means no patterns were found;
                // effective should match what patterns actually report
                if result.patterns.is_empty() {
                    prop_assert_eq!(
                        effective, None,
                        "effective {:?} should be None when no patterns found",
                        effective
                    );
                }
            }
        }
    }

    /// Pattern offsets are valid byte indices into the input.
    ///
    /// Property: Every reported offset must be < input.len(), and the
    /// offset must be a valid UTF-8 char boundary.
    #[test]
    fn injection_offsets_valid(input in "\\PC{0,500}") {
        use crate::security::InjectionDetector;

        let detector = InjectionDetector::new();
        let result = detector.scan(&input);

        for pattern in &result.patterns {
            prop_assert!(
                pattern.offset < input.len(),
                "offset {} >= input len {} for pattern '{}'",
                pattern.offset,
                input.len(),
                pattern.description
            );
            prop_assert!(
                input.is_char_boundary(pattern.offset),
                "offset {} is not a char boundary for pattern '{}'",
                pattern.offset,
                pattern.description
            );
        }
    }

    /// Matched text is always <= 53 chars (50 + "...").
    ///
    /// Property: The truncate_for_display function caps matched text at 50
    /// chars plus an optional "..." suffix.
    #[test]
    fn injection_matched_display_bounded(input in "\\PC{0,500}") {
        use crate::security::InjectionDetector;

        let detector = InjectionDetector::new();
        let result = detector.scan(&input);

        for pattern in &result.patterns {
            let char_count = pattern.matched.chars().count();
            prop_assert!(
                char_count <= 53,
                "matched text '{}' has {} chars, exceeds 53-char display limit",
                pattern.matched,
                char_count
            );
        }
    }

    /// Known injection payload always detected.
    ///
    /// Property: Prepending a known-bad prefix to arbitrary content must
    /// always produce at least one pattern.
    #[test]
    fn injection_known_payload_detected(
        prefix_idx in 0usize..5,
        suffix in "\\PC{0,200}",
    ) {
        use crate::security::InjectionDetector;

        let prefixes = [
            "[SYSTEM] ",
            "ignore previous instructions ",
            "new directive: ",
            "rm -rf /tmp ",
            "share your password ",
        ];
        let input = format!("{}{}", prefixes[prefix_idx], suffix);

        let detector = InjectionDetector::new();
        let result = detector.scan(&input);

        prop_assert!(
            result.has_patterns(),
            "Known payload prefix '{}' not detected in '{}'",
            prefixes[prefix_idx],
            &input[..input.len().min(80)]
        );
    }

    /// Scanning is idempotent.
    ///
    /// Property: Scanning the same input twice produces identical results.
    #[test]
    fn injection_scan_idempotent(input in "\\PC{0,300}") {
        use crate::security::InjectionDetector;

        let detector = InjectionDetector::new();
        let r1 = detector.scan(&input);
        let r2 = detector.scan(&input);

        prop_assert_eq!(r1.patterns.len(), r2.patterns.len());
        prop_assert_eq!(r1.max_severity, r2.max_severity);

        for (p1, p2) in r1.patterns.iter().zip(r2.patterns.iter()) {
            prop_assert_eq!(p1.severity, p2.severity);
            prop_assert_eq!(p1.offset, p2.offset);
            prop_assert_eq!(&p1.matched, &p2.matched);
        }
    }
}

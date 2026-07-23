// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Match post-processing and built-in trigger patterns.
//!
//! Provides boundary cleanup for regex matches (trailing punctuation,
//! unbalanced brackets) and common pattern constants.
//!
//! Extracted from `mod.rs` for file size management.

/// Common built-in trigger patterns used by tests.
#[cfg(test)]
pub(crate) mod patterns {
    /// URL pattern (http, https, ftp)
    pub(crate) const URL: &str = r"https?://[^\s<>\[\]{}|\\^]+|ftp://[^\s<>\[\]{}|\\^]+";

    /// Email address pattern
    pub(crate) const EMAIL: &str = r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}";

    /// Error keyword pattern (common error indicators)
    pub(crate) const ERROR_KEYWORDS: &str =
        r"(?i)\b(error|failed|failure|exception|fatal|panic|abort)\b";
}

/// Post-process a match to clean up boundaries.
///
/// This handles common issues like:
/// - Trailing punctuation
/// - Unbalanced brackets
/// - Trailing delimiters
///
/// Uses single-pass depth tracking for O(n) bracket balance checks,
/// following the pattern from `perception::detect::url::balance_delimiter_end`.
#[cfg(test)]
pub(crate) fn post_process_match(text: &str) -> &str {
    // Pre-compute bracket balance in one O(n) pass.
    // Excess = opens - closes. If excess >= 0, brackets are balanced or
    // have more opens than closes, so the trailing closer is meaningful.
    let mut paren_excess: i32 = 0;
    let mut bracket_excess: i32 = 0;
    let mut brace_excess: i32 = 0;
    for c in text.chars() {
        match c {
            '(' => paren_excess += 1,
            ')' => paren_excess -= 1,
            '[' => bracket_excess += 1,
            ']' => bracket_excess -= 1,
            '{' => brace_excess += 1,
            '}' => brace_excess -= 1,
            _ => {}
        }
    }

    let mut result = text;
    let trailing = &['.', ',', ':', ';', '?', '!', ')', ']', '}', '>', '\'', '"'];
    while !result.is_empty() {
        let last = result
            .chars()
            .last()
            .expect("invariant: checked !is_empty()");
        if trailing.contains(&last) {
            // If brackets are balanced (excess >= 0), keep the trailing closer
            if last == ')' && paren_excess >= 0 {
                break;
            }
            if last == ']' && bracket_excess >= 0 {
                break;
            }
            if last == '}' && brace_excess >= 0 {
                break;
            }
            // Removing a closing bracket restores one unit of balance
            match last {
                ')' => paren_excess += 1,
                ']' => bracket_excess += 1,
                '}' => brace_excess += 1,
                _ => {}
            }
            result = &result[..result.len() - last.len_utf8()];
        } else {
            break;
        }
    }

    result
}

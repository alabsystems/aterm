// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `[fabric] command` parsing for `aterm-gui` and `aterm-link`: either a
//! whitespace-split command line or a literal argv array. A blank line or
//! empty array turns the fabric off; empty or NUL argv elements are refused.

/// Expected TOML type description for `[fabric] command`.
pub const EXPECTED: &str = "a command line (one string, split on whitespace) or an argv (an array \
                            of strings, one word each)";

/// Splits `line` on whitespace with no quoting or shell expansion.
#[must_use]
pub fn split_line(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// Validates argv element `word` at 0-based `index`.
///
/// # Errors
///
/// An empty element, or one that holds a NUL.
pub fn check_word(index: usize, word: &str) -> Result<(), String> {
    let n = index.saturating_add(1);
    if word.is_empty() {
        return Err(format!(
            "`[fabric] command` element {n} is empty: each element is one argv word, passed as \
             written, and an empty word is never meant"
        ));
    }
    if word.contains('\0') {
        return Err(format!(
            "`[fabric] command` element {n} holds a NUL byte, which no argv word can carry"
        ));
    }
    Ok(())
}

/// Returns whether `argv` is non-empty and every word is non-empty and whitespace-free.
#[must_use]
pub fn line_spells(argv: &[String]) -> bool {
    !argv.is_empty()
        && argv
            .iter()
            .all(|w| !w.is_empty() && !w.chars().any(char::is_whitespace))
}

#[cfg(test)]
#[path = "fabric_command_tests.rs"]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A token named in an environment-style file (`NAME=value`, `export NAME=value`, the value
//! bare or in one pair of quotes), read without running anything: the one reader `aterm init`'s
//! `[github] token_key` and `aterm secrets git-credential --token-key` share.

/// Why `key` names no token in a file's text ([`assigned`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unassigned {
    /// No line assigns it.
    Missing,
    /// More than one line does.
    Repeated,
}

/// Whether `key` is an environment assignment name: a letter or `_`, then letters, digits and
/// `_`.
#[must_use]
pub fn assignable(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The value the one line of `text` that assigns `key` holds, trimmed and with one pair of
/// enclosing quotes (`'` or `"`) taken off. Blank lines, `#` comments and lines with no `=` are
/// skipped; an `export ` prefix is read past. Nothing is expanded or run.
///
/// # Errors
/// [`Unassigned::Missing`] when no line assigns `key`, [`Unassigned::Repeated`] when more than
/// one does.
pub fn assigned<'a>(text: &'a str, key: &str) -> Result<&'a str, Unassigned> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        if found.is_some() {
            return Err(Unassigned::Repeated);
        }
        let value = value.trim();
        found = Some(
            value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .or_else(|| value.strip_prefix('"').and_then(|v| v.strip_suffix('"')))
                .unwrap_or(value),
        );
    }
    found.ok_or(Unassigned::Missing)
}

#[cfg(test)]
#[path = "token_line_tests.rs"]
mod tests;

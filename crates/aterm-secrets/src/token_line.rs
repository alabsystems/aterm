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
mod tests {
    use super::*;

    /// A name's one assignment is read bare, quoted or exported; a comment, a line with no `=`
    /// and another name are skipped. Negative controls: a name assigned twice, or nowhere.
    #[test]
    fn a_name_reads_its_one_assignment_and_nothing_runs() {
        let text = "# GH_TOKEN=commented\nOTHER=x\nnot an assignment\nexport GH_TOKEN = 'abc_1'\n";
        assert_eq!(assigned(text, "GH_TOKEN"), Ok("abc_1"));
        assert_eq!(assigned("GH_TOKEN=\"$(run)\"", "GH_TOKEN"), Ok("$(run)"));
        assert_eq!(assigned("GH_TOKEN=bare", "GH_TOKEN"), Ok("bare"));
        assert_eq!(
            assigned("GH_TOKEN=a\nGH_TOKEN=b\n", "GH_TOKEN"),
            Err(Unassigned::Repeated)
        );
        assert_eq!(assigned("OTHER=a\n", "GH_TOKEN"), Err(Unassigned::Missing));
        assert!(assignable("GH_TOKEN") && assignable("_x9"));
        for refused in ["", "9X", "GH-TOKEN", "GH TOKEN"] {
            assert!(!assignable(refused), "{refused}");
        }
    }
}

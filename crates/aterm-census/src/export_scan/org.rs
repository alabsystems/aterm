// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The engine's org rewrite (`org_rewrite` in its `bin/pub`), which runs after the export's
//! transforms: `<private org>/<registry repo>` in any letter case becomes
//! `alabsystems/<repo>`. What it leaves is what the private-reference guard then reads, so the
//! scan rewrites exactly as the engine does: the whole file decoded as UTF-8 (one that is not
//! stays as it is), a lockfile or a file over [`BIG`] left alone, and in a `.toml` file each
//! non-comment line that pins a git dependency ([`pins`]) left alone.

use aterm_regex::Regex;

/// The size past which the engine leaves a file as it is.
const BIG: u64 = 5_000_000;

/// The lockfiles of other ecosystems the engine leaves to its ledger, besides every `*.lock`.
const LOCKFILES: [&str; 3] = ["package-lock.json", "npm-shrinkwrap.json", "pnpm-lock.yaml"];

/// What the org becomes.
const PUBLIC: &str = "alabsystems";

/// The engine's org rewrite for one org and the registry's repository names.
pub(super) struct Rewrite {
    /// `<org>/`, ASCII lower case.
    prefix_str: String,
    /// `<org>/`, ASCII lower case, as chars.
    prefix: Vec<char>,
    /// The registry's repository names, longest first (the engine's alternation order).
    names: Vec<Vec<char>>,
    /// The engine's `ORG_SKIP_LINE`.
    pin: Regex,
}

/// Python's `\w` (a Unicode word character) or `-`: what may not touch either end of a match.
fn joins(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

/// Python's `Path.suffix`: the last `.`-part of `name`, not counting a leading dot.
fn suffix(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 && i < name.len() - 1 => &name[i..],
        _ => "",
    }
}

/// Python's `str.splitlines(keepends=True)`.
fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = match c {
            '\r' if chars.peek().is_some_and(|&(_, n)| n == '\n') => {
                chars.next();
                i + 2
            }
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}'
            | '\u{2028}' | '\u{2029}' => i + c.len_utf8(),
            _ => continue,
        };
        lines.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python's `str.isspace`, which `lstrip` strips: Rust's whitespace and the separators
/// U+001C..U+001F.
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

impl Rewrite {
    pub(super) fn new(org: &str, names: &[String]) -> Self {
        let mut names: Vec<Vec<char>> = names
            .iter()
            .filter(|n| !n.is_empty())
            .map(|n| n.chars().collect())
            .collect();
        names.sort_by_key(|n| std::cmp::Reverse(n.len()));
        let prefix_str = format!("{}/", org.to_ascii_lowercase());
        let prefix = prefix_str.chars().collect();
        Self {
            prefix_str,
            prefix,
            names,
            pin: Regex::new(r#"git\s*=\s*['"]|['"]git\+|^\s*\[patch\."#)
                .expect("the engine's ORG_SKIP_LINE compiles"),
        }
    }

    /// `bytes` of the exported file `rel` (`size` bytes on disk) as the engine's org rewrite
    /// leaves it, or `None` when it changes nothing.
    pub(super) fn file(&self, rel: &str, bytes: &[u8], size: u64) -> Option<Vec<u8>> {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if size > BIG || suffix(name) == ".lock" || LOCKFILES.contains(&name) {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let has_prefix = if self.prefix_str.is_ascii() {
            super::find_ignore_ascii_case(text.as_bytes(), self.prefix_str.as_bytes()).is_some()
        } else {
            text.to_lowercase().contains(&self.prefix_str)
        };
        if !has_prefix {
            return None;
        }
        let manifest = suffix(name) == ".toml";
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        for line in split_lines(text) {
            if manifest
                && !line.trim_start_matches(python_space).starts_with('#')
                && self.pin.is_match(line)
            {
                out.push_str(line);
                continue;
            }
            changed |= self.line(line, &mut out);
        }
        changed.then(|| out.into_bytes())
    }

    /// `line` rewritten onto `out`; whether anything was.
    fn line(&self, line: &str, out: &mut String) -> bool {
        let chars: Vec<char> = line.chars().collect();
        let mut changed = false;
        let mut i = 0;
        while i < chars.len() {
            if let Some(len) = self.at(&chars, i) {
                out.push_str(PUBLIC);
                out.push('/');
                out.extend(&chars[i + self.prefix.len()..i + len]);
                i += len;
                changed = true;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        changed
    }

    /// The length of the engine's `ORG_PAT` match starting at `chars[i]`, if one does.
    fn at(&self, chars: &[char], i: usize) -> Option<usize> {
        let before = i.checked_sub(1).map(|j| chars[j]);
        let shell_default = i >= 2 && chars[i - 2] == ':' && chars[i - 1] == '-';
        if joins(before) && !shell_default {
            return None;
        }
        let folds = |at: usize, want: &[char]| {
            chars.get(at..at + want.len()).is_some_and(|have| {
                have.iter()
                    .zip(want)
                    .all(|(h, w)| h.eq_ignore_ascii_case(w))
            })
        };
        if !folds(i, &self.prefix) {
            return None;
        }
        let from = i + self.prefix.len();
        self.names
            .iter()
            .find(|name| folds(from, name) && !joins(chars.get(from + name.len()).copied()))
            .map(|name| self.prefix.len() + name.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: &str = "privorg";

    fn rewrite(rel: &str, text: &str) -> String {
        let names = ["ay".to_string(), "aterm".to_string(), "aterm-x".to_string()];
        let bytes = text.as_bytes();
        Rewrite::new(ORG, &names)
            .file(rel, bytes, bytes.len() as u64)
            .map_or_else(
                || text.to_string(),
                |b| String::from_utf8(b).expect("utf-8"),
            )
    }

    #[test]
    fn it_rewrites_a_registry_repo_in_any_case_and_keeps_the_names_case() {
        assert_eq!(
            rewrite("a.md", "see github.com/PrivOrg/Aterm and privorg/ay.\r\n"),
            "see github.com/alabsystems/Aterm and alabsystems/ay.\r\n"
        );
        assert_eq!(rewrite("a.md", "privorg/aterm-x"), "alabsystems/aterm-x");
        assert_eq!(rewrite("a.md", "${2:-privorg/ay}"), "${2:-alabsystems/ay}");
    }

    #[test]
    fn it_leaves_what_the_engines_pattern_does_not_match() {
        for text in [
            "gh-privorg/ay",
            "xprivorg/ay",
            "privorg/aterm-y",
            "privorg/ayx",
            "privorg/other",
        ] {
            assert_eq!(rewrite("a.md", text), text);
        }
    }

    #[test]
    fn it_leaves_pins_lockfiles_and_text_that_is_not_utf8() {
        let pin = "dep = { git = \"https://github.com/privorg/ay\" }\n# privorg/ay\n";
        assert_eq!(
            rewrite("Cargo.toml", pin),
            "dep = { git = \"https://github.com/privorg/ay\" }\n# alabsystems/ay\n"
        );
        assert_eq!(rewrite("Cargo.lock", "privorg/ay"), "privorg/ay");
        assert_eq!(rewrite("package-lock.json", "privorg/ay"), "privorg/ay");
        assert_eq!(rewrite("a.md", "privorg/ay"), "alabsystems/ay");
        let names = ["ay".to_string()];
        let latin1 = b"privorg/ay \xff";
        assert_eq!(Rewrite::new(ORG, &names).file("a.md", latin1, 12), None);
        assert_eq!(
            Rewrite::new(ORG, &names).file("a.md", b"privorg/ay", BIG + 1),
            None
        );
    }

    #[test]
    fn lines_split_as_pythons_splitlines() {
        assert_eq!(
            split_lines("a\r\nb\rc\u{2028}d\u{b}e"),
            ["a\r\n", "b\r", "c\u{2028}", "d\u{b}", "e"]
        );
        assert_eq!(suffix(".lock"), "");
        assert_eq!(suffix("x.lock"), ".lock");
    }
}

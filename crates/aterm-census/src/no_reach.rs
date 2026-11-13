// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! THE NO-REACH CLASS (`docs/DESIGN-landing-economy.md` §2.1): the repository Markdown no build,
//! include or test reads, so only the source readers (fmt, census and export-scan) judge it.
//! `verify land` composes a moved main whose only shared paths are in it after rerunning those
//! three on the merge, and census's `no-reach-read` guard holds that no member reads one.
//!
//! A path is in the class when it is one of:
//! - **root Markdown**: a `*.md` file at the repository root, but `CHANGELOG.md`, which a test
//!   reads until changelog fragments (§2.1b) replace it ([`EXCLUDED`]);
//! - **docs Markdown**: `docs/**/*.md` with no `Cargo.toml` in `docs/` or any directory between it
//!   and the file, so no package (member, fork or path dependency) owns it;
//! - **a member's spec table**: `<dir>/SPECS.md` where `<dir>/Cargo.toml` exists and `<dir>` is a
//!   workspace member: it matches the root manifest's `[workspace] members` and is in no
//!   `exclude` entry.
//!
//! Every segment must be a plain name (no empty, `.` or `..` segment). The class reads only path
//! names and the root manifest, so it is the same question on a checkout, on a tree Git holds
//! and in history; the entry's mode (a symlink, a gitlink) is the caller's to read.

use std::collections::BTreeSet;

/// The root Markdown file the class leaves out: a test reads it (§2.1b removes the reader).
pub const EXCLUDED: &str = "CHANGELOG.md";

/// What the class reads of a tree besides the path itself: the workspace's member patterns and
/// exclusions, and every directory that holds a `Cargo.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// `[workspace] members`, or empty where the root manifest does not read whole.
    members: Vec<String>,
    /// `[workspace] exclude`.
    exclude: Vec<String>,
    /// Each directory that holds a `Cargo.toml` (`""` for the root).
    manifests: BTreeSet<String>,
}

impl Layout {
    /// The layout of a tree whose root `Cargo.toml` reads `root_manifest` (`None`: none, or
    /// unreadable) and whose files are `paths` (root-relative, `/`-separated). A root manifest
    /// whose `[workspace]` arrays this cannot read exactly has no members, so no spec table is in
    /// the class: it fails closed.
    #[must_use]
    pub fn new<'a>(root_manifest: Option<&str>, paths: impl IntoIterator<Item = &'a str>) -> Self {
        let manifests = paths
            .into_iter()
            .filter_map(|path| {
                if path == "Cargo.toml" {
                    Some(String::new())
                } else {
                    path.strip_suffix("/Cargo.toml").map(str::to_owned)
                }
            })
            .collect();
        let (members, exclude) = root_manifest.and_then(workspace_arrays).unwrap_or_default();
        Self {
            members,
            exclude,
            manifests,
        }
    }

    /// Whether `dir` is a workspace member: a `members` pattern matches it (an exact path, or
    /// `<prefix>/*` for one segment under the prefix) and no `exclude` entry is it or above it.
    fn member(&self, dir: &str) -> bool {
        let matches = |pattern: &str| match pattern.strip_suffix("/*") {
            Some(prefix) if !has_glob(prefix) => dir
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|name| !name.is_empty() && !name.contains('/')),
            Some(_) => false,
            None => !has_glob(pattern) && pattern == dir,
        };
        let excluded = self.exclude.iter().any(|entry| {
            dir == entry
                || dir
                    .strip_prefix(entry.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        });
        !excluded && self.members.iter().any(|pattern| matches(pattern))
    }
}

/// Whether root-relative `path` is in the no-reach class over `layout` (the module's rule).
#[must_use]
pub fn class(path: &str, layout: &Layout) -> bool {
    if !path.ends_with(".md") {
        return false;
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments
        .iter()
        .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
    {
        return false;
    }
    match segments.as_slice() {
        [name] => *name != EXCLUDED,
        ["docs", rest @ ..] => {
            let mut dir = String::from("docs");
            if layout.manifests.contains(&dir) {
                return false;
            }
            for segment in &rest[..rest.len() - 1] {
                dir.push('/');
                dir.push_str(segment);
                if layout.manifests.contains(&dir) {
                    return false;
                }
            }
            true
        }
        [.., "SPECS.md"] => {
            let dir = &path[..path.len() - "/SPECS.md".len()];
            layout.manifests.contains(dir) && layout.member(dir)
        }
        _ => false,
    }
}

/// This module's own source. `verify land` decides which shared paths are in the class with the
/// rule it was built with, while census's `no-reach-read` guard holds the merge to the merge's
/// rule; the land composes only where the merge holds this same text, so the two agree.
pub const SOURCE: &str = include_str!("no_reach.rs");

/// Whether Rust source `text` names `path` in code: a string literal or an include argument
/// holds it, or a code token does, in the file or in a Rust code block of its doc comments (a
/// doctest, which `targo test --doc` builds and runs: [`doctests`]). A comment that names it
/// elsewhere does not; text that does not lex names it (nothing can say where it sits).
#[must_use]
pub fn names(text: &str, path: &str) -> bool {
    if !text.contains(path) {
        return false;
    }
    if names_code(text, path) {
        return true;
    }
    let doctests = doctests(text);
    doctests.contains(path) && names_code(&doctests, path)
}

/// The text `verify land` searches the merge for, for each shared no-reach `path`: its file name
/// (a reader that builds the path from pieces, or roots it at a helper, still spells that), and
/// each directory between `docs/` and it (a reader that walks the directory).
#[must_use]
pub fn needles(path: &str) -> Vec<&str> {
    let name = path.rsplit_once('/').map_or(path, |(_, name)| name);
    let mut out = vec![name];
    if path.starts_with("docs/") {
        out.extend(
            path.match_indices('/')
                .map(|(at, _)| &path[..at])
                .filter(|dir| *dir != "docs"),
        );
    }
    out
}

/// Whether the merge's file `file` (root-relative), whose text is `text`, may read no-reach
/// `path`, so the path is judged in full rather than composed: `verify land`'s backstop for a
/// read census's `no-reach-read` guard cannot resolve (a helper's root, a path joined from
/// pieces, a test's working directory). It is conservative where the guard is exact:
/// - a member's spec table `<dir>/SPECS.md` is read by a Rust file under `<dir>/` that names
///   `SPECS.md` in code (a test or build script runs in its crate's directory), and by any
///   other that names the whole path or whose string is a bare spec table name (`SPECS.md`,
///   `./SPECS.md`, `{…}/SPECS.md`: a walk of every crate's);
/// - other Markdown is read by a Rust file that names one of its [`needles`] in code;
/// - in each case also by a crate's Markdown whose Rust code blocks name it, as a
///   `#[doc = include_str!(…)]` would run (not root or docs Markdown or a `SPECS.md`, which is in
///   the class, where the guard names any include of it, or is `CHANGELOG.md`);
/// - never by census's own sources, whose guards name paths to police them and rerun on the
///   merge, nor by any other file.
#[must_use]
pub fn may_read(file: &str, text: &str, path: &str) -> bool {
    if !searched(file) {
        return false;
    }
    let rust = file.ends_with(".rs");
    let spec_dir = path
        .strip_suffix("/SPECS.md")
        .filter(|_| !path.starts_with("docs/"));
    let held = match spec_dir {
        Some(_) => text.contains("SPECS.md"),
        None => needles(path)
            .into_iter()
            .any(|needle| text.contains(needle)),
    };
    if !held {
        return false;
    }
    let blocks = if rust { String::new() } else { fenced(text) };
    let named = |needle: &str| {
        if rust {
            names(text, needle)
        } else {
            blocks.contains(needle) && names_code(&blocks, needle)
        }
    };
    match spec_dir {
        Some(dir) => {
            if file
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
            {
                named("SPECS.md")
            } else {
                named(path) || walks_spec_tables(text, rust)
            }
        }
        _ => needles(path).into_iter().any(named),
    }
}

/// Whether [`may_read`] reads root-relative `file` at all: a Rust file, or a crate's Markdown (a
/// `.md` below the root, outside `docs/`, not a `SPECS.md`), outside `crates/aterm-census`.
#[must_use]
pub fn searched(file: &str) -> bool {
    !file.starts_with("crates/aterm-census/")
        && (file.ends_with(".rs")
            || (file.ends_with(".md")
                && file.contains('/')
                && !file.starts_with("docs/")
                && !file.ends_with("/SPECS.md")))
}

/// Whether `text` (Rust, or Markdown whose Rust blocks count) holds a string that is a bare spec
/// table name: `SPECS.md`, `./SPECS.md`, or one placeholder then `/SPECS.md`.
fn walks_spec_tables(text: &str, rust: bool) -> bool {
    let bare = |value: &str| {
        value == "SPECS.md"
            || value == "./SPECS.md"
            || value
                .strip_suffix("}/SPECS.md")
                .is_some_and(|head| head.starts_with('{') && !head[1..].contains(['{', '}']))
    };
    let read = |text: &str| match crate::items::Source::read(text.to_owned()) {
        None => true,
        Some(source) => source.strings().any(|(value, _)| value.is_none_or(bare)),
    };
    if rust {
        read(text) || read(&doctests(text))
    } else {
        read(&fenced(text))
    }
}

/// `text` (Rust source) with all but the Rust code blocks of its doc comments blanked, byte for
/// byte with every newline kept, so offsets and lines read as the file's: the doctests rustdoc
/// builds. A doc comment is `///` (not `////`), `//!`, `/** … */` (not `/***`) or `/*! … */`;
/// a block is fenced by ```` ``` ```` or `~~~` and is Rust unless its info string names anything
/// but rustdoc's own attributes (`ignore`, which is never built, included); a hidden line's `#`
/// is blanked. A run of doc comments ends where code comes between two of them.
#[must_use]
pub fn doctests(text: &str) -> String {
    let mut out = blanked(text);
    if !text.contains("```") && !text.contains("~~~") {
        return out;
    }
    let mut fence = Fence::default();
    let mut last = 0;
    for span in crate::lex::spans(text) {
        if span.kind != crate::lex::SpanKind::Comment {
            continue;
        }
        let comment = &text[span.start..span.end];
        let line_doc = (comment.starts_with("///") && !comment.starts_with("////"))
            || comment.starts_with("//!");
        let block_doc = comment.len() >= 5
            && ((comment.starts_with("/**") && !comment.starts_with("/***"))
                || comment.starts_with("/*!"));
        if !line_doc && !block_doc {
            continue;
        }
        if !text[last..span.start].trim().is_empty() {
            fence = Fence::default();
        }
        last = span.end;
        if line_doc {
            fence.line(text, span.start + 3, span.end, &mut out);
            continue;
        }
        let body_end = if comment.ends_with("*/") {
            span.end - 2
        } else {
            span.end
        };
        let mut at = span.start + 3;
        while at < body_end {
            let line_end = text[at..body_end].find('\n').map_or(body_end, |n| at + n);
            let line = &text[at..line_end];
            let lead = line.len() - line.trim_start().len();
            let mut start = at + lead;
            if text[start..line_end].starts_with('*') {
                start += 1;
            }
            fence.line(text, start, line_end, &mut out);
            at = line_end + 1;
        }
    }
    out
}

/// Markdown `text` with all but its Rust code blocks blanked, as [`doctests`] reads a doc
/// comment's: what a `#[doc = include_str!(…)]` of it makes a doctest.
#[must_use]
pub fn fenced(text: &str) -> String {
    let mut out = blanked(text);
    let mut fence = Fence::default();
    let mut at = 0;
    while at < text.len() {
        let line_end = text[at..].find('\n').map_or(text.len(), |n| at + n);
        fence.line(text, at, line_end, &mut out);
        at = line_end + 1;
    }
    out
}

/// `text` with every byte but its newlines a blank.
fn blanked(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\n' {
                "\n".to_owned()
            } else {
                " ".repeat(c.len_utf8())
            }
        })
        .collect()
}

/// Where a run of documentation is: outside a code block, or in one, Rust or not.
#[derive(Default)]
struct Fence {
    /// The open block's marker and whether it is Rust.
    open: Option<(&'static str, bool)>,
}

impl Fence {
    /// Read the documentation line `text[start..end]`: a fence opens or closes a block, and a
    /// line of a Rust block is copied into `out` at its own offsets (a hidden line's `#` not).
    fn line(&mut self, text: &str, start: usize, end: usize, out: &mut String) {
        let line = &text[start..end];
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        match (self.open, marker) {
            (None, Some(marker)) => {
                let info = trimmed.trim_start_matches(marker.as_bytes()[0] as char);
                self.open = Some((marker, rust_info(info)));
            }
            (Some((open, _)), Some(marker)) if open == marker => self.open = None,
            (Some((_, true)), _) => {
                let lead = line.len() - trimmed.len();
                let mut from = start + lead;
                if trimmed == "#" || trimmed.starts_with("# ") {
                    from += 1;
                }
                // Each byte of `text[from..end]` replaces the blank at its own offset: `out` is
                // `text` blanked byte for byte (all ASCII), and `from` and `end` are boundaries
                // of `text`'s chars.
                out.replace_range(from..end, &text[from..end]);
            }
            _ => {}
        }
    }
}

/// Whether a code block's info string leaves it Rust: every word is one of rustdoc's own (an
/// edition, `should_panic`, `no_run`, …), `ignore` alone not.
fn rust_info(info: &str) -> bool {
    info.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .all(|word| {
            matches!(
                word,
                "rust"
                    | "should_panic"
                    | "no_run"
                    | "compile_fail"
                    | "test_harness"
                    | "standalone_crate"
            ) || word.starts_with("edition")
                || word.starts_with("ignore-")
                || word.starts_with('{')
        })
}

/// [`names`] of `text` itself, its doctests apart.
fn names_code(text: &str, path: &str) -> bool {
    let Some(source) = crate::items::Source::read(text.to_owned()) else {
        return true;
    };
    source
        .strings()
        .any(|(value, _)| value.is_none_or(|value| value.contains(path)))
        || source.includes().iter().any(|include| match &include.arg {
            crate::items::IncludeArg::Relative(arg) | crate::items::IncludeArg::ManifestDir(arg) => {
                arg.contains(path)
            }
            crate::items::IncludeArg::OutDir | crate::items::IncludeArg::Unknown => false,
        })
        || source.code().iter().any(|token| token.text.contains(path))
        || source
            .mounts()
            .iter()
            .any(|mount| matches!(&mount.path, crate::items::MountPath::Literal(at) if at.contains(path)))
}

fn has_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '[', ']', '{', '}', '!'])
}

/// The `[workspace]` table's `members` and `exclude` string arrays (each empty when absent), or
/// `None` where the table is missing or either array holds anything but plain double-quoted
/// strings, commas, blanks and `#` comments.
fn workspace_arrays(manifest: &str) -> Option<(Vec<String>, Vec<String>)> {
    let mut body = String::new();
    let mut inside = false;
    let mut found = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            inside = trimmed == "[workspace]";
            found |= inside;
            continue;
        }
        if inside {
            body.push_str(line);
            body.push('\n');
        }
    }
    if !found {
        return None;
    }
    Some((array(&body, "members")?, array(&body, "exclude")?))
}

/// The string array `key = [ … ]` that starts a line of `body`: empty when no line starts with
/// `key`, `None` when one does and its value is not such an array.
fn array(body: &str, key: &str) -> Option<Vec<String>> {
    let mut at = 0;
    let start = loop {
        let line_end = body[at..].find('\n').map_or(body.len(), |end| at + end);
        let line = &body[at..line_end];
        let lead = line.len() - line.trim_start().len();
        if let Some(rest) = line.trim_start().strip_prefix(key)
            && rest.trim_start().starts_with('=')
        {
            break at + lead + key.len();
        }
        if line_end >= body.len() {
            return Some(Vec::new());
        }
        at = line_end + 1;
    };
    let rest = body[start..].trim_start().strip_prefix('=')?.trim_start();
    let mut chars = rest.strip_prefix('[')?.chars();
    let mut out = Vec::new();
    loop {
        match chars.next()? {
            ']' => return Some(out),
            ',' | ' ' | '\t' | '\n' | '\r' => {}
            '#' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut value = String::new();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' | '\n' => return None,
                        c => value.push(c),
                    }
                }
                out.push(value);
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
#[path = "no_reach_tests.rs"]
mod tests;

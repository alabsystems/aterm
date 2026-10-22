// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH LIBRARIES HOLD A DOCTEST, read off their sources, so the doctests stage's
//! stand-in doc driver skips the rustdoc pass of every library that holds none
//! ([`crate::stages::doctest_split`], [`crate::docdriver`]; most crates have
//! none). A scan, never a manifest flag, and it FAILS WIDE: whatever it cannot
//! read counts as a doctest, because a crate it wrongly leaves out is a doctest
//! that silently never runs, while one it wrongly keeps costs one rustdoc pass.
//!
//! What counts, as rustdoc reads docs:
//! * a fenced block (```` ``` ```` or `~~~`, any length) whose info string rustdoc
//!   compiles ([`info_runs`]): no info, a rustdoc tag (`rust`, `no_run`,
//!   `compile_fail,E0308`, `ignore-wasm32`, `edition2021` …), or any word that is
//!   not a language ([`NOT_RUST`]); `ignore` alone or with `rust` never runs;
//! * an indented code block (four spaces past the doc text, after a blank doc line
//!   or at its start, outside a list item's own indent), which rustdoc compiles as
//!   Rust;
//! * in `///`, `//!`, `/** */` and `/*! */` doc comments;
//! * a doc attribute whose text the scan cannot see (`#[doc = include_str!(…)]`,
//!   `concat!`, a macro's `$doc`, a string holding a fence or spanning lines);
//! * in every `.rs` file under the library's `src/`, and in each file a scanned
//!   file pulls in by `#[path = "…"]`, `include!("…")` or
//!   `include!(concat!(env!("CARGO_MANIFEST_DIR"), "…"))` (aterm-gpu-web's library
//!   is partly aterm-wasm's files); an `include!` of any other computed path counts.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// rustdoc's own tags: a block carrying one compiles as Rust whatever else its info
/// string says ([`info_runs`]). Error codes (`E0308`), `edition*` and
/// `ignore-<target>` are matched by shape.
const RUSTDOC_TAGS: [&str; 7] = [
    "rust",
    "no_run",
    "should_panic",
    "compile_fail",
    "test_harness",
    "standalone_crate",
    "allow_fail",
];

/// Info-string words that name a block rustdoc does not compile: the languages the
/// tree's docs show, and `notest`, which vendored code writes for "not a test". A
/// word in neither list counts as Rust (fail wide).
pub const NOT_RUST: [&str; 34] = [
    "text",
    "txt",
    "plain",
    "console",
    "sh",
    "bash",
    "zsh",
    "shell",
    "fish",
    "powershell",
    "toml",
    "json",
    "yaml",
    "yml",
    "ini",
    "xml",
    "html",
    "css",
    "js",
    "javascript",
    "ts",
    "c",
    "cpp",
    "objc",
    "swift",
    "python",
    "py",
    "sql",
    "diff",
    "markdown",
    "md",
    "tla",
    "rc",
    "notest",
];

/// Does the library whose sources are under `src` hold a doctest? Every `.rs` file
/// under `src`, then every file those pull in ([`pulled_in`]), each read once. An
/// unreadable directory or file, or a pull of a computed path, counts as one.
#[must_use]
pub fn src_holds_a_doctest(src: &Path) -> bool {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs = vec![src.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return true;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(normalized(&path));
            }
        }
    }
    let krate = src.parent().unwrap_or(src);
    let mut seen: BTreeSet<PathBuf> = files.iter().cloned().collect();
    while let Some(file) = files.pop() {
        let Ok(text) = std::fs::read_to_string(&file) else {
            return true;
        };
        if docs_hold_a_doctest(&text) {
            return true;
        }
        let Some(pulled) = pulled_in(&text) else {
            return true;
        };
        let dir = file.parent().unwrap_or(src);
        for pull in pulled {
            let path = normalized(&match pull {
                Pull::Beside(rel) => dir.join(rel),
                Pull::InCrate(rel) => krate.join(rel.trim_start_matches('/')),
            });
            if seen.insert(path.clone()) {
                files.push(path);
            }
        }
    }
    false
}

/// `path` with its `.` and `..` components resolved lexically, so a file reached
/// by `#[path]` and by the walk is read once.
fn normalized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// A file another pulls in, as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pull {
    /// Relative to the pulling file's directory: `#[path = "…"]`, `include!("…")`.
    Beside(String),
    /// Relative to the compiling crate's directory:
    /// `include!(concat!(env!("CARGO_MANIFEST_DIR"), "…"))`.
    InCrate(String),
}

/// The files `text` pulls in: each `#[path = "…"]` and each `include!(…)` that
/// opens a line, the call read across the lines it spans. `None` when an
/// `include!` names a path computed any other way, whose text the scan cannot see.
#[must_use]
pub fn pulled_in(text: &str) -> Option<Vec<Pull>> {
    /// How many lines an `include!` call is read across.
    const CALL_LINES: usize = 8;
    let lines: Vec<&str> = text.lines().collect();
    let literal = |rest: &str| Some(rest.strip_prefix('"')?.split('"').next()?.to_string());
    let mut v = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if let Some(r) = t.strip_prefix("#[path") {
            v.push(Pull::Beside(literal(
                r.trim_start().strip_prefix('=')?.trim_start(),
            )?));
        } else if t.starts_with("include!(") {
            let call: String = lines[i..]
                .iter()
                .take(CALL_LINES)
                .flat_map(|l| l.chars())
                .filter(|c| !c.is_whitespace())
                .collect();
            let rest = call.strip_prefix("include!(")?;
            v.push(
                match rest.strip_prefix("concat!(env!(\"CARGO_MANIFEST_DIR\"),") {
                    Some(lit) => Pull::InCrate(literal(lit)?),
                    None => Pull::Beside(literal(rest)?),
                },
            );
        }
    }
    Some(v)
}

/// Does `text` hold a doctest rustdoc would compile (the module doc's list)?
#[must_use]
pub fn docs_hold_a_doctest(text: &str) -> bool {
    let mut doc = DocText::default();
    let mut in_block = false;
    for line in text.lines() {
        let t = line.trim_start();
        if in_block {
            let (body, closes) = match line.find("*/") {
                Some(i) => (&line[..i], true),
                None => (line, false),
            };
            // ` * text` (a starred block) reads as `text`; an unstarred line as is.
            let body = match body.trim_start().strip_prefix('*') {
                Some(after) => after,
                None => body,
            };
            if doc.line(body) {
                return true;
            }
            if closes {
                in_block = false;
                doc = DocText::default();
            }
            continue;
        }
        let opens = t
            .strip_prefix("/**")
            .filter(|r| !r.starts_with(['*', '/']))
            .or_else(|| t.strip_prefix("/*!"));
        if let Some(rest) = opens {
            let (body, closes) = match rest.find("*/") {
                Some(i) => (&rest[..i], true),
                None => (rest, false),
            };
            if doc.line(body) {
                return true;
            }
            if closes {
                doc = DocText::default();
            } else {
                in_block = true;
            }
            continue;
        }
        let comment = t
            .strip_prefix("///")
            .filter(|r| !r.starts_with('/'))
            .or_else(|| t.strip_prefix("//!"));
        if let Some(body) = comment {
            if doc.line(body) {
                return true;
            }
            continue;
        }
        if t.starts_with("#[") || t.starts_with("#![") {
            if doc_attribute_hides_text(t) {
                return true;
            }
            // An attribute between doc lines keeps the item's doc text going.
            continue;
        }
        doc = DocText::default();
    }
    false
}

/// One item's doc text, read a line at a time.
#[derive(Default)]
struct DocText {
    /// The open fence's character and length.
    fence: Option<(char, usize)>,
    /// A line has been read.
    started: bool,
    /// The last line read was blank.
    blank: bool,
    /// The current list item's content indent: an indented code block inside it
    /// sits four columns past it.
    list: usize,
}

impl DocText {
    /// Read one line of doc text (`raw`, after its comment marker): is a doctest
    /// open here?
    fn line(&mut self, raw: &str) -> bool {
        // rustdoc strips the doc text's common indent, which is one space by habit.
        let text = raw.strip_prefix(' ').unwrap_or(raw);
        let body = text.trim_start();
        let indent: usize = text[..text.len() - body.len()]
            .chars()
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let first = !std::mem::replace(&mut self.started, true);
        if let Some((c, n)) = self.fence {
            if fence(body).is_some_and(|(fc, fnum, info)| fc == c && fnum >= n && info.is_empty()) {
                self.fence = None;
                self.blank = true;
            }
            return false;
        }
        if body.is_empty() {
            self.blank = true;
            return false;
        }
        let after_blank = std::mem::replace(&mut self.blank, false) || first;
        if let Some((c, n, info)) = fence(body) {
            if info_runs(info) {
                return true;
            }
            self.fence = Some((c, n));
            return false;
        }
        if after_blank && indent >= 4 + self.list {
            return true;
        }
        if let Some(width) = list_marker(body) {
            self.list = indent + width;
        } else if after_blank && indent < self.list {
            self.list = 0;
        }
        false
    }
}

/// A fence opening `body`: its character, its length and its info string. A
/// backtick run whose info string holds a backtick is inline code, not a fence.
fn fence(body: &str) -> Option<(char, usize, &str)> {
    let c = body.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = body.chars().take_while(|x| *x == c).count();
    if n < 3 {
        return None;
    }
    let info = body[n..].trim();
    (c == '~' || !info.contains('`')).then_some((c, n, info))
}

/// The content indent a list marker opening `body` gives its item (`- `, `* `,
/// `+ `, `1. `, `1) `), or `None`.
fn list_marker(body: &str) -> Option<usize> {
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    let marker = match body.as_bytes().get(digits)? {
        b'.' | b')' if (1..=9).contains(&digits) => digits + 1,
        b'-' | b'*' | b'+' if digits == 0 => 1,
        _ => return None,
    };
    let rest = &body[marker..];
    let spaces = rest.chars().take_while(|c| *c == ' ').count();
    match spaces {
        0 if !rest.is_empty() => None,
        1..=4 => Some(marker + spaces),
        _ => Some(marker + 1),
    }
}

/// Does rustdoc compile a fenced block with this info string? Never when it says
/// `ignore`; always when it carries a rustdoc tag ([`RUSTDOC_TAGS`], an error code,
/// `edition*`, `ignore-<target>`); otherwise unless a word names a language
/// ([`NOT_RUST`]). `{.class}` and `key=value` attributes are neither.
#[must_use]
pub fn info_runs(info: &str) -> bool {
    let words: Vec<String> = info
        .split(|c: char| c == ',' || c == '{' || c == '}' || c.is_whitespace())
        .filter(|w| !w.is_empty() && !w.starts_with('.') && !w.contains('='))
        .map(str::to_ascii_lowercase)
        .collect();
    if words.iter().any(|w| w == "ignore") {
        return false;
    }
    let error_code =
        |w: &str| w.len() == 5 && w.starts_with('e') && w[1..].bytes().all(|b| b.is_ascii_digit());
    let rustdoc = |w: &String| {
        RUSTDOC_TAGS.contains(&w.as_str())
            || w.starts_with("edition")
            || w.starts_with("ignore-")
            || error_code(w)
    };
    words.iter().any(rustdoc) || !words.iter().any(|w| NOT_RUST.contains(&w.as_str()))
}

/// Does this attribute line give doc text the scan cannot read: `doc = …` with
/// anything but a one-line string literal free of fences (`include_str!`,
/// `concat!`, a macro's `$doc`, a constant, a string spanning lines)?
fn doc_attribute_hides_text(attr: &str) -> bool {
    let mut from = 0;
    while let Some(at) = attr[from..].find("doc") {
        let start = from + at;
        from = start + 3;
        let before = attr[..start].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let Some(rhs) = attr[from..].trim_start().strip_prefix('=') else {
            continue;
        };
        // `==`, or `doc = \"…` inside a string literal: no attribute.
        if rhs.starts_with('=') || rhs.trim_start().starts_with('\\') {
            continue;
        }
        let rhs = rhs.trim_start();
        let literal = rhs
            .strip_prefix('"')
            .or_else(|| rhs.strip_prefix("r\""))
            .or_else(|| rhs.strip_prefix("r#\""));
        let Some(literal) = literal else {
            return true;
        };
        let Some(end) = closing_quote(literal) else {
            return true;
        };
        let text = &literal[..end];
        if text.contains("```") || text.contains("~~~") || text.starts_with("    ") {
            return true;
        }
    }
    false
}

/// The byte index of the `"` that closes a string literal's text, past `\"`.
fn closing_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(i),
            _ => escaped = false,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A doctest is a block rustdoc compiles, in any doc-comment form: Rust's
    /// info string, rustdoc's tags (error codes and `ignore-<target>` too), an
    /// unknown word (fail wide), an indented code block, a doc attribute the scan
    /// cannot read. Never `text`, a language, `ignore`, a plain comment, a string,
    /// or a list item's own indented text.
    #[test]
    fn a_doctest_is_a_block_rustdoc_compiles_in_a_doc_comment() {
        for yes in [
            "/// ```\n/// f();\n/// ```\n",
            "    //! ```rust\n    //! ```\n",
            "/// ```no_run\n/// ```\n",
            "/// ```edition2021,should_panic\n/// ```\n",
            "/// ```compile_fail,E0308\n/// let x: u8 = \"\";\n/// ```\n",
            "/// ```ignore-wasm32\n/// ```\n",
            "/// ```custom_word\n/// ```\n",
            "/// ~~~\n/// f();\n/// ~~~\n",
            "/// ````\n/// f();\n/// ````\n",
            "/**\n * ```\n * f();\n * ```\n */\n",
            "/*!\nText.\n\n```\nf();\n```\n*/\n",
            "#![doc = include_str!(\"../README.md\")]\n",
            "#[cfg_attr(doc, doc = include_str!(\"x.md\"))]\n",
            "#[doc = concat!(\"a\", $b)]\n",
            "#[doc = \"```\"]\n",
            "/// ```text\n/// x\n/// ```\n/// ```\n/// f();\n/// ```\n",
            "/// Text.\n///\n///     let x = 1;\n",
            "///     let x = 1;\n",
            "/// * item\n///\n///       code_in_the_item();\n",
        ] {
            assert!(docs_hold_a_doctest(yes), "{yes:?}");
        }
        for no in [
            "/// ```text\n/// x\n/// ```\n",
            "/// ```sh\n/// ls\n/// ```\n/// after\n",
            "/// ```rust,ignore\n/// ```\n",
            "/// ```ignore\n/// ```\n",
            "/// ```notest\n/// ```\n",
            "// ```\n// f();\n// ```\n",
            "//// ```\n//// f();\n//// ```\n",
            "/*\n```\nf();\n```\n*/\n",
            "let fence = \"```\";\n",
            "#[doc = \"plain\"]\n",
            "#[doc(hidden)]\n#[cfg(doc)]\n",
            "/// ```inline``` code\n",
            "/// Text\n///     continued, not a block.\n",
            "/// 1. item\n///\n///    more of the item.\n",
            "/// ```text\n///     indented inside a fence\n/// ```\n",
            "    #[doc = \\\" A \\\"] pub struct InAString;\n",
        ] {
            assert!(!docs_hold_a_doctest(no), "{no:?}");
        }
    }

    /// What a file pulls in, as written; a computed `include!` is unreadable.
    #[test]
    fn a_file_pulls_in_its_path_and_include_targets() {
        let text = "#[path = \"../../w/src/a.rs\"]\nmod a;\n  include!(\"b.rs\");\n\
                    let s = \"include!(\\\"no\\\")\";\n";
        assert_eq!(
            pulled_in(text),
            Some(vec![
                Pull::Beside("../../w/src/a.rs".to_string()),
                Pull::Beside("b.rs".to_string())
            ])
        );
        assert_eq!(
            pulled_in(
                "include!(concat!(\n    env!(\"CARGO_MANIFEST_DIR\"),\n    \"/src/t.rs\"\n));\n"
            ),
            Some(vec![Pull::InCrate("/src/t.rs".to_string())])
        );
        assert_eq!(
            pulled_in("include!(concat!(env!(\"OUT_DIR\"), \"/g.rs\"));\n"),
            None
        );
    }

    /// The walk follows a pull out of `src/`, and an unreadable pull counts.
    #[test]
    fn a_doctest_in_a_file_pulled_in_from_another_crate_counts() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("a/src");
        let other = tmp.path().join("b/src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::create_dir_all(&other).expect("mkdir");
        std::fs::write(
            other.join("x.rs"),
            "/// ```\n/// f();\n/// ```\npub fn f() {}\n",
        )
        .expect("write");
        std::fs::write(src.join("lib.rs"), "//! No doctest here.\n").expect("write");
        assert!(!src_holds_a_doctest(&src));
        std::fs::write(
            src.join("lib.rs"),
            "#[path = \"../../b/src/x.rs\"]\nmod x;\n",
        )
        .expect("write");
        assert!(src_holds_a_doctest(&src));
        std::fs::write(
            src.join("lib.rs"),
            "include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../b/src/x.rs\"));\n",
        )
        .expect("write");
        assert!(
            src_holds_a_doctest(&src),
            "a pull from the crate's directory is followed"
        );
        std::fs::write(src.join("lib.rs"), "include!(\"../../b/src/gone.rs\");\n").expect("write");
        assert!(src_holds_a_doctest(&src), "an unreadable pull fails wide");
    }
}

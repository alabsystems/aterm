// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Source scanner that detects whether a crate contains any doctests.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

const RUSTDOC_TAGS: [&str; 7] = [
    "rust",
    "no_run",
    "should_panic",
    "compile_fail",
    "test_harness",
    "standalone_crate",
    "allow_fail",
];

/// Code-fence info-string tokens that rustdoc does not compile as Rust.
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

/// Does the library whose sources are under `src` hold a doctest?
#[must_use]
pub fn src_holds_a_doctest(src: &Path) -> bool {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs = vec![src.to_path_buf()];
    let mut seen_dirs: BTreeSet<PathBuf> =
        BTreeSet::from([src.canonicalize().unwrap_or_else(|_| normalized(src))]);
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return true;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else {
                return true;
            };
            if meta.is_dir() {
                let key = path.canonicalize().unwrap_or_else(|_| normalized(&path));
                if seen_dirs.insert(key) {
                    dirs.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(normalized(&path));
            }
        }
    }
    let krate = src.parent().unwrap_or(src);
    let mut seen: BTreeSet<PathBuf> = files.iter().cloned().collect();
    while let Some(file) = files.pop() {
        let Ok(text) = crate::read_regular_text(&file) else {
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
    /// Relative to the compiling crate's directory: `include!(concat!(env!("CARGO_MANIFEST_DIR"), "…"))`.
    InCrate(String),
}

/// Returns the files referenced by `#[path = "…"]` and `include!(…)` in `text`.
#[must_use]
pub fn pulled_in(text: &str) -> Option<Vec<Pull>> {
    const CALL_LINES: usize = 8;
    let lines: Vec<&str> = text.lines().collect();
    let mut v = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if let Some(attr) = t.strip_prefix("#[").map(str::trim_start) {
            if let Some(r) = attr
                .strip_prefix("path")
                .filter(|r| r.starts_with(char::is_whitespace) || r.starts_with('='))
            {
                let rhs = r.trim_start().strip_prefix('=')?.trim_start();
                v.push(Pull::Beside(parse_string_literal(rhs)?.0.to_string()));
            } else if attr.starts_with("cfg_attr")
                && let Some(path_rhs) = cfg_attr_path_rhs(attr)
            {
                v.push(Pull::Beside(parse_string_literal(path_rhs)?.0.to_string()));
            }
        } else if let Some(after_inc) = t.strip_prefix("include!")
            && after_inc.trim_start().starts_with(['(', '[', '{'])
        {
            let joined = lines[i..]
                .iter()
                .take(CALL_LINES)
                .copied()
                .collect::<Vec<_>>()
                .join(" ");
            v.push(parse_include_call(&joined)?);
        }
    }
    Some(v)
}

fn parse_string_literal(rest: &str) -> Option<(&str, &str)> {
    let rest = rest.trim_start();
    if let Some(after_r) = rest.strip_prefix('r') {
        let hashes = after_r.bytes().take_while(|&b| b == b'#').count();
        let body = after_r[hashes..].strip_prefix('"')?;
        let close = format!("\"{}", "#".repeat(hashes));
        let end = body.find(&close)?;
        return Some((&body[..end], &body[end + close.len()..]));
    }
    let body = rest.strip_prefix('"')?;
    let end = closing_quote(body)?;
    Some((&body[..end], &body[end + 1..]))
}

fn cfg_attr_path_rhs(attr: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(at) = attr[from..].find("path") {
        let idx = from + at;
        from = idx + 4;
        let before = attr[..idx].trim_end();
        if !before.ends_with([',', '(']) {
            continue;
        }
        let after = &attr[from..];
        if let Some(rhs) = after.trim_start().strip_prefix('=')
            && !rhs.starts_with('=')
        {
            return Some(rhs.trim_start());
        }
    }
    None
}

fn strip_open_delim(s: &str) -> Option<&str> {
    let s = s.trim_start();
    s.strip_prefix('(')
        .or_else(|| s.strip_prefix('['))
        .or_else(|| s.strip_prefix('{'))
}

fn strip_close_delim(s: &str) -> Option<&str> {
    let s = s.trim_start();
    s.strip_prefix(')')
        .or_else(|| s.strip_prefix(']'))
        .or_else(|| s.strip_prefix('}'))
}

fn parse_include_call(joined: &str) -> Option<Pull> {
    let rest = joined.trim_start().strip_prefix("include!")?;
    let rest = strip_open_delim(rest)?.trim_start();
    if let Some(after_concat) = rest.strip_prefix("concat!") {
        let inner = strip_open_delim(after_concat)?.trim_start();
        let after_env = inner.strip_prefix("env!")?;
        let env_inner = strip_open_delim(after_env)?.trim_start();
        let (key, after_key) = parse_string_literal(env_inner)?;
        if key != "CARGO_MANIFEST_DIR" {
            return None;
        }
        let after_env_call = strip_close_delim(after_key)?.trim_start();
        let lit_rest = after_env_call.strip_prefix(',')?.trim_start();
        return Some(Pull::InCrate(parse_string_literal(lit_rest)?.0.to_string()));
    }
    Some(Pull::Beside(parse_string_literal(rest)?.0.to_string()))
}

/// Does `text` hold a doctest rustdoc would compile?
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
    fn line(&mut self, raw: &str) -> bool {
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

fn fence(body: &str) -> Option<(char, usize, &str)> {
    let c = body.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let n = body.chars().take_while(|x| *x == c).count();
    if n < 3 {
        return None;
    }
    let info = body[n..].trim();
    (c == '~' || !info.contains('`')).then_some((c, n, info))
}

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

/// Does rustdoc compile a fenced block with this info string?
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

    #[test]
    fn pulled_in_handles_raw_strings_cfg_attr_and_alternate_include_delimiters() {
        let text = "#[path = r\"a/b.rs\"]\nmod b;\n\
                    #[cfg_attr(unix, path = r#\"c/d.rs\"#)]\nmod d;\n\
                    include! [r\"e.rs\"];\n\
                    include!{concat![env!(\"CARGO_MANIFEST_DIR\"), r\"/src/f.rs\"]}\n";
        assert_eq!(
            pulled_in(text),
            Some(vec![
                Pull::Beside("a/b.rs".to_string()),
                Pull::Beside("c/d.rs".to_string()),
                Pull::Beside("e.rs".to_string()),
                Pull::InCrate("/src/f.rs".to_string()),
            ])
        );
    }

    #[cfg(unix)]
    #[test]
    fn src_holds_a_doctest_follows_symlinked_subdirectories_and_handles_cycles() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("a/src");
        let ext = tmp.path().join("ext");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::create_dir_all(&ext).expect("mkdir");
        std::fs::write(src.join("lib.rs"), "pub fn plain() {}\n").expect("write");
        std::fs::write(ext.join("sub.rs"), "pub fn no_doc() {}\n").expect("write");
        std::os::unix::fs::symlink(&ext, src.join("linked")).expect("symlink");
        std::os::unix::fs::symlink(&src, ext.join("cycle")).expect("cycle symlink");
        assert!(!src_holds_a_doctest(&src));
        std::fs::write(
            ext.join("sub.rs"),
            "/// ```\n/// f();\n/// ```\npub fn f() {}\n",
        )
        .expect("write");
        assert!(src_holds_a_doctest(&src));
    }
}

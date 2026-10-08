// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EVERY TEST TARGET A COMMENT OR DOC CITES IS ONE ITS PACKAGE DECLARES.
//!
//! The crates here turn `autotests` off and mount their `tests/*.rs` files as
//! modules of one suite binary, so a file's name stops being a `--test` name
//! the day it becomes a module, and nothing compiles a comment. Found
//! 2026-10-02: the live-class auditor's header cited aterm-objc's
//! `objc_suite` (no such target; `winit_seam` is its own), the render
//! corpus's bless line a target named for its own file (a module of
//! `render_suite`), and four of this crate's docs an atpkg `index_probe` that
//! never existed. Each answers `no test target named` to whoever copies it,
//! and none failed anything. It is asserted here, where the gate's own tests
//! run on every merge.
//!
//! WHAT IS READ: the tree the gate measures — git's tracked files and its
//! untracked, non-ignored ones ([`tree_files`]) — minus `vendor/` (other
//! projects' sources and the astream submodule). Build output, ignored scratch
//! and a gate snapshot's lane target dirs are never read, so no file outside
//! the tree can invent a finding or, as a shadow manifest, hide one. In it:
//! full-line comments (`//`, `///`, `//!` in `.rs`; `#` in `.toml`, `.sh`,
//! `.py`) and the whole of each `.md`. Consecutive comment lines are one text,
//! so a command wrapped across lines reads whole. Dated records keep what they
//! recorded: a path carrying a `YYYY-MM-DD`, everything under
//! `docs/measured/`, and `CHANGELOG.md`.
//!
//! WHAT IS JUDGED: a `--test <name>` whose command — the text after the
//! nearest backtick, `targo` or `cargo` before it, at most 240 bytes back —
//! selects with `-p`/`--package` a package this tree declares; the name must
//! be a test target of one of them.
//!
//! NOT JUDGED, ON PURPOSE: string literals (the ones that spell a spec are
//! fixtures: a recorded transcript, a synthetic `-p x`), and with them an
//! `#[ignore = "…"]` reason; a spec with no package (`--test x` alone, which
//! in a comment as often names another crate's target as its own); a
//! `--workspace` run; a package that is not this tree's. NOT READ, as a
//! limit of a reader with no Rust lexer: a comment trailing code on its line,
//! a `/* … */` block, and the module filter after `--` (only the target is
//! judged, not that `-- <module>::` names a module of it).
//!
//! A package's test targets are read from its manifest the way cargo reads
//! them: every `[[test]]` name, plus, unless `autotests = false`, each
//! `tests/<n>.rs` and `tests/<n>/main.rs`. On 2026-10-02 that agreed with
//! `targo metadata --no-deps` for all 95 packages: the workspace's 87 members
//! and 8 more from seven standalone manifests. No manifest parser: this crate
//! has no dependencies on purpose (see its Cargo.toml).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use aterm_verify::identity;

/// The tree the gate measures, by the listing that decides it
/// ([`identity`]): tracked files plus untracked, non-ignored ones, sorted,
/// never under `vendor/`. Panics when git cannot list `root`: a census of
/// nothing would pass.
fn tree_files(root: &Path) -> Vec<PathBuf> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let out = identity::git(root, &path_env)
        .args(["ls-files", "-z"])
        .output()
        .unwrap_or_else(|e| panic!("cannot run git in {}: {e}", root.display()));
    assert!(
        out.status.success(),
        "git ls-files failed in {}: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let untracked = identity::untracked_paths(root, &path_env)
        .unwrap_or_else(|| panic!("git cannot list {}'s untracked files", root.display()));
    let mut files: Vec<PathBuf> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .chain(untracked)
        .filter(|p| !p.starts_with("vendor/"))
        .map(PathBuf::from)
        .collect();
    files.sort();
    files.dedup();
    files
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-verify sits two levels below the workspace root")
        .to_path_buf()
}

/// A manifest's `[package]` name and test targets. `tests_dir` is what sits
/// directly under the package's `tests/`: file names, and `<dir>/` for each
/// directory holding a `main.rs`.
fn test_targets(manifest: &str, tests_dir: &[String]) -> Option<(String, BTreeSet<String>)> {
    let mut table = "";
    let mut name = None;
    let mut autotests = true;
    let mut targets = BTreeSet::new();
    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            table = line;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match (table, key.trim()) {
            ("[package]", "name") if name.is_none() => name = Some(value.to_string()),
            ("[package]", "autotests") => autotests = value != "false",
            ("[[test]]", "name") => {
                targets.insert(value.to_string());
            }
            _ => {}
        }
    }
    if autotests {
        for entry in tests_dir {
            if let Some(n) = entry
                .strip_suffix(".rs")
                .or_else(|| entry.strip_suffix('/'))
            {
                targets.insert(n.to_string());
            }
        }
    }
    Some((name?, targets))
}

/// Every package of the tree, by name: one per `Cargo.toml` in `files` (the
/// workspace's members, the standalone tools and oracles, a fuzz crate) that
/// has a `[package]`. Two manifests of one name pool their targets, so a
/// fixture manifest can hide a finding but never invent one.
fn packages(root: &Path, files: &[PathBuf]) -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for rel in files.iter().filter(|r| r.ends_with("Cargo.toml")) {
        let dir = root.join(rel.parent().unwrap_or(Path::new("")));
        let Ok(manifest) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let mut tests_dir = Vec::new();
        for e in fs::read_dir(dir.join("tests"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            if path.is_file() {
                tests_dir.push(name);
            } else if path.join("main.rs").is_file() {
                tests_dir.push(format!("{name}/"));
            }
        }
        if let Some((name, targets)) = test_targets(&manifest, &tests_dir) {
            found.entry(name).or_default().extend(targets);
        }
    }
    found
}

/// One readable text of a file, and the file line each of its lines began on
/// (`(offset in body, 1-based line)`).
struct Text {
    body: String,
    starts: Vec<(usize, usize)>,
}

impl Text {
    fn line_of(&self, offset: usize) -> usize {
        self.starts
            .iter()
            .take_while(|(at, _)| *at <= offset)
            .last()
            .map_or(0, |(_, line)| *line)
    }
}

/// Is `rel` a dated record, which keeps what it recorded?
fn dated(rel: &str) -> bool {
    let b = rel.as_bytes();
    rel.starts_with("docs/measured/")
        || rel == "CHANGELOG.md"
        || b.windows(10).any(|w| {
            w.iter().enumerate().all(|(i, c)| {
                if i == 4 || i == 7 {
                    *c == b'-'
                } else {
                    c.is_ascii_digit()
                }
            })
        })
}

/// The texts of `source` a reader takes as instructions: each run of full-line
/// comments joined, or the whole of a Markdown file. Nothing for a dated
/// record or a kind of file not read.
fn texts(rel: &str, source: &str) -> Vec<Text> {
    if dated(rel) {
        return Vec::new();
    }
    let ext = rel.rsplit_once('.').map_or("", |(_, e)| e);
    let comment = |line: &str| -> Option<String> {
        let t = line.trim_start();
        let rest = match ext {
            "md" => return Some(line.to_string()),
            "rs" => t
                .strip_prefix("///")
                .or_else(|| t.strip_prefix("//!"))
                .or_else(|| t.strip_prefix("//"))?,
            "toml" | "sh" | "py" if !t.starts_with("#!") => {
                t.strip_prefix('#')?.trim_start_matches('#')
            }
            _ => return None,
        };
        Some(rest.strip_prefix(' ').unwrap_or(rest).to_string())
    };
    let mut out = Vec::new();
    let mut cur: Option<Text> = None;
    for (i, line) in source.lines().enumerate() {
        match comment(line) {
            Some(c) => {
                let t = cur.get_or_insert_with(|| Text {
                    body: String::new(),
                    starts: Vec::new(),
                });
                t.starts.push((t.body.len(), i + 1));
                t.body.push_str(c.trim_end());
                t.body.push(' ');
            }
            None => out.extend(cur.take()),
        }
    }
    out.extend(cur);
    out
}

/// One `--test <target>` in a text: where, and the packages its command selects.
#[derive(Debug, PartialEq, Eq)]
struct Citation {
    offset: usize,
    packages: Vec<String>,
    target: String,
}

/// The offset just past the last whole word `word` in `s`.
fn after_last_word(s: &str, word: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut end = s.len();
    while let Some(i) = s[..end].rfind(word) {
        let free = |c: Option<&u8>| c.is_none_or(|c| !(c.is_ascii_alphanumeric() || *c == b'_'));
        if free(i.checked_sub(1).and_then(|j| b.get(j))) && free(b.get(i + word.len())) {
            return Some(i + word.len());
        }
        end = i;
    }
    None
}

/// Every judged `--test` in `body`: a flag with a target name, whose command
/// selects packages and is not a `--workspace` run.
fn citations(body: &str) -> Vec<Citation> {
    const FLAG: &str = "--test";
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = body[from..].find(FLAG) {
        let at = from + i;
        from = at + FLAG.len();
        if at > 0 && !matches!(b[at - 1], b' ' | b'`' | b'(' | b'"' | b'\'') {
            continue;
        }
        let rest = &body[from..];
        let rest = match rest.strip_prefix('=') {
            Some(r) => r,
            None if rest.starts_with(' ') => rest.trim_start(),
            None => continue,
        };
        let rest = rest.strip_prefix(['`', '"', '\'']).unwrap_or(rest);
        let len = rest
            .bytes()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
            .count();
        if len == 0 || rest.starts_with('-') {
            continue;
        }
        let mut lo = at.saturating_sub(240);
        while !body.is_char_boundary(lo) {
            lo += 1;
        }
        let before = &body[lo..at];
        let cut = [
            before.rfind('`').map(|j| j + 1),
            after_last_word(before, "targo"),
            after_last_word(before, "cargo"),
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0);
        let command = &before[cut..];
        if command.contains("--workspace") {
            continue;
        }
        let words: Vec<&str> = command.split_whitespace().collect();
        let mut packages = Vec::new();
        for (k, w) in words.iter().enumerate() {
            let named = match *w {
                "-p" | "--package" => words.get(k + 1).copied(),
                _ => w
                    .strip_prefix("-p=")
                    .or_else(|| w.strip_prefix("--package=")),
            };
            if let Some(p) = named {
                // A pkgid spec (`name@version`) names its package before the `@`.
                let p = p.trim_matches([',', ')', ';', '`', '"', '\'']);
                packages.push(p.split('@').next().unwrap_or(p).to_string());
            }
        }
        if !packages.is_empty() {
            out.push(Citation {
                offset: at,
                packages,
                target: rest[..len].to_string(),
            });
        }
    }
    out
}

/// The census of one file: `(judged, findings)`, a finding for each judged
/// citation whose target none of its packages declares.
fn census(
    rel: &str,
    source: &str,
    packages: &BTreeMap<String, BTreeSet<String>>,
) -> (usize, Vec<String>) {
    let mut judged = 0;
    let mut findings = Vec::new();
    for text in texts(rel, source) {
        for c in citations(&text.body) {
            let known: Vec<(&String, &BTreeSet<String>)> = c
                .packages
                .iter()
                .filter_map(|p| packages.get_key_value(p))
                .collect();
            if known.is_empty() {
                continue;
            }
            judged += 1;
            if known.iter().any(|(_, t)| t.contains(&c.target)) {
                continue;
            }
            let declared: Vec<String> = known
                .iter()
                .map(|(p, t)| {
                    let names: Vec<&str> = t.iter().map(String::as_str).collect();
                    format!("{p} declares [{}]", names.join(", "))
                })
                .collect();
            findings.push(format!(
                "{rel}:{}: `--test {}` with -p {}: {}",
                text.line_of(c.offset),
                c.target,
                c.packages.join(" -p "),
                declared.join("; ")
            ));
        }
    }
    (judged, findings)
}

#[test]
fn every_cited_test_target_is_declared() {
    let root = workspace_root();
    let files = tree_files(&root);
    let packages = packages(&root, &files);
    let mut judged = 0;
    let mut findings = Vec::new();
    for rel in &files {
        let read = rel
            .extension()
            .is_some_and(|e| ["rs", "toml", "sh", "py", "md"].iter().any(|k| e == *k));
        if !read {
            continue;
        }
        let Ok(source) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let (j, f) = census(&rel, &source, &packages);
        judged += j;
        findings.extend(f);
    }
    assert!(
        findings.is_empty(),
        "{} cited test target(s) no package declares — name the target that \
         runs it (a suite module is `--test <suite> -- <module>::`):\n{}",
        findings.len(),
        findings.join("\n")
    );
    eprintln!(
        "cited test targets: {judged} judged in {} files",
        files.len()
    );
    // A reader that judges nothing passes everything. So does a manifest
    // reader that finds no package; one that loses a package's targets turns
    // its citations into findings instead. 60 judged on 2026-10-02.
    assert!(
        judged >= 20,
        "only {judged} citations judged across {} files and {} packages: the reader is blind",
        files.len(),
        packages.len()
    );
}

/// The reader, on the shapes that broke and the shapes that must not be judged.
#[test]
fn a_cited_target_is_read_the_way_its_reader_reads_it() {
    let packages: BTreeMap<String, BTreeSet<String>> = [
        ("aterm-objc", &["objc_static_suite", "winit_seam"][..]),
        ("aterm-render", &["render_suite"][..]),
    ]
    .into_iter()
    .map(|(p, t)| (p.to_string(), t.iter().map(|s| (*s).to_string()).collect()))
    .collect();

    // Built from lines so no line of THIS file is a comment the census reads.
    let rs = [
        "//! `cargo test -p aterm-objc --test objc_suite winit_seam::` 6/6 PASS",
        "//! `targo --unverified test -p aterm-objc --test winit_seam`",
        "//! `ATERM_BLESS_GOLDEN=1 targo --unverified test -p aterm-render --test",
        "//! procedural_orthogonal_regression` and justify the diff",
        "/// `targo --unverified test -p aterm-render --test",
        "/// render_suite -- procedural_orthogonal_regression::`",
        "// `targo test --workspace --test paint`, `-p x --test probe`, `--test-threads`",
        "// `targo test -p aterm-objc -p aterm-render --test=render_suite -- --test-threads=1`",
        "// cargo's re-run spec (`-p aterm-objc --test winit_seam`, `-p aterm-gui --lib`)",
        "// `targo test -p \"aterm-objc\" --test \"objc_suite\"`, quoted",
        "// `targo test -p aterm-objc@0.1.0 --test winit_seam`, by pkgid",
        "let s = \"targo test -p aterm-objc --test nowhere\";",
    ]
    .join("\n");
    let (judged, findings) = census("crates/a/src/lib.rs", &rs, &packages);
    assert_eq!(
        findings,
        [
            "crates/a/src/lib.rs:1: `--test objc_suite` with -p aterm-objc: \
             aterm-objc declares [objc_static_suite, winit_seam]",
            "crates/a/src/lib.rs:3: `--test procedural_orthogonal_regression` with -p \
             aterm-render: aterm-render declares [render_suite]",
            "crates/a/src/lib.rs:10: `--test objc_suite` with -p aterm-objc: \
             aterm-objc declares [objc_static_suite, winit_seam]",
        ]
    );
    assert_eq!(
        judged, 8,
        "lines 1, 2, 3, 5, 8, 9, 10, 11; never the string literal"
    );

    let md = "Run\n`targo --unverified test -p aterm-objc --test swizzle`.\n";
    assert_eq!(census("docs/OBJC.md", md, &packages).1.len(), 1);
    assert_eq!(
        census("docs/OBJC-2026-10-02.md", md, &packages),
        (0, Vec::new())
    );
    assert_eq!(
        census("docs/measured/x/notes.md", md, &packages),
        (0, Vec::new())
    );
    assert_eq!(
        census("crates/a/fixtures/cmd.txt", md, &packages),
        (0, Vec::new())
    );

    let toml = "# `--test winit_seam` is the spelling\n# `-p aterm-objc --test\n#   weak` alone\nname = \"x\"\n";
    let (judged, findings) = census("crates/aterm-objc/Cargo.toml", toml, &packages);
    assert_eq!(judged, 1);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert!(findings[0].starts_with("crates/aterm-objc/Cargo.toml:2: `--test weak`"));
}

/// The manifest reader, on the rules cargo applies.
#[test]
fn a_manifest_declares_what_cargo_would_build() {
    let dir: Vec<String> = ["a.rs", "b/", "notes.md", "suite.rs"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let off = "\
[package]
name = \"p\" # trailing
autotests = false

[package.metadata.x]
name = \"not-it\"

[[bin]]
name = \"tool\"

[[test]]
path = \"tests/suite.rs\"
name = \"suite\"
";
    let (name, t) = test_targets(off, &dir).expect("a package");
    assert_eq!(name, "p");
    assert_eq!(t.into_iter().collect::<Vec<_>>(), ["suite"]);

    let on = "[package]\nname = \"q\"\n\n[[test]]\nname = \"c\"\npath = \"elsewhere/c.rs\"\n";
    let (_, t) = test_targets(on, &dir).expect("a package");
    assert_eq!(t.into_iter().collect::<Vec<_>>(), ["a", "b", "c", "suite"]);

    assert!(test_targets("[workspace]\nmembers = [\"crates/*\"]\n", &dir).is_none());
}

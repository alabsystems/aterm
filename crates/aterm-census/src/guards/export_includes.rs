// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! export-includes: a file the export publishes compiles in only files the export publishes.

use std::collections::BTreeSet;

use super::{Bound, Corpus, Finding, Rule, SourceFile, Tree, hits, literal, open_delim, token};
use crate::export::{ASTREAM, MANIFEST, Selection, astream_carries};

pub(super) const EXPORT_INCLUDES: Rule = Rule {
    id: "export-includes",
    why: "the public snapshot carries what publish/manifest.txt selects and, of vendor/astream, \
          astream's root Cargo.toml and each crate's tree but its tests/, examples/ and benches/; \
          an exported file whose include!, include_str! or include_bytes! reads a file it leaves \
          out does not compile there (a test module's too: its crate's tests and every \
          workspace lint then fail in a public clone), and no gate builds every target of one. \
          Withhold the file with what it reads, as the manifest withholds a test with its corpus",
    scope: &[],
    check: export_includes,
};

/// The include macros, each of whose argument is a path read at compile time.
const MACROS: [&str; 3] = ["include", "include_str", "include_bytes"];

fn export_includes(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let manifest = match crate::tree::read_to_string(&tree.root.join(MANIFEST)) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            out.push(finding(
                MANIFEST,
                0,
                format!("unreadable ({e}), so the rule cannot run"),
            ));
            return;
        }
    };
    held_to(tree.corpus, manifest.as_deref(), out);
}

fn finding(path: &str, line: usize, detail: String) -> Finding {
    Finding {
        rule: EXPORT_INCLUDES.id,
        path: path.to_string(),
        line,
        detail,
    }
}

/// The rule over `corpus`, with the export selecting what `manifest` does (everything
/// outside the central denies when there is none, as the export-content guard reads it).
fn held_to(corpus: &Corpus, manifest: Option<&str>, out: &mut Vec<Finding>) {
    let selection = match manifest.map_or_else(|| Ok(Selection::everything()), Selection::parse) {
        Ok(selection) => selection,
        Err((line, why)) => {
            out.push(finding(MANIFEST, line, why));
            return;
        }
    };
    let present: BTreeSet<&str> = corpus.paths.iter().map(String::as_str).collect();
    let astream = format!("{ASTREAM}/");
    for file in &corpus.files {
        if !selection.selects(&file.rel) || !file.mentions(&MACROS) {
            continue;
        }
        for (at, target) in includes(file, &present) {
            let left_out = match target.strip_prefix(&astream) {
                Some(inner) => (!astream_carries(inner)).then(|| {
                    format!(
                        "the export's {ASTREAM} carries astream's root Cargo.toml and each \
                         crate's tree but its tests/, examples/ and benches/"
                    )
                }),
                None => (!selection.selects(&target)).then(|| format!("{MANIFEST} withholds it")),
            };
            if let Some(why) = left_out {
                let line = file.raw[..at].matches('\n').count() + 1;
                out.push(file.finding(
                    EXPORT_INCLUDES.id,
                    line,
                    format!("includes `{target}`, which the export leaves out: {why}"),
                ));
            }
        }
    }
}

/// Each file `file` includes that `present` holds, with the byte offset of its macro: a
/// literal argument against the file's directory, a `concat!` of
/// `env!("CARGO_MANIFEST_DIR")` and literals against its crate's (the nearest directory
/// above it with a `Cargo.toml`). Any other argument (`OUT_DIR`, a macro's parameter)
/// names no checkout file this can read, and a path that climbs out of the checkout none.
fn includes(file: &SourceFile, present: &BTreeSet<&str>) -> Vec<(usize, String)> {
    let lexed = file.lexed();
    let (code, text) = (lexed.code.as_str(), lexed.text.as_str());
    let dir = file.rel.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut out = Vec::new();
    for name in MACROS {
        for at in hits(code, name, Bound::Word) {
            let Some((open, close_delim)) =
                token(code, at + name.len(), "!").and_then(|p| open_delim(code, p))
            else {
                continue;
            };
            let Some((from_crate, path)) = argument(code, text, open, close_delim) else {
                continue;
            };
            let base = if from_crate {
                let Some(root) = crate_dir(dir, present) else {
                    continue;
                };
                root
            } else if path.starts_with('/') {
                continue;
            } else {
                dir
            };
            if let Some(target) = resolved(base, &path).filter(|t| present.contains(t.as_str())) {
                out.push((at, target));
            }
        }
    }
    out.sort();
    out
}

/// What an include's argument names, from `at` in the CODE view (just inside its
/// opening delimiter, closed by `close_delim`): `(false, path)` for a string literal,
/// `(true, path)` for a `concat!` whose first argument is `env!("CARGO_MANIFEST_DIR")`
/// and whose others are string literals.
fn argument(code: &str, text: &str, at: usize, close_delim: &str) -> Option<(bool, String)> {
    if let Some((path, next)) = literal(code, text, at) {
        let end = token(code, next, ",")
            .and_then(|p| token(code, p, close_delim))
            .or_else(|| token(code, next, close_delim));
        return end.map(|_| (false, path.to_string()));
    }
    let mut at = token(code, at, "concat")?;
    at = token(code, at, "!")?;
    let (open_concat, close_concat) = open_delim(code, at)?;
    at = token(code, open_concat, "env")?;
    at = token(code, at, "!")?;
    let (open_env, close_env) = open_delim(code, at)?;
    let (var, next) = literal(code, text, open_env)?;
    if var != "CARGO_MANIFEST_DIR" {
        return None;
    }
    at = token(code, next, ",")
        .and_then(|p| token(code, p, close_env))
        .or_else(|| token(code, next, close_env))?;
    let mut path = String::new();
    loop {
        if let Some(after_concat) = token(code, at, close_concat) {
            let end = token(code, after_concat, ",")
                .and_then(|p| token(code, p, close_delim))
                .or_else(|| token(code, after_concat, close_delim));
            return end.map(|_| (true, path));
        }
        at = token(code, at, ",")?;
        if let Some(after_concat) = token(code, at, close_concat) {
            let end = token(code, after_concat, ",")
                .and_then(|p| token(code, p, close_delim))
                .or_else(|| token(code, after_concat, close_delim));
            return end.map(|_| (true, path));
        }
        let (part, next) = literal(code, text, at)?;
        path.push_str(part);
        at = next;
    }
}

/// The nearest directory at or above `dir` that holds a `Cargo.toml`.
fn crate_dir<'a>(dir: &'a str, present: &BTreeSet<&str>) -> Option<&'a str> {
    let mut dir = dir;
    loop {
        let manifest = if dir.is_empty() {
            "Cargo.toml".to_string()
        } else {
            format!("{dir}/Cargo.toml")
        };
        if present.contains(manifest.as_str()) {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = dir.rsplit_once('/').map_or("", |(up, _)| up);
    }
}

/// `path` under `dir`, `.` and `..` resolved; `None` when it climbs out of the checkout.
fn resolved(dir: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::super::tests::spots;
    use super::*;

    const MANIFEST_TEXT: &str = "Cargo.toml\ncrates\nvendor\n!vendor/astream\n\
                                 !crates/b/tests/withheld.rs\n!crates/b/tests/corpus\n";

    fn run(files: &[(&str, &str)]) -> Vec<Finding> {
        let corpus = Corpus::of(files);
        let mut found = Vec::new();
        held_to(&corpus, Some(MANIFEST_TEXT), &mut found);
        found
    }

    #[test]
    fn an_exported_file_includes_only_what_the_export_carries() {
        let lib = "#[cfg(test)]\nmod t {\n\
                   const A: &str = include_str!(concat!(\n    env!(\"CARGO_MANIFEST_DIR\"),\n    \
                   \"/../../vendor/astream/crates/astream-x/tests/fixtures/a.txt\"\n));\n\
                   const B: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \
                   \"/../../vendor/astream/\", \"evidence/manifest.toml\",));\n\
                   const C: &[u8] = include_bytes!(\"../../../vendor/astream/crates/astream-x/src/data.bin\");\n\
                   const D: &str = include_str!(\"../tests/corpus/one.txt\");\n\
                   const E: &str = include_str!(\"fixtures/ok.txt\");\n\
                   const F: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/gen.rs\"));\n\
                   // include_str!(\"../tests/corpus/one.txt\") in a comment is no include\n\
                   const G: &str = \"include_str!(\\\"../tests/corpus/one.txt\\\")\";\n\
                   const H: &str = include_str!(\"../../../docs/notes.md\");\n\
                   const I: &str = include_str!(\"../../../vendor/astream/Cargo.toml\");\n\
                   const J: &str = include_str!(\"../../../../../outside.txt\");\n\
                   const K: &str = include_str![\"../tests/corpus/one.txt\"];\n\
                   const L: &str = include_str!{concat![env!(\"CARGO_MANIFEST_DIR\"), \"/../../vendor/astream/evidence/manifest.toml\"]};\n}\n";
        let found = run(&[
            ("Cargo.toml", ""),
            ("crates/b/Cargo.toml", ""),
            ("crates/b/src/lib.rs", lib),
            ("crates/b/src/fixtures/ok.txt", ""),
            ("crates/b/tests/corpus/one.txt", ""),
            // A withheld test reads the withheld corpus: neither is exported.
            (
                "crates/b/tests/withheld.rs",
                "const X: &str = include_str!(\"corpus/one.txt\");\n",
            ),
            ("docs/notes.md", ""),
            ("vendor/astream/Cargo.toml", ""),
            ("vendor/astream/evidence/manifest.toml", ""),
            ("vendor/astream/crates/astream-x/Cargo.toml", ""),
            ("vendor/astream/crates/astream-x/src/data.bin", ""),
            ("vendor/astream/crates/astream-x/tests/fixtures/a.txt", ""),
        ]);
        assert_eq!(
            spots(&found),
            [
                "crates/b/src/lib.rs:3",
                "crates/b/src/lib.rs:7",
                "crates/b/src/lib.rs:9",
                "crates/b/src/lib.rs:14",
                "crates/b/src/lib.rs:17",
                "crates/b/src/lib.rs:18",
            ],
            "{found:#?}"
        );
        assert!(
            found[0].detail.contains(
                "`vendor/astream/crates/astream-x/tests/fixtures/a.txt`, which the export leaves \
                 out: the export's vendor/astream carries"
            ),
            "{}",
            found[0].detail
        );
        assert!(
            found[1]
                .detail
                .contains("`vendor/astream/evidence/manifest.toml`"),
            "{}",
            found[1].detail
        );
        assert!(
            found[2].detail.contains(
                "`crates/b/tests/corpus/one.txt`, which the export leaves out: \
                 publish/manifest.txt withholds it"
            ),
            "{}",
            found[2].detail
        );
        assert!(
            found[3].detail.contains("`docs/notes.md`"),
            "{}",
            found[3].detail
        );
    }

    #[test]
    fn the_export_carries_astreams_root_manifest_and_each_crates_tree_but_its_targets() {
        for carried in [
            "Cargo.toml",
            "crates/astream-x/Cargo.toml",
            "crates/astream-x/src/lib.rs",
            "crates/astream-x/build.rs",
            "crates/astream-x/src/tests/fixture.txt",
        ] {
            assert!(astream_carries(carried), "{carried}");
        }
        for left in [
            "README.md",
            "evidence/manifest.toml",
            "crates/astream-x/tests/fixtures/a.txt",
            "crates/astream-x/examples/e.rs",
            "crates/astream-x/benches/b.rs",
            "crates/Cargo.toml",
            "crates/astream-x",
        ] {
            assert!(!astream_carries(left), "{left}");
        }
    }
}

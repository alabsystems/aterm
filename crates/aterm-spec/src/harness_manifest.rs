// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The `#[kani::proof]` harness manifest `trust-ir spec-link --harness-manifest`
//! reads (`{"harnesses":[{"name","span"}]}`): the data its L1 resolves a
//! `proof_name` against. `xtask harness-manifest` is the command that writes it,
//! and `aterm-gui`'s `spec_xref_closure` runs that command.

use std::io;
use std::path::{Path, PathBuf};

/// One `#[kani::proof]` harness: its fn name and a `file:line:1` span (opaque to
/// L1, which matches only on `name`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Harness {
    pub name: String,
    pub span: String,
}

/// Enumerate every `#[kani::proof] fn` under `<root>/crates` and write the
/// manifest to `<out_dir>/harness-manifest.json`, returning that path.
pub fn write(root: &Path, out_dir: &Path) -> io::Result<PathBuf> {
    let harnesses = scan(root)?;
    std::fs::create_dir_all(out_dir)?;
    let path = out_dir.join("harness-manifest.json");
    std::fs::write(&path, render(&harnesses))?;
    Ok(path)
}

/// Every harness under `<root>/crates`, de-duplicated by name (the L1 key) and
/// sorted by it.
///
/// A line walk, as the harnesses are authored: a `#[kani::proof…]` attribute
/// arms the next `fn <ident>`, with further attribute, blank and comment lines
/// allowed between them; any other line disarms it.
pub fn scan(root: &Path) -> io::Result<Vec<Harness>> {
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files)?;
    files.sort();
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        let rel = file.strip_prefix(root).unwrap_or(file).to_string_lossy();
        let mut armed = false;
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim_start();
            if line.starts_with("#[kani::proof") {
                armed = true;
                continue;
            }
            if !armed || line.starts_with("#[") || line.is_empty() || line.starts_with("//") {
                continue;
            }
            armed = false;
            if let Some(name) = parse_fn_name(line)
                && seen.insert(name.clone())
            {
                out.push(Harness {
                    name,
                    span: format!("{rel}:{}:1", i + 1),
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Every `*.rs` under `dir`, skipping `target/` and hidden directories (a
/// worktree under `.claude/worktrees/` is a whole second checkout).
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map_or(&b""[..], std::ffi::OsStr::as_encoded_bytes);
        if path.is_dir() {
            if name != b"target" && !matches!(name, [b'.', ..]) {
                collect_rs_files(&path, out)?;
            }
        } else if path
            .extension()
            .is_some_and(|e| e.as_encoded_bytes() == b"rs")
        {
            out.push(path);
        }
    }
    Ok(())
}

/// `<ident>` out of a `(pub )?(unsafe )?fn <ident>…` line; `None` otherwise.
fn parse_fn_name(line: &str) -> Option<String> {
    let mut rest = line;
    for kw in [
        "pub ",
        "pub(crate) ",
        "unsafe ",
        "const ",
        "async ",
        "extern ",
    ] {
        if let Some(s) = rest.strip_prefix(kw) {
            rest = s.trim_start();
        }
    }
    let ident: String = rest
        .strip_prefix("fn ")?
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
}

/// The manifest JSON, hand-rolled (no serde here), each value escaped.
fn render(harnesses: &[Harness]) -> String {
    let mut s = String::from("{\n  \"harnesses\": [");
    for (i, h) in harnesses.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("\n    { \"name\": ");
        push_json_str(&mut s, &h.name);
        s.push_str(", \"span\": ");
        push_json_str(&mut s, &h.span);
        s.push_str(" }");
    }
    if !harnesses.is_empty() {
        s.push_str("\n  ");
    }
    s.push_str("]\n}\n");
    s
}

fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("aterm-spec-harness-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, text).expect("write");
        }
        root
    }

    #[test]
    fn a_proof_attribute_arms_the_next_fn_through_attrs_and_comments() {
        let root = tree(
            "arm",
            &[
                (
                    "crates/a/src/lib.rs",
                    "#[kani::proof]\n#[kani::unwind(3)]\n// why\n\npub fn ring_push() {}\n\
                     #[kani::proof]\nlet x = 1;\nfn not_a_harness() {}\n",
                ),
                // A duplicate name is one harness; target/ and dot-dirs are not read.
                (
                    "crates/b/src/lib.rs",
                    "#[kani::proof]\nunsafe fn ring_push() {}\n",
                ),
                (
                    "crates/b/target/x.rs",
                    "#[kani::proof]\nfn in_target() {}\n",
                ),
                ("crates/.wt/x.rs", "#[kani::proof]\nfn in_hidden() {}\n"),
            ],
        );
        let found = scan(&root).expect("scan");
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            found,
            vec![Harness {
                name: "ring_push".into(),
                span: "crates/a/src/lib.rs:5:1".into(),
            }]
        );
    }

    #[test]
    fn the_manifest_is_the_shape_spec_link_reads_and_escapes_its_values() {
        assert_eq!(render(&[]), "{\n  \"harnesses\": []\n}\n");
        let two = [
            Harness {
                name: "a".into(),
                span: "x.rs:1:1".into(),
            },
            Harness {
                name: "b\"q".into(),
                span: "d\\y.rs:2:1".into(),
            },
        ];
        assert_eq!(
            render(&two),
            "{\n  \"harnesses\": [\n    { \"name\": \"a\", \"span\": \"x.rs:1:1\" },\n    \
             { \"name\": \"b\\\"q\", \"span\": \"d\\\\y.rs:2:1\" }\n  ]\n}\n"
        );
    }

    #[test]
    fn write_puts_the_manifest_under_the_out_dir() {
        let root = tree(
            "write",
            &[("crates/a/src/lib.rs", "#[kani::proof]\nfn h() {}\n")],
        );
        let out = write(&root, &root.join("target/trust")).expect("write");
        let text = std::fs::read_to_string(&out).expect("read");
        let _ = std::fs::remove_dir_all(&root);
        assert!(out.ends_with("target/trust/harness-manifest.json"));
        assert!(text.contains("\"name\": \"h\""), "{text}");
    }
}

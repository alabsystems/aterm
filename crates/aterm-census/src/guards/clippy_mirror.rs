// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! clippy-mirror: a crate's own `clippy.toml` repeats, word for word, every workspace row that
//! can name a call in that crate.
//!
//! Clippy lints a package by the nearest `clippy.toml` (or `.clippy.toml`) at or above its
//! manifest's folder and never merges it with the workspace's, so a crate that keeps a file for
//! a law of its own (the keeper maps no region, the journal reads no clock) lints by that file
//! alone. A row the workspace file gains (the kept-out pipe rows, a chosen flush) is off in such
//! a crate until its file repeats it, and nothing says so: the bare pipe the workspace refuses
//! lints clean there.
//!
//! A crate's file repeats each workspace row of `std`, `core` and `alloc`, and each workspace row
//! of another crate it names in a row of its own or that one of its packages depends on (any
//! target; dev and build dependencies too, since clippy lints tests and build scripts), in the
//! same list and the same words. A file that lists no row opts out of the workspace's rows whole
//! and says why (aterm-libc's, whose crate cannot name itself `libc`); a file that is no
//! package's nearest (aterm-time's `wasm32/`, which the browser cells name by `CLIPPY_CONF_DIR`)
//! is not a crate's file.
//!
//! Findings: a workspace row a crate's file leaves out, or words otherwise; a list entry either
//! file holds other than as one `{ path = "…", … }` line, which this rule cannot read.

use std::collections::{BTreeMap, BTreeSet};

use super::{Finding, Rule, Tree};

pub(super) const CLIPPY_MIRROR: Rule = Rule {
    id: "clippy-mirror",
    why: "clippy reads only the nearest clippy.toml and never merges it with the workspace's: a \
          crate's own file that leaves out a workspace row of std or core, or of a crate it names \
          or depends on, turns that row off in the crate, so a call the workspace refuses (a bare \
          pipe, an unchosen flush) lints clean there",
    scope: &[],
    check: clippy_mirror,
};

/// The names clippy looks for in each folder, in the order it takes them.
const NAMES: [&str; 2] = [".clippy.toml", "clippy.toml"];

/// The lists whose entries are path rows.
const LISTS: [&str; 3] = [
    "disallowed-methods",
    "disallowed-types",
    "disallowed-macros",
];

/// The crates every package loads: their workspace rows hold in every crate's file.
const EVERYWHERE: [&str; 3] = ["std", "core", "alloc"];

/// One row of a path list.
struct Row<'a> {
    list: &'a str,
    path: &'a str,
    /// The row as written, trimmed, without its trailing comma.
    text: &'a str,
    line: usize,
}

/// A `clippy.toml`'s rows, the line each list opens on, and the list entries it could not read.
#[derive(Default)]
struct Rows<'a> {
    rows: Vec<Row<'a>>,
    opens: BTreeMap<&'a str, usize>,
    unread: Vec<(&'a str, usize)>,
}

/// The rows of `text`, one `{ path = "…", … }` entry per line inside each of [`LISTS`].
fn rows(text: &str) -> Rows<'_> {
    let mut out = Rows::default();
    let mut open: Option<&str> = None;
    for (at, raw) in text.lines().enumerate() {
        let (line, t) = (at + 1, raw.trim());
        let Some(list) = open else {
            let Some((key, rest)) = t.split_once('=') else {
                continue;
            };
            let Some(list) = LISTS.into_iter().find(|l| *l == key.trim()) else {
                continue;
            };
            out.opens.insert(list, line);
            match rest.trim() {
                "[" => open = Some(list),
                "[]" => {}
                _ => out.unread.push((list, line)),
            }
            continue;
        };
        if t == "]" {
            open = None;
            continue;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let body = t.strip_suffix(',').unwrap_or(t);
        let path = body
            .strip_prefix('{')
            .filter(|_| body.ends_with('}'))
            .and_then(|inner| inner.split_once("path = \""))
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(path, _)| path);
        match path {
            Some(path) => out.rows.push(Row {
                list,
                path,
                text: body,
                line,
            }),
            None => out.unread.push((list, line)),
        }
    }
    out
}

/// The crate a row's path names: its first segment.
fn krate(path: &str) -> &str {
    path.split("::").next().unwrap_or(path)
}

/// The crates a manifest names in a dependency table (`[dependencies]`, `[dev-dependencies]`,
/// `[build-dependencies]`, each under any `[target.…]`, or one dependency's own table), as code
/// spells them (`-` read as `_`).
fn dependencies(manifest: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_table = false;
    for raw in manifest.lines() {
        let t = raw.trim();
        if t.starts_with('[') {
            let head = t.trim_start_matches('[').trim_end_matches(']').trim();
            in_table = head.ends_with("dependencies");
            if let Some((_, name)) = head.rsplit_once("dependencies.") {
                out.insert(name.trim_matches('"').replace('-', "_"));
            }
            continue;
        }
        if !in_table || t.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = t.split_once('=') {
            let key = key.trim();
            let name = key.split('.').next().unwrap_or(key).trim_matches('"');
            if !name.is_empty() {
                out.insert(name.replace('-', "_"));
            }
        }
    }
    out
}

/// The lint file clippy reads in `dir` ("" for the checkout's root), if one is there.
fn file_in<'a>(dir: &str, present: &BTreeSet<&'a str>) -> Option<&'a str> {
    NAMES.into_iter().find_map(|name| {
        let rel = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        };
        present.get(rel.as_str()).copied()
    })
}

/// The lint file nearest the package in `dir` below the checkout's root, if it has one.
fn nearest<'a>(dir: &str, present: &BTreeSet<&'a str>) -> Option<&'a str> {
    let mut at = dir;
    while !at.is_empty() {
        if let Some(file) = file_in(at, present) {
            return Some(file);
        }
        at = at.rsplit_once('/').map_or("", |(up, _)| up);
    }
    None
}

/// The packages one crate file lints, and every crate their manifests depend on.
#[derive(Default)]
struct Linted {
    packages: Vec<String>,
    depends: BTreeSet<String>,
}

fn clippy_mirror(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let paths = tree.corpus.paths();
    let present: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let finding = |path: &str, line: usize, detail: String| Finding {
        rule: CLIPPY_MIRROR.id,
        path: path.to_string(),
        line,
        detail,
    };
    let Some(root_file) = file_in("", &present) else {
        return;
    };
    let Some(root_text) = tree.text(root_file) else {
        out.push(finding(
            root_file,
            0,
            "cannot be read, so no crate's lint file can be held to its rows".to_string(),
        ));
        return;
    };
    let root = rows(&root_text);
    unread(root_file, &root, out);
    let mut files: BTreeMap<&str, Linted> = BTreeMap::new();
    for (manifest, text) in tree.corpus.manifests() {
        let dir = manifest.strip_suffix("/Cargo.toml").unwrap_or(manifest);
        let Some(file) = nearest(dir, &present) else {
            continue;
        };
        let linted = files.entry(file).or_default();
        linted.packages.push(dir.to_string());
        linted.depends.extend(dependencies(text));
    }
    for (file, linted) in files {
        let Some(text) = tree.text(file) else {
            out.push(finding(
                file,
                0,
                "cannot be read, so its rows cannot be held to the workspace's".to_string(),
            ));
            continue;
        };
        let own = rows(&text);
        unread(file, &own, out);
        if own.rows.is_empty() && own.unread.is_empty() {
            continue;
        }
        let named: BTreeSet<&str> = own.rows.iter().map(|r| krate(r.path)).collect();
        let held: BTreeSet<(&str, &str)> = own.rows.iter().map(|r| (r.list, r.text)).collect();
        let by_path: BTreeMap<(&str, &str), usize> = own
            .rows
            .iter()
            .map(|r| ((r.list, r.path), r.line))
            .collect();
        let packages = linted.packages.join(", ");
        for row in &root.rows {
            let of = krate(row.path);
            let holds =
                EVERYWHERE.contains(&of) || named.contains(of) || linted.depends.contains(of);
            if !holds || held.contains(&(row.list, row.text)) {
                continue;
            }
            out.push(match by_path.get(&(row.list, row.path)) {
                Some(&line) => finding(
                    file,
                    line,
                    format!(
                        "words the workspace's {} row `{}` otherwise: repeat it word for word",
                        row.list, row.path
                    ),
                ),
                None => finding(
                    file,
                    own.opens.get(row.list).copied().unwrap_or(0),
                    format!(
                        "leaves out the workspace's {} row `{}`, so clippy never applies it in \
                         {packages}: repeat it word for word",
                        row.list, row.path
                    ),
                ),
            });
        }
    }
}

/// One finding per list entry of `file` that [`rows`] could not read.
fn unread(file: &str, rows: &Rows<'_>, out: &mut Vec<Finding>) {
    for &(list, line) in &rows.unread {
        out.push(Finding {
            rule: CLIPPY_MIRROR.id,
            path: file.to_string(),
            line,
            detail: format!(
                "a {list} entry this rule reads only as one `{{ path = \"…\", … }}` line"
            ),
        });
    }
}

#[cfg(test)]
#[path = "clippy_mirror_tests.rs"]
mod tests;

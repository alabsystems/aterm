// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! unmounted-test: every `#[test]` and every `tests/` source file compiles in a Cargo target.
//!
//! Two Cargo shapes silently drop tests without a compiler warning:
//! - `[lib]` or `[[bin]]` sets `test = false` (or `harness = false`), or a module file in `src/`
//!   is never loaded by a target whose libtest harness runs, leaving its `#[test]` functions dead;
//! - `[package]` sets `autotests = false` (or a file sits under `tests/<sub/>`), and neither a
//!   `[[test]]` table nor a `mod` / `#[path]` / `include!` from a target root mounts the file.

use std::collections::{BTreeMap, BTreeSet};

use super::rules::PRODUCT;
use super::stock_label::where_cargo_looks;
use super::{Finding, Reach, Rule, SourceFile, Tree, after_ws, joined};
use crate::scan_set::ManifestTargets;

pub(super) const UNMOUNTED_TEST: Rule = Rule {
    id: "unmounted-test",
    why: "a `#[test]` in a target Cargo never runs with libtest (`[lib] test = false`, \
          `harness = false`, or an unloaded module) and a `tests/*.rs` file unmounted under \
          `autotests = false` silently never compile or run",
    scope: &[],
    check: unmounted_test,
};

pub(super) struct PkgRoots {
    /// Every target's root: library, binaries, tests, benches, examples, build script.
    pub(super) all: BTreeSet<String>,
    libtest: BTreeSet<String>,
    /// The roots a build without tests compiles: library, binaries, build script.
    pub(super) shipped: BTreeSet<String>,
    /// The build script's root, if the package has one.
    pub(super) build: Option<String>,
}

fn unmounted_test(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    let pkgs = load_packages(tree, out);
    if pkgs.is_empty() {
        return;
    }
    let dirs: Vec<&str> = pkgs.keys().map(String::as_str).collect();
    let mut owned: BTreeMap<&str, Vec<&SourceFile>> = BTreeMap::new();
    for file in tree.corpus.under(PRODUCT) {
        if let Some(dir) = owner_dir(&file.rel, &dirs) {
            owned.entry(dir).or_default().push(file);
        }
    }
    let mut all_reach = BTreeSet::new();
    let mut libtest_reach = BTreeSet::new();
    for (dir, targets) in &pkgs {
        let files = owned.get(dir.as_str()).map_or(&[][..], Vec::as_slice);
        let roots = pkg_roots(dir, targets, files);
        // Every libtest root is a root of some target, so what the libtest roots reach is
        // reached again from every root; the walk goes on from there, once.
        let mut reach = Reach::new(tree.corpus, dir, false);
        reach.from(&roots.libtest);
        let libtest = reach.files();
        if roots.all.len() == roots.libtest.len() {
            all_reach.extend(libtest.iter().cloned());
        } else {
            reach.from(&roots.all);
            all_reach.extend(reach.files());
        }
        libtest_reach.extend(libtest);
    }
    for dir in pkgs.keys() {
        let files = owned.get(dir.as_str()).map_or(&[][..], Vec::as_slice);
        let tests_prefix = format!("{dir}/tests/");
        for &file in files {
            if file.rel.starts_with(&tests_prefix) && !all_reach.contains(&file.rel) {
                out.push(file.finding(
                    UNMOUNTED_TEST.id,
                    0,
                    format!(
                        "under `{tests_prefix}` of `{dir}/Cargo.toml`, which compiles it in no \
                         Cargo target (missing `[[test]]` or `mod` mount)"
                    ),
                ));
            } else if !libtest_reach.contains(&file.rel)
                && let Some(line) = first_test_attr_line(file)
            {
                out.push(file.finding(
                    UNMOUNTED_TEST.id,
                    line,
                    format!(
                        "`#[test]` is not reachable from any libtest target of `{dir}/Cargo.toml` \
                         (`test = false`, `harness = false`, or unmounted module)"
                    ),
                ));
            }
        }
    }
}

/// Every `[package]` manifest under [`PRODUCT`], keyed by its package directory.
fn load_packages(tree: &Tree<'_>, out: &mut Vec<Finding>) -> BTreeMap<String, ManifestTargets> {
    let mut pkgs = BTreeMap::new();
    for (rel, text) in tree.corpus.manifests() {
        if !PRODUCT.iter().any(|p| rel.starts_with(p)) {
            continue;
        }
        match crate::scan_set::manifest_targets(text, rel) {
            Ok(targets) if targets.package => {
                let dir = rel.strip_suffix("/Cargo.toml").unwrap_or_default();
                pkgs.insert(dir.to_string(), targets);
            }
            Ok(_) => {}
            Err(why) => out.push(Finding {
                rule: UNMOUNTED_TEST.id,
                path: rel.clone(),
                line: 0,
                detail: format!("unreadable manifest, so its targets go unread: {why}"),
            }),
        }
    }
    pkgs
}

/// The deepest package directory in `dirs` (sorted lexicographically) containing `rel`.
fn owner_dir<'a>(rel: &str, dirs: &[&'a str]) -> Option<&'a str> {
    let idx = dirs.partition_point(|&dir| dir < rel);
    dirs[..idx].iter().rev().copied().find(|&dir| {
        rel.strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Declared and Cargo-discovered target roots for package `dir`.
pub(super) fn pkg_roots(dir: &str, targets: &ManifestTargets, files: &[&SourceFile]) -> PkgRoots {
    let mut all = BTreeSet::new();
    let mut libtest = BTreeSet::new();
    let mut shipped = BTreeSet::new();
    let mut claimed = BTreeSet::new();
    let has_lib = targets.declared.iter().any(|&(k, _, _)| k == "lib");
    for ((kind, name, path), &runs) in targets.declared.iter().zip(&targets.runs_libtest) {
        let cands: Vec<String> = match path {
            Some(p) => vec![joined(dir, p)],
            None => {
                let own = name.is_some() && *name == targets.name;
                where_cargo_looks(kind, name.as_deref(), own)
                    .into_iter()
                    .map(|at| format!("{dir}/{at}"))
                    .collect()
            }
        };
        for cand in cands {
            claimed.insert(cand.clone());
            all.insert(cand.clone());
            if matches!(*kind, "lib" | "bin") {
                shipped.insert(cand.clone());
            }
            if runs {
                libtest.insert(cand);
            }
        }
    }
    let build = match &targets.build {
        Some(Some(path)) => Some(joined(dir, path)),
        Some(None) => None,
        None => files
            .iter()
            .any(|file| file.rel.len() == dir.len() + 9 && file.rel.ends_with("/build.rs"))
            .then(|| format!("{dir}/build.rs")),
    };
    for &file in files {
        let sub = &file.rel[dir.len() + 1..];
        let mut it = sub.split('/');
        let (kind, default_libtest) = match (it.next(), it.next(), it.next(), it.next(), it.next())
        {
            (Some("src"), Some("lib.rs"), None, None, None) if !has_lib => ("lib", true),
            (Some("src"), Some("main.rs"), None, None, None)
            | (Some("src"), Some("bin"), Some(_), None, None)
            | (Some("src"), Some("bin"), Some(_), Some("main.rs"), None) => ("bin", true),
            (Some("tests"), Some(_), None, None, None)
            | (Some("tests"), Some(_), Some("main.rs"), None, None) => ("test", true),
            (Some("benches"), Some(_), None, None, None)
            | (Some("benches"), Some(_), Some("main.rs"), None, None) => ("bench", false),
            (Some("examples"), Some(_), None, None, None)
            | (Some("examples"), Some(_), Some("main.rs"), None, None) => ("example", false),
            _ => continue,
        };
        if !targets.undiscovered.contains(&kind) && !claimed.contains(&file.rel) {
            all.insert(file.rel.clone());
            if matches!(kind, "lib" | "bin") {
                shipped.insert(file.rel.clone());
            }
            if default_libtest {
                libtest.insert(file.rel.clone());
            }
        }
    }
    all.extend(build.clone());
    shipped.extend(build.clone());
    PkgRoots {
        all,
        libtest,
        shipped,
        build,
    }
}

/// First 1-based line carrying a `#[test]` attribute in the CODE view (outside strings/comments).
fn first_test_attr_line(file: &SourceFile) -> Option<usize> {
    if !file.raw().contains("test") {
        return None;
    }
    file.lines()
        .find(|line| code_line_has_test_attr(line.code))
        .map(|line| line.no)
}

fn code_line_has_test_attr(code: &str) -> bool {
    for (at, _) in code.match_indices('#') {
        let Some(inner) = after_ws(&code[at + 1..], "[") else {
            continue;
        };
        let inner = inner.trim_start_matches([' ', '\t']);
        let rest = inner
            .strip_prefix("test")
            .or_else(|| inner.strip_prefix("core::prelude::v1::test"))
            .or_else(|| inner.strip_prefix("std::prelude::v1::test"));
        if rest.and_then(|r| after_ws(r, "]")).is_some() {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[path = "unmounted_test_tests.rs"]
mod tests;

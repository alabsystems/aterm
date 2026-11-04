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

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::rules::PRODUCT;
use super::stock_label::where_cargo_looks;
use super::{Corpus, Finding, LoadKind, Rule, SourceFile, Tree, after_ws, joined};
use crate::scan_set::ManifestTargets;

pub(super) const UNMOUNTED_TEST: Rule = Rule {
    id: "unmounted-test",
    why: "a `#[test]` in a target Cargo never runs with libtest (`[lib] test = false`, \
          `harness = false`, or an unloaded module) and a `tests/*.rs` file unmounted under \
          `autotests = false` silently never compile or run",
    scope: &[],
    check: unmounted_test,
};

struct PkgRoots {
    all: BTreeSet<String>,
    libtest: BTreeSet<String>,
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
        all_reach.extend(reachable(tree.corpus, dir, &roots.all));
        libtest_reach.extend(reachable(tree.corpus, dir, &roots.libtest));
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
            } else if let Some(line) = first_test_attr_line(file)
                && !libtest_reach.contains(&file.rel)
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
    for (rel, text) in &tree.corpus.manifests {
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

/// The deepest package directory in `dirs` containing `rel`.
fn owner_dir<'a>(rel: &str, dirs: &[&'a str]) -> Option<&'a str> {
    dirs.iter()
        .copied()
        .filter(|dir| {
            rel.strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|dir| dir.len())
}

/// Declared and Cargo-discovered target roots for package `dir`.
fn pkg_roots(dir: &str, targets: &ManifestTargets, files: &[&SourceFile]) -> PkgRoots {
    let mut all = BTreeSet::new();
    let mut libtest = BTreeSet::new();
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
            if runs {
                libtest.insert(cand);
            }
        }
    }
    for &file in files {
        let sub = &file.rel[dir.len() + 1..];
        let parts: Vec<&str> = sub.split('/').collect();
        if parts.as_slice() == ["build.rs"] {
            all.insert(file.rel.clone());
            continue;
        }
        let (kind, default_libtest) = match parts.as_slice() {
            ["src", "lib.rs"] if !has_lib => ("lib", true),
            ["src", "main.rs"] | ["src", "bin", _] | ["src", "bin", _, "main.rs"] => ("bin", true),
            ["tests", _] | ["tests", _, "main.rs"] => ("test", true),
            ["benches", _] | ["benches", _, "main.rs"] => ("bench", false),
            ["examples", _] | ["examples", _, "main.rs"] => ("example", false),
            _ => continue,
        };
        if !targets.undiscovered.contains(&kind) && !claimed.contains(&file.rel) {
            all.insert(file.rel.clone());
            if default_libtest {
                libtest.insert(file.rel.clone());
            }
        }
    }
    PkgRoots { all, libtest }
}

/// Transitive closure of files in `corpus` loaded from `roots` (`mod`, `#[path]`, `include!`).
fn reachable(corpus: &Corpus, manifest_dir: &str, roots: &BTreeSet<String>) -> BTreeSet<String> {
    let mut seen: BTreeSet<(String, bool)> = BTreeSet::new();
    let mut queue: VecDeque<(String, bool)> = VecDeque::new();
    for root in roots {
        if corpus.get(root).is_some() && seen.insert((root.clone(), true)) {
            queue.push_back((root.clone(), true));
        }
    }
    while let Some((rel, dir_owner)) = queue.pop_front() {
        let Some(file) = corpus.get(&rel) else {
            continue;
        };
        for load in file.loads_in(dir_owner, Some(manifest_dir)) {
            for child in load.files {
                let Some(target) = corpus.get(&child) else {
                    continue;
                };
                let child_owner = match load.kind {
                    LoadKind::Mod => target.name() == "mod.rs",
                    LoadKind::PathMod => true,
                    LoadKind::Include => dir_owner,
                };
                if seen.insert((child.clone(), child_owner)) {
                    queue.push_back((child, child_owner));
                }
            }
        }
    }
    seen.into_iter().map(|(rel, _)| rel).collect()
}

/// First 1-based line carrying a `#[test]` attribute in the CODE view (outside strings/comments).
fn first_test_attr_line(file: &SourceFile) -> Option<usize> {
    if !file.raw.contains("test") {
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
mod tests {
    use super::UNMOUNTED_TEST;
    use crate::guards::tests::{run, spots};

    /// NEGATIVE CONTROL: `[lib] test = false` silently drops inline `#[test]` functions in `src/`
    /// unless an integration test target mounts the module.
    #[test]
    fn lib_test_false_flags_orphaned_unit_test_and_passes_path_mounted_module() {
        let found = run(
            &UNMOUNTED_TEST,
            &[
                (
                    "crates/a/Cargo.toml",
                    "[package]\nname = \"a\"\n\n[lib]\ntest = false\n",
                ),
                (
                    "crates/a/src/lib.rs",
                    "mod orphan;\nmod shared;\n/// Doc mentioning `#[test]` in a comment.\npub fn f() {}\n",
                ),
                (
                    "crates/a/src/orphan.rs",
                    "#[cfg(test)]\nmod tests {\n    #[test]\n    fn dead() {}\n}\n",
                ),
                ("crates/a/src/shared.rs", "#[test]\nfn runs_in_it() {}\n"),
                (
                    "crates/a/tests/it.rs",
                    "#[path = \"../src/shared.rs\"]\nmod shared;\n",
                ),
            ],
        );
        assert_eq!(spots(&found), ["crates/a/src/orphan.rs:3"]);
    }

    /// NEGATIVE CONTROL: `autotests = false` silently drops any `tests/*.rs` file that is neither
    /// a `[[test]]` root nor mounted by `mod` / `#[path]` / `include!`.
    #[test]
    fn autotests_false_flags_unlisted_test_file_and_passes_suite_modules() {
        let found = run(
            &UNMOUNTED_TEST,
            &[
                (
                    "crates/b/Cargo.toml",
                    "[package]\nname = \"b\"\nautotests = false\n\n[[test]]\nname = \"suite\"\npath = \"tests/suite.rs\"\n",
                ),
                ("crates/b/src/lib.rs", "pub fn ok() {}\n"),
                ("crates/b/tests/suite.rs", "mod mounted;\n"),
                ("crates/b/tests/mounted.rs", "#[test]\nfn live() {}\n"),
                ("crates/b/tests/forgotten.rs", "#[test]\nfn dead() {}\n"),
            ],
        );
        assert_eq!(spots(&found), ["crates/b/tests/forgotten.rs:0"]);
    }

    /// NEGATIVE CONTROL: an unloaded `src/extra_tests.rs` or a `harness = false` target holding
    /// `#[test]` is flagged, while a nested Cargo package under `tests/toy/` owns its own tree.
    #[test]
    fn unloaded_module_and_harness_false_are_flagged_while_nested_package_is_isolated() {
        let found = run(
            &UNMOUNTED_TEST,
            &[
                (
                    "crates/c/Cargo.toml",
                    "[package]\nname = \"c\"\n\n[[test]]\nname = \"drv\"\npath = \"tests/drv.rs\"\nharness = false\n",
                ),
                (
                    "crates/c/src/lib.rs",
                    "pub fn live() { let _ = \"#[test]\"; }\n",
                ),
                (
                    "crates/c/src/dangling_tests.rs",
                    "#[test]\nfn never_loaded() {}\n",
                ),
                (
                    "crates/c/tests/drv.rs",
                    "#[test]\nfn no_harness() {}\nfn main() {}\n",
                ),
                (
                    "crates/c/tests/toy/Cargo.toml",
                    "[package]\nname = \"toy\"\n",
                ),
                (
                    "crates/c/tests/toy/src/lib.rs",
                    "#[test]\nfn toy_unit() {}\n",
                ),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/c/src/dangling_tests.rs:1",
                "crates/c/tests/drv.rs:1",
            ]
        );
    }

    /// NEGATIVE CONTROL: Cargo skips `examples/*.rs` under `targo test --tests` unless `[[example]]`
    /// sets `test = true`, whereas cross-crate `#[path]` plus `CARGO_MANIFEST_DIR` `include!` resolves
    /// per compiling package.
    #[test]
    fn example_without_test_true_is_flagged_and_manifest_dir_include_resolves_per_pkg() {
        let found = run(
            &UNMOUNTED_TEST,
            &[
                (
                    "crates/d/Cargo.toml",
                    "[package]\nname = \"d\"\n\n[[example]]\nname = \"live_ex\"\ntest = true\n",
                ),
                ("crates/d/src/lib.rs", "mod api;\npub fn d() {}\n"),
                (
                    "crates/d/src/api.rs",
                    "#[cfg(test)]\nmod tests {\n    include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/api_tests.rs\"));\n}\n",
                ),
                ("crates/d/src/api_tests.rs", "#[test]\nfn d_api() {}\n"),
                (
                    "crates/d/examples/live_ex.rs",
                    "fn main() {}\n#[test]\nfn ok() {}\n",
                ),
                (
                    "crates/d/examples/dead_ex.rs",
                    "fn main() {}\n#[test]\nfn skipped() {}\n",
                ),
                ("crates/e/Cargo.toml", "[package]\nname = \"e\"\n"),
                (
                    "crates/e/src/lib.rs",
                    "#[path = \"../../d/src/api.rs\"]\nmod api;\n",
                ),
                ("crates/e/src/api_tests.rs", "#[test]\nfn e_api() {}\n"),
            ],
        );
        assert_eq!(spots(&found), ["crates/d/examples/dead_ex.rs:2"]);
    }
}

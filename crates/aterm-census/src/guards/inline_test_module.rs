// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! inline-test-module: no build without tests reads test code it then strips.
//!
//! rustc strips a test-only item only after reading it. A test module written inline
//! (`#[cfg(test)] mod tests { … }`) is part of a file every build reads, so it is in that build's
//! dep-info, and Cargo's content freshness (`checksum-freshness`) sees an edit to it: the crate
//! recompiles, then every crate that links it, then the release binary's fat-LTO link (0.192.0's
//! test-only fix hop: 6 units, 1,286 s, 42 GiB per slice). Declared out of line
//! (`#[cfg(test)] #[path = "ops_tests.rs"] mod tests;`), the expander drops the declaration before
//! it loads the file, so no build without tests reads it: an edit to it leaves every release unit
//! Fresh, and the release reuses its slices on Cargo's own proof (B12's all-Fresh hop). The same
//! holds for a file whose own `#![cfg(test)]` makes it test-only, and for an inline module gated by
//! an inner `#![cfg(test)]`: rustc opens them to read that attribute unless the `mod` line that
//! mounts them carries the `cfg`. (`docs/measured/inline-test-module-2026-11-09.md`)
//!
//! What it reads: every package under `crates/` and `apps/`, whatever links it, so no graph
//! resolution can leave one out (an absorbed fork, a build-dependency, a proc macro's own
//! dependency, a `[patch]` target): each from the files Cargo may compile as its library,
//! binaries and build script, and each `.rs` file its build script names in a string literal (a
//! file it watches or mounts into a tool it compiles), following only the loads a build without
//! tests compiles ([`Reach`] with `shipped_only`). Not read: `vendor/` (astream's changes land in
//! astream first; `vendor/trust-*` are Trust's), test, bench and example targets, and test-only
//! items that are not modules, which stay where they are and keep their file in every build.
//! This guard decides only how often a test-only hop is reusable, never whether reuse is sound:
//! that is rustc's dep-info and Cargo's freshness.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::{Corpus, Finding, Reach, Rule, SourceFile, Tree};
use crate::items::{Source, TestModule};

pub(super) const INLINE_TEST_MODULE: Rule = Rule {
    id: "inline-test-module",
    why: "a build without tests reads every file its code mounts, so a test module written inline \
          in one makes a test-only edit recompile that crate, every crate that links it and the \
          release binary's fat LTO (6 units, 1,286 s, 42 GiB a slice); out of line \
          (`#[cfg(test)] #[path = \"<stem>_tests.rs\"] mod tests;`) no such build opens it",
    scope: &[],
    check: inline_test_module,
};

/// A file a build without tests opens that holds test code such a build reads and strips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unread {
    /// Repo-relative path.
    pub rel: String,
    /// The folder of the package it sits in (the deepest manifest above it), whose layout
    /// decides which files Cargo builds as targets of their own.
    pub package: String,
    /// Each target that compiles it with tests: whether it owns its directory there (a crate
    /// root, a `mod.rs`, an included or `#[path]`-mounted file, whose children resolve beside
    /// it).
    pub owns_dir: BTreeSet<bool>,
    /// Its own inner attributes make all of it test-only (`#![cfg(test)]`).
    pub whole: bool,
    /// Its inline test modules, outermost only.
    pub modules: Vec<TestModule>,
}

/// What [`unread`] could not read: a manifest or source file, by path, and why.
pub type Unreadable = (String, String);

/// Every file a build without tests opens that holds inline test code, by path, or what could
/// not be read.
pub(crate) fn unread(tree: &Tree<'_>) -> Vec<Result<Unread, Unreadable>> {
    let mut out = Vec::new();
    let mut shipped: BTreeSet<String> = BTreeSet::new();
    let mut owns: BTreeMap<String, BTreeSet<bool>> = BTreeMap::new();
    let mut packages: Vec<&str> = Vec::new();
    for (manifest, text) in tree.corpus.manifests() {
        let Some(dir) = manifest.strip_suffix("/Cargo.toml") else {
            continue;
        };
        let targets = match crate::scan_set::manifest_targets(text, manifest) {
            Ok(targets) if targets.package => targets,
            Ok(_) => continue,
            Err(why) => {
                out.push(Err((manifest.clone(), why)));
                continue;
            }
        };
        packages.push(dir);
        let prefix = format!("{dir}/");
        let under = [prefix.as_str()];
        let files: Vec<&SourceFile> = tree.corpus.under(&under).collect();
        let found = super::unmounted_test::pkg_roots(dir, &targets, &files);
        let mut roots = found.shipped.clone();
        if let Some(script) = &found.build {
            roots.extend(named_by(tree.corpus, dir, script));
        }
        let mut release = Reach::new(tree.corpus, dir, true);
        release.from(&roots);
        shipped.extend(release.files());
        // Ownership is read from every target a test build compiles, a test target's `#[path]`
        // mount of a shipped file included: that is where moved test code compiles. A build
        // script, and a file only it names, never compiles with `cfg(test)`.
        let mut every = found.all;
        if let Some(script) = &found.build {
            every.remove(script);
        }
        let mut any = Reach::new(tree.corpus, dir, false);
        any.from(&every);
        for (rel, owner) in any.reached() {
            owns.entry(rel.clone()).or_default().insert(*owner);
        }
    }
    for rel in shipped {
        let Some(file) = tree.corpus.get(&rel) else {
            continue;
        };
        let raw = file.raw();
        if !raw.contains("cfg") {
            continue;
        }
        let Some(source) = Source::read(raw.to_string()) else {
            out.push(Err((rel, "does not read as token trees".to_string())));
            continue;
        };
        if source.whole() || !source.test_modules().is_empty() {
            let package = packages
                .iter()
                .filter(|dir| {
                    rel.strip_prefix(**dir)
                        .is_some_and(|sub| sub.starts_with('/'))
                })
                .max_by_key(|dir| dir.len())
                .map_or_else(String::new, |dir| (*dir).to_string());
            out.push(Ok(Unread {
                package,
                owns_dir: owns.remove(&rel).unwrap_or_default(),
                rel,
                whole: source.whole(),
                modules: source.test_modules().to_vec(),
            }));
        }
    }
    out
}

/// [`unread`] over the checkout at `root`, read with no memo run: what `verify census --fix`
/// moves.
///
/// # Errors
/// The checkout's source cannot be read.
pub fn unread_at(root: &Path) -> Result<Vec<Result<Unread, Unreadable>>, String> {
    const NO_ENGINE: &crate::EngineBaseline = &Err(String::new());
    let corpus = Corpus::at(root)?;
    Ok(unread(&Tree::new(root, &corpus, NO_ENGINE)))
}

/// Each `.rs` file of the corpus that the build script at `script`, of the package at `dir`,
/// names in a string literal: a path Cargo reads from the package's folder (`"src/x.rs"`,
/// `"cargo:rerun-if-changed=../y/src/x.rs"`, or the tail of `"{}/src/x.rs"`), a file it watches
/// or mounts into a tool it compiles.
fn named_by(corpus: &Corpus, dir: &str, script: &str) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    let Some(source) = corpus
        .get(script)
        .and_then(|file| Source::read(file.raw().to_string()))
    else {
        return named;
    };
    for (value, _) in source.strings() {
        let Some(value) = value.filter(|v| v.ends_with(".rs")) else {
            continue;
        };
        let tail = value.rsplit_once('=').map_or(value, |(_, tail)| tail);
        let tail = tail.rsplit_once('}').map_or(tail, |(_, tail)| tail);
        let rel = super::joined(dir, tail);
        if corpus.get(&rel).is_some() {
            named.insert(rel);
        }
    }
    named
}

fn inline_test_module(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for entry in unread(tree) {
        let unread = match entry {
            Ok(unread) => unread,
            Err((rel, why)) => {
                out.push(Finding {
                    rule: INLINE_TEST_MODULE.id,
                    path: rel,
                    line: 0,
                    detail: format!("{why}, so its test code goes unjudged"),
                });
                continue;
            }
        };
        let Some(file) = tree.corpus.get(&unread.rel) else {
            continue;
        };
        if unread.whole {
            out.push(file.finding(
                INLINE_TEST_MODULE.id,
                0,
                "its own inner `cfg` makes it test-only, but a `mod` line a build without tests \
                 compiles mounts it, so that build opens it to read the attribute: gate that \
                 `mod` line with the same `cfg`"
                    .to_string(),
            ));
        }
        let raw = file.raw();
        for module in &unread.modules {
            let line = line_of(raw, module.item.0);
            let last = line_of(raw, module.item.1);
            let detail = if module.inner_gate {
                format!(
                    "declares test module `{}` (lines {line}-{last}) with its `cfg` inside it, so \
                     every build reads it: put the `cfg` on its `mod` line and move it out of line",
                    module.name
                )
            } else {
                format!(
                    "declares test module `{}` inline (lines {line}-{last}), so every build reads \
                     it; move it out of line with `targo --unverified verify census --fix`",
                    module.name
                )
            };
            out.push(file.finding(INLINE_TEST_MODULE.id, line, detail));
        }
    }
}

/// The 1-based line of byte `at` in `text`.
fn line_of(text: &str, at: usize) -> usize {
    text.as_bytes()[..at.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

#[cfg(test)]
#[path = "inline_test_module_tests.rs"]
mod tests;

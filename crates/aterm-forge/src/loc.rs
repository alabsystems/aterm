// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-package facts: how much source, how much `unsafe`, does it run code at
//! build time, what licence, and is it ours.
//!
//! # What LOC means here, exactly
//!
//! Physical lines over every `*.rs` under the package's root directory,
//! including that package's own tests, examples, benches and the files a
//! published `.crate` carries outside `src/` (`loc_method =
//! "rs-physical-all-files-v1"`). It measures the source aterm would OWN if it
//! vendored the package — not the code that reaches codegen. That choice is
//! deliberate and it is the number the budget ratchets.
//!
//! One consequence is worth stating because it is the difference between two
//! plausible totals: the walk descends into dot-directories, so
//! `smol_str-0.2.2/.github/ci.rs` counts. That single file is 122 lines, and it
//! is the ENTIRE gap between the mac-arm figure pinned in `src/measured.rs` and
//! the total a dot-directory-skipping walk would report. It ships inside the
//! `.crate` tarball, so vendoring smol_str means owning it, so it counts. This
//! is the one place forge does NOT reuse
//! [`aterm_census::collect_rs_files`] verbatim: that walker skips dot-
//! directories on purpose, because inside the aterm checkout they hold
//! `.claude/worktrees` clones that would multiply the census. Under a registry
//! or `vendor/` package root there are no worktrees, only shipped source.
//!
//! # Why `unsafe` is counted as tokens
//!
//! `\bunsafe\b`, not `unsafe {` blocks. MEASURED: the objc2 family emits nearly
//! all of its `unsafe` from `extern_methods!`/`extern_class!`, so a block count
//! reads 0 for an 83k-line crate. The word boundary is real: the workspace
//! denies `unsafe_op_in_unsafe_fn`, and that lint name must never inflate a
//! package's number.

use crate::model::{Cell, CellSurvey, Graph, PkgFacts, PkgId};
use crate::resolve;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Where a package's source lives, in the order the contract fixes:
/// a WORKSPACE MEMBER first, then its `[patch.crates-io]` target (both matched
/// on name AND version), then the registry checkout, then `crates/<name>`.
///
/// PATCH-BEFORE-REGISTRY, since 2026-08-30, and the previous order is worth
/// knowing about because it was deliberate and still wrong. It preferred a
/// pristine registry copy of the same version, to keep the LOC ledger stable
/// while a fork was being edited — "a fork that deletes 400 lines has not
/// shrunk the surface aterm depends on, it has shrunk the fork". The goal was
/// right; the mechanism silently fell through to `vendor/<name>` when no such
/// checkout existed, so the ledger's committed numbers depended on whether the
/// operator's CARGO_HOME happened to hold one — green on m21, red everywhere
/// else, same commit. See the body for the measurement. What compiles is the
/// fork, so the fork is what this module sizes; [`crate::attest`] remains where
/// fork-vs-upstream drift is audited.
///
/// WORKSPACE-MEMBER-BEFORE-REGISTRY IS ALSO NOT AN ACCIDENT, and it was found
/// by being wrong. That same "prefer the pristine copy" rule matches purely on
/// `<name>-<version>`, so when aterm patched in a crate it WROTE — package
/// `tracing 0.1.44` at `crates/aterm-tracing`, a 1,541-line no-op facade
/// replacing the real one — the survey found
/// `~/.cargo/registry/src/*/tracing-0.1.44` first and billed aterm 72,271
/// lines of somebody else's source for code that is not in the build. The
/// replacement's whole purpose is to delete 84,483 lines; measured through the
/// registry it appeared to delete 12,212. The stability argument does not
/// reach here: for a FIRST-PARTY member the lines we wrote ARE the surface,
/// and the identically-named registry crate is a coincidence of how
/// `[patch.crates-io]` matches (on package name). So members are resolved by
/// their manifest's `[package] name`, not by directory, and they win.
pub fn package_dir(root: &Path, id: &PkgId) -> Option<PathBuf> {
    package_dir_hinted(root, id, None)
}

/// [`package_dir`] with the directory `cargo tree` printed as a last resort.
/// The public signature cannot take it (it is keyed on [`PkgId`] alone), but a
/// caller holding the resolve output should pass it: it is cargo's own answer,
/// and it is right even for a workspace laid out differently from this one.
pub fn package_dir_hinted(root: &Path, id: &PkgId, printed: Option<&Path>) -> Option<PathBuf> {
    // NAME AND VERSION, and the version half is the whole safety of this
    // branch. Matching on name alone is FAIL-OPEN on the number this campaign
    // is scored on: `[patch.crates-io]` cannot satisfy every requirement (a
    // `=0.1.41` or a `^0.1.45` would resolve a SECOND, registry-sourced
    // `tracing` alongside ours), and a name-only hit would then bill that real
    // third-party package to `crates/aterm-tracing` and — because
    // `is_third_party` is decided from the directory below — quietly drop it
    // out of `third_party_packages` and `third_party_loc` entirely. The survey
    // already reports 8 duplicate names on mac-arm, so a name resolving at two
    // versions is routine, not hypothetical. With the version compared, the
    // member wins only for the exact package it actually is, and any other
    // version falls through to the registry branch where it belongs.
    if let Some((version, member)) = workspace_members(root).get(&id.name)
        && version == &id.version
    {
        return Some(member.clone());
    }
    // A PATCHED PACKAGE RESOLVES TO THE PATH THAT COMPILES, before the registry
    // is consulted at all. This reverses "registry-before-vendor", and the
    // reversal was forced by measurement rather than taste.
    //
    // The old order preferred a PRISTINE registry checkout of the same version
    // when one happened to be unpacked in this machine's CARGO_HOME, and fell
    // through to `vendor/<name>` when none was — silently. The two measure
    // different code, so a committed, ratcheted number depended on an
    // unversioned local cache. Proved by construction: a scratch CARGO_HOME
    // mirroring the real one plus only the pristine `winit 0.30.13` and
    // `smol_str 0.2.2` trees unpacked from their published `.crate`s puts all
    // four survey cells EXACTLY on their recorded ceilings and turns `check`
    // green; without them every cell reads +713 lines (winit +685, smol_str
    // +28, the latter in all four cells via winit). Every ratchet commit in
    // this ledger's history was authored on m21, so the ceilings were
    // calibrated to m21's cache, and the same commit was green there and red
    // everywhere else.
    //
    // The old rule's stated purpose was STABILITY — "a fork that deletes 400
    // lines has not shrunk the surface aterm depends on, it has shrunk the
    // fork". That goal is real, and this branch never delivered it: a number
    // that changes with `cargo fetch` is not stable, it is only stable on one
    // laptop. Nor is the pristine copy reliably obtainable — neither the
    // unpacked source nor the `.crate` for either fork exists on this machine,
    // so "measure pristine" is not even available to fail closed on here
    // without a network.
    //
    // What ships is the fork. `aterm` compiles `vendor/winit`, not the
    // registry's winit, so the fork's lines ARE the third-party surface this
    // campaign is scored on, and they are readable on every machine from the
    // repository alone. The signal the old rule was protecting is not lost:
    // fork-vs-upstream drift is [`crate::attest`]'s obligation (OB-7 diffs the
    // fork against its pinned upstream), which is where it belongs — this
    // module sizes the surface, attest audits how it differs.
    //
    // The patch TABLE is the authority, not the `vendor/` directory name: a
    // patch may point anywhere, and the first-party replacements point at
    // `crates/aterm-*`. Those are already resolved by the member branch above;
    // this catches the vendored forks and anything else the table redirects.
    // NAME AND VERSION HERE TOO, for the reason the member branch above spells
    // out: `[patch.crates-io]` matches on package NAME, and cargo will still
    // resolve a SECOND, registry-sourced copy of that name at a version the
    // patch cannot satisfy. Billing that real third-party package to the fork's
    // directory would drop it out of `third_party_*` entirely. The patch
    // target's own manifest version is the discriminator, exactly as the
    // member's is.
    if let Some((version, dir)) = patch_targets(root).get(&id.name)
        && version == &id.version
        && dir.is_dir()
    {
        return Some(dir.clone());
    }
    for src in registry_srcs() {
        let candidate = src.join(format!("{}-{}", id.name, id.version));
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    // NO VERSION-BLIND `vendor/<name>` FALLBACK. There used to be one, and with
    // the patch branch above it is worse than redundant: every `vendor/` tree
    // here IS a `[patch.crates-io]` target, so the only ids it could still
    // catch are the ones the version guard just REFUSED — a second,
    // registry-sourced copy at a version the fork cannot satisfy. Answering
    // `vendor/<name>` for those would bill a real third-party package to the
    // fork's directory and drop it out of `third_party_*` entirely, which is
    // the exact failure the member branch's version check was written to stop.
    let member = root.join("crates").join(&id.name);
    if member.is_dir() {
        return Some(member);
    }
    printed.filter(|p| p.is_dir()).map(Path::to_path_buf)
}

/// Package name → (version, directory) for every `[patch.crates-io]` target.
///
/// The patch TABLE, not the `vendor/` directory name, is the authority on where
/// a patched package's source lives: a patch may point anywhere, and this
/// repo's first-party replacements point at `crates/aterm-*` while its forks
/// point at `vendor/*`. Parsed with the same minimal TOML walk
/// `attest::patch_paths` uses; kept here rather than shared because `attest`
/// returns an ordered pair list for its own obligations and this wants a
/// lookup.
///
/// Memoized per root for the same reason [`workspace_members`] is: `survey`
/// measures four cells over ~150 packages and each one asks this question.
fn patch_targets(root: &Path) -> &'static MemberIndex {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, &'static MemberIndex>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(hit) = guard.get(root) {
        return hit;
    }
    let mut table: MemberIndex = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) {
        let mut in_patch = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_patch = t == "[patch.crates-io]";
                continue;
            }
            if !in_patch || t.is_empty() || t.starts_with('#') {
                continue;
            }
            // `name = { path = "vendor/name" }`
            let Some((name, rest)) = t.split_once('=') else {
                continue;
            };
            let Some(after) = rest.split_once("path") else {
                continue;
            };
            let mut it = after.1.split('"');
            let _ = it.next();
            if let Some(path) = it.next().filter(|p| !p.is_empty()) {
                let dir = root.join(path);
                let Some(version) = manifest_version(&dir) else {
                    continue;
                };
                table.insert(name.trim().trim_matches('"').to_string(), (version, dir));
            }
        }
    }
    let leaked: &'static MemberIndex = Box::leak(Box::new(table));
    guard.insert(root.to_path_buf(), leaked);
    leaked
}

/// `[package] version` from a manifest directory, if it reads.
fn manifest_version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = t.strip_prefix("version") {
            let mut it = rest.split('"');
            let _ = it.next();
            if let Some(v) = it.next().filter(|v| !v.is_empty()) {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// `[package] name` -> (version, directory) for every `crates/*` member.
///
/// Named because the raw type tripped `clippy::type_complexity` once it had to
/// carry the version — and the version is the half that makes the lookup safe.
type MemberIndex = BTreeMap<String, (String, PathBuf)>;

/// `[package] name` → directory, for every `crates/*` member of THIS root.
///
/// Keyed on the manifest name rather than on the directory name because the
/// two are allowed to differ, and one member deliberately makes them differ:
/// `crates/aterm-tracing` is package `tracing`, since `[patch.crates-io]`
/// matches on package name and nothing else would redirect the four
/// third-party consumers at it. A directory-name lookup misses exactly the
/// crate that needs to be found.
///
/// Memoized per root: `survey` measures five cells that share ~150 packages,
/// and this is ~70 small manifest reads.
fn workspace_members(root: &Path) -> &'static MemberIndex {
    static MEMBERS: OnceLock<Mutex<BTreeMap<PathBuf, &'static MemberIndex>>> = OnceLock::new();
    let cache = MEMBERS.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(map) = cache.lock()
        && let Some(hit) = map.get(root)
    {
        return hit;
    }
    // Members overwhelmingly inherit `version.workspace = true`; only the
    // patched-in facades state a literal, because a patch has to satisfy the
    // consumers' semver. Resolve both so the caller can compare an exact
    // version rather than trusting a name.
    let workspace_version = std::fs::read_to_string(root.join("Cargo.toml"))
        .ok()
        .and_then(|text| {
            text.parse::<aterm_toml::edit::DocumentMut>()
                .ok()
                .and_then(|doc| {
                    doc.get("workspace")
                        .and_then(|w| w.get("package"))
                        .and_then(|p| p.get("version"))
                        .and_then(aterm_toml::edit::Item::as_str)
                        .map(str::to_string)
                })
        })
        .unwrap_or_default();
    let mut found: BTreeMap<String, (String, PathBuf)> = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            let dir = entry.path();
            let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
                continue;
            };
            let name = package_name(&text);
            if !name.is_empty() {
                let version = package_version(&text).unwrap_or_else(|| workspace_version.clone());
                found.insert(name, (version, dir));
            }
        }
    }
    let leaked: &'static BTreeMap<String, (String, PathBuf)> = Box::leak(Box::new(found));
    if let Ok(mut map) = cache.lock() {
        map.insert(root.to_path_buf(), leaked);
    }
    leaked
}

/// A member's LITERAL `[package] version`, or `None` when it inherits
/// (`version.workspace = true` parses as a table, not a string).
fn package_version(text: &str) -> Option<String> {
    text.parse::<aterm_toml::edit::DocumentMut>()
        .ok()
        .and_then(|doc| {
            doc.get("package")
                .and_then(|p| p.get("version"))
                .and_then(aterm_toml::edit::Item::as_str)
                .map(str::to_string)
        })
}

/// `[package] name` out of a manifest, parsed the same way
/// [`manifest_facts`] parses its keys. Empty when the manifest states none (a
/// virtual manifest; cargo forbids inheriting a package name).
fn package_name(text: &str) -> String {
    text.parse::<aterm_toml::edit::DocumentMut>()
        .ok()
        .and_then(|doc| {
            doc.get("package")
                .and_then(|p| p.get("name"))
                .and_then(aterm_toml::edit::Item::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// `$CARGO_HOME/registry/src/index.crates.io-*`, discovered once. The hash
/// suffix is a function of the registry URL and cargo's protocol, so it is
/// globbed rather than hard-coded; more than one may exist after a protocol
/// change and all of them are searched.
fn registry_srcs() -> &'static [PathBuf] {
    static SRCS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    SRCS.get_or_init(|| {
        let Some(home) = cargo_home() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(home.join("registry").join("src")) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_index = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("index.crates.io-"));
                if is_index && path.is_dir() {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    })
}

fn cargo_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".cargo"))
}

/// Facts for every node in one graph.
pub fn facts(root: &Path, graph: &Graph) -> BTreeMap<PkgId, PkgFacts> {
    facts_with_paths(root, graph, &BTreeMap::new())
}

/// [`facts`] with the directories `cargo tree` disclosed.
pub fn facts_with_paths(
    root: &Path,
    graph: &Graph,
    printed: &BTreeMap<PkgId, PathBuf>,
) -> BTreeMap<PkgId, PkgFacts> {
    let mut out = BTreeMap::new();
    for id in &graph.nodes {
        out.insert(
            id.clone(),
            cached_facts(root, id, printed.get(id).map(PathBuf::as_path)),
        );
    }
    out
}

/// Measure one package from scratch — no cache, for callers that need a fresh
/// read after editing a fork.
pub fn measure(root: &Path, id: &PkgId, printed: Option<&Path>) -> PkgFacts {
    let Some(dir) = package_dir_hinted(root, id, printed) else {
        // Unresolvable means "not in this checkout", which for a graph node can
        // only be a registry package whose source was never unpacked. It is
        // third-party by definition; its LOC is honestly unknown, so it is 0
        // and the survey's own totals will not silently absorb it.
        return PkgFacts {
            is_third_party: true,
            ..PkgFacts::default()
        };
    };
    let mut files = Vec::new();
    collect_rs(&dir, &mut files);
    let mut loc = 0u64;
    let mut unsafe_tokens = 0u64;
    for file in &files {
        let Ok(bytes) = std::fs::read(file) else {
            continue;
        };
        loc += physical_lines(&bytes);
        unsafe_tokens += count_unsafe(&bytes);
    }
    let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
    let (license, is_proc_macro) = manifest_facts(&manifest);
    PkgFacts {
        loc,
        unsafe_tokens,
        has_build_rs: dir.join("build.rs").is_file(),
        is_proc_macro,
        license,
        // The contract's test, verbatim: a package is third-party iff its
        // manifest is NOT under `<root>/crates/`. The six `[patch.crates-io]`
        // path packages under `vendor/` are therefore third-party, which is
        // correct — they are upstream code aterm now maintains, not code aterm
        // wrote.
        is_third_party: !dir.starts_with(root.join("crates")),
        root_dir: Some(dir),
    }
}

/// The process-wide memo. A package's on-disk facts do not change while forge
/// runs, and the five cells share ~150 packages: without this, `survey` walks
/// the same registry trees five times.
fn cached_facts(root: &Path, id: &PkgId, printed: Option<&Path>) -> PkgFacts {
    static CACHE: OnceLock<Mutex<BTreeMap<(PathBuf, PkgId), PkgFacts>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let key = (root.to_path_buf(), id.clone());
    if let Ok(map) = cache.lock()
        && let Some(hit) = map.get(&key)
    {
        return hit.clone();
    }
    let fresh = measure(root, id, printed);
    if let Ok(mut map) = cache.lock() {
        map.insert(key, fresh.clone());
    }
    fresh
}

/// Graph plus facts for one cell.
pub fn survey_cell(root: &Path, cell: &Cell) -> Result<CellSurvey, String> {
    let mut log = String::new();
    let out = survey_cell_logged(root, cell, &mut log);
    if !log.is_empty() {
        eprint!("{log}");
    }
    out
}

/// [`survey_cell`] with the resolver's notes captured instead of printed.
pub fn survey_cell_logged(
    root: &Path,
    cell: &Cell,
    log: &mut String,
) -> Result<CellSurvey, String> {
    let (graph, paths) = resolve::graph_and_paths(root, cell, log)?;
    let facts = facts_with_paths(root, &graph, &paths);
    Ok(CellSurvey {
        cell: cell.clone(),
        graph,
        facts,
    })
}

/// Every `*.rs` under `dir`. `target/` is skipped (build output is not source)
/// and `.git` is skipped (an object store holds no `.rs` and walking one is
/// pure cost); everything else, dot-directories included, is shipped source.
/// See the module docs for why this diverges from
/// [`aterm_census::collect_rs_files`].
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let name = path.file_name().unwrap_or_default();
            if name == "target" || name == ".git" {
                continue;
            }
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Physical lines: every newline, plus a final line that lacks one. An empty
/// file is 0 lines, which is why this is not `split('\n').count()`.
fn physical_lines(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    if bytes.last() == Some(&b'\n') {
        newlines
    } else {
        newlines + 1
    }
}

/// `\bunsafe\b` occurrences. Word-bounded on both sides, so
/// `unsafe_op_in_unsafe_fn` contributes 0 and `unsafe {`, `unsafe fn`,
/// `#[unsafe(no_mangle)]` and a bare `unsafe` at end of file each contribute 1.
fn count_unsafe(bytes: &[u8]) -> u64 {
    const WORD: &[u8] = b"unsafe";
    let mut count = 0u64;
    let mut at = 0usize;
    while at + WORD.len() <= bytes.len() {
        let Some(offset) = bytes[at..].windows(WORD.len()).position(|w| w == WORD) else {
            break;
        };
        let start = at + offset;
        let end = start + WORD.len();
        let left_free = start == 0 || !is_ident_byte(bytes[start - 1]);
        let right_free = end == bytes.len() || !is_ident_byte(bytes[end]);
        if left_free && right_free {
            count += 1;
        }
        at = start + 1;
    }
    count
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `package.license` and `[lib] proc-macro = true`, read with the same
/// comment-preserving parser the policy file uses. A manifest that inherits
/// `license.workspace = true` yields an empty string rather than a lie —
/// workspace members are not the licence-obligation surface, forks are.
fn manifest_facts(text: &str) -> (String, bool) {
    let Ok(doc) = text.parse::<aterm_toml::edit::DocumentMut>() else {
        return (String::new(), false);
    };
    let license = doc
        .get("package")
        .and_then(|p| p.get("license"))
        .and_then(aterm_toml::edit::Item::as_str)
        .unwrap_or_default()
        .to_string();
    let is_proc_macro = doc
        .get("lib")
        .and_then(|l| l.get("proc-macro"))
        .and_then(aterm_toml::edit::Item::as_bool)
        .unwrap_or(false);
    (license, is_proc_macro)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measured;
    use crate::resolve::default_cells;

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/aterm-forge sits two levels under the workspace root")
            .to_path_buf()
    }

    fn survey(cell_index: usize) -> CellSurvey {
        let cells = default_cells();
        survey_cell(&repo_root(), &cells[cell_index])
            .unwrap_or_else(|e| panic!("cell must survey offline: {e}"))
    }

    #[test]
    fn physical_lines_counts_a_missing_final_newline() {
        assert_eq!(physical_lines(b""), 0);
        assert_eq!(physical_lines(b"a\n"), 1);
        assert_eq!(physical_lines(b"a"), 1);
        assert_eq!(physical_lines(b"a\nb"), 2);
        assert_eq!(physical_lines(b"a\nb\n"), 2);
        assert_eq!(physical_lines(b"\n\n"), 2);
    }

    #[test]
    fn unsafe_is_a_token_not_a_substring() {
        assert_eq!(count_unsafe(b"unsafe { }"), 1);
        assert_eq!(count_unsafe(b"pub unsafe fn f()"), 1);
        assert_eq!(count_unsafe(b"#[unsafe(no_mangle)]"), 1);
        assert_eq!(count_unsafe(b"unsafe"), 1);
        // The lint the workspace denies must never inflate a package.
        assert_eq!(count_unsafe(b"unsafe_op_in_unsafe_fn"), 0);
        assert_eq!(count_unsafe(b"#![deny(unsafe_op_in_unsafe_fn)]"), 0);
        assert_eq!(count_unsafe(b"my_unsafe_helper"), 0);
        assert_eq!(count_unsafe(b"unsafely"), 0);
        assert_eq!(count_unsafe(b"unsafe unsafe"), 2);
        assert_eq!(count_unsafe(b""), 0);
    }

    #[test]
    fn manifest_facts_read_licence_and_the_proc_macro_flag() {
        let (lic, pm) = manifest_facts("[package]\nlicense = \"MIT OR Apache-2.0\"\n");
        assert_eq!(lic, "MIT OR Apache-2.0");
        assert!(!pm);
        let (_, pm) = manifest_facts("[package]\nname = \"x\"\n[lib]\nproc-macro = true\n");
        assert!(pm);
        // An inherited licence is reported as unknown, never invented.
        let (lic, _) = manifest_facts("[package]\nlicense.workspace = true\n");
        assert_eq!(lic, "");
        assert_eq!(manifest_facts("this is not toml ][").0, "");
    }

    #[test]
    fn package_dir_prefers_the_member_then_the_patch_then_the_registry() {
        let root = repo_root();

        // THE REGISTRY ARM. `syn` is the deliberate choice and the reason is a
        // scar: this assertion used to name `libc 0.2.186`, and it went RED the
        // day libc was retired to the first-party `crates/aterm-libc` — because
        // the member then correctly won, which is the very precedence this test
        // exists to prove. The example must therefore be a package the campaign
        // has DOCUMENTED it cannot retire. `syn` is proc-macro-only: it is
        // compiler-resident with zero machine code in the binary, and
        // `docs/THIRD_PARTY_ROAD_TO_ZERO.md` records that neither lane —
        // mirroring nor extraction — retires that class at all.
        let registry = package_dir(&root, &PkgId::new("syn", "2.0.117"))
            .expect("syn 2.0.117 is unpacked in this checkout's registry");
        assert!(registry.ends_with("syn-2.0.117"), "{}", registry.display());
        assert!(
            !registry.starts_with(&root),
            "registry sources live outside the repo"
        );

        // THE MEMBER ARM, asserted on the package that broke the registry arm,
        // so the move is recorded rather than merely worked around: `libc` is
        // patched to a workspace member and must resolve there, NOT to the
        // registry copy of upstream libc that also exists on this box.
        let retired = package_dir(&root, &PkgId::new("libc", "0.2.186"))
            .expect("the patched `libc` resolves");
        assert_eq!(retired, root.join("crates").join("aterm-libc"));

        // THE PATCH ARM. A vendored fork answers from the path that COMPILES,
        // on every machine — the assertion the old order could not make, since
        // it read the registry first and reached `vendor/` only when this box
        // happened to lack a pristine checkout of the same version.
        let forked = package_dir(&root, &PkgId::new("winit", "0.30.13"))
            .expect("the patched winit resolves");
        assert_eq!(forked, root.join("vendor").join("winit"));

        // …and only at the version the fork actually IS. This assertion used to
        // read the other way — an unpublished version fell through to
        // `vendor/winit` — and that fallback is exactly what had to go: a
        // second, registry-sourced copy of the same NAME is somebody else's
        // package, and billing it to the fork's directory would drop a real
        // third-party package out of `third_party_*` altogether.
        assert_ne!(
            package_dir(&root, &PkgId::new("winit", "0.0.0-not-published")),
            Some(root.join("vendor").join("winit")),
            "the version guard must refuse a name-only match"
        );

        let member = package_dir(&root, &PkgId::new("aterm-core", "0.47.0"))
            .expect("workspace members resolve under crates/");
        assert_eq!(member, root.join("crates").join("aterm-core"));

        assert_eq!(
            package_dir(&root, &PkgId::new("no-such-crate-anywhere", "1.0.0")),
            None
        );
    }

    /// THE COLLISION, on the real tree: package `tracing 0.1.44` is a
    /// workspace member at `crates/aterm-tracing`, and a registry checkout of
    /// upstream `tracing-0.1.44` also exists on this box. The member must win,
    /// by MANIFEST name (the directory is called something else on purpose),
    /// or the survey bills aterm 72,271 lines of source that is not in the
    /// build and the replacement's whole 84,483-line win reads as 12,212.
    #[test]
    fn a_first_party_patch_target_is_measured_at_the_member_not_the_registry_copy() {
        let root = repo_root();
        let dir = package_dir(&root, &PkgId::new("tracing", "0.1.44"))
            .expect("the patched `tracing` resolves");
        assert_eq!(dir, root.join("crates").join("aterm-tracing"));
        assert!(
            measure(&root, &PkgId::new("tracing", "0.1.44"), None).loc < 5_000,
            "the shim is ~1.5k lines; upstream tracing 0.1.44 is 72,271"
        );
        assert!(
            !measure(&root, &PkgId::new("tracing", "0.1.44"), None).is_third_party,
            "a workspace member is aterm's own code"
        );
    }

    #[test]
    fn a_member_is_found_by_its_manifest_name_not_its_directory_name() {
        assert_eq!(
            workspace_members(&repo_root()).get("tracing").cloned(),
            Some((
                "0.1.44".to_string(),
                repo_root().join("crates").join("aterm-tracing")
            ))
        );
        assert_eq!(package_name("[package]\nname = \"x\"\n"), "x");
        assert_eq!(package_name("[workspace]\nmembers = []\n"), "");
        assert_eq!(
            package_version("[package]\nversion = \"1.2.3\"\n").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(
            package_version("[package]\nversion.workspace = true\n"),
            None,
            "an inherited version is not a literal and must fall back to the workspace"
        );
    }

    /// THE MEMBER BRANCH IS VERSION-GATED, and this proves the gate is armed.
    ///
    /// A name-only match was fail-open on the campaign's headline number: a
    /// second, registry-sourced `tracing` at a version `[patch.crates-io]`
    /// cannot satisfy would have resolved to `crates/aterm-tracing` and, since
    /// `is_third_party` is read off the directory, vanished from
    /// `third_party_packages` and `third_party_loc` altogether. Ask for a
    /// version the member is NOT, and the member must decline.
    #[test]
    fn a_member_does_not_answer_for_a_different_version_of_its_name() {
        let root = repo_root();
        let ours = package_dir(&root, &PkgId::new("tracing", "0.1.44"));
        assert_eq!(
            ours.as_deref(),
            Some(root.join("crates").join("aterm-tracing").as_path()),
            "control: the member DOES answer for its own version"
        );
        let other = package_dir(&root, &PkgId::new("tracing", "0.1.41"));
        assert_ne!(
            other.as_deref(),
            Some(root.join("crates").join("aterm-tracing").as_path()),
            "a DIFFERENT version of the same name must never resolve to the member — that \
             would bill somebody else's source to us and delete it from the third-party count"
        );
    }

    #[test]
    fn the_printed_path_is_the_last_resort() {
        let root = repo_root();
        let hint = root.join("vendor").join("indexmap");
        let got = package_dir_hinted(&root, &PkgId::new("nope-not-real", "0.0.1"), Some(&hint));
        assert_eq!(got.as_deref(), Some(hint.as_path()));
    }

    /// Assert one cell against its row in [`measured::CELLS`]. Every count the
    /// baseline carries is checked at once, so a cell needs exactly one test
    /// and an extraction needs exactly one edit — in `measured.rs`, not here.
    fn assert_matches_baseline(cell_index: usize) {
        let want = measured::CELLS[cell_index];
        let s = survey(cell_index);
        assert_eq!(s.cell.name, want.cell, "baseline row is for another cell");
        let third = s.third_party().count();
        let got = measured::Baseline {
            cell: want.cell,
            resolved: s.graph.nodes.len(),
            workspace: s.graph.nodes.len() - third,
            third_party: third,
            third_party_loc: s.third_party_loc(),
            // Every build script is arbitrary code the compiler runs, and
            // `targo trust` marks all of them `-Ztrust-verify=off`
            // unconditionally — hence a pinned row of its own.
            build_scripts: s.build_scripts(),
            proc_macros: s.proc_macros(),
            duplicate_names: s.duplicate_names().len(),
        };
        assert_eq!(
            got, want,
            "cell `{}` has moved off the measured baseline",
            want.cell
        );
    }

    #[test]
    fn mac_arm_matches_the_measured_baseline() {
        assert_matches_baseline(0);
    }

    #[test]
    fn mac_arm_third_party_loc_matches_the_baseline() {
        assert_eq!(
            survey(0).third_party_loc(),
            measured::MAC_ARM.third_party_loc
        );
    }

    #[test]
    fn mac_arm_duplicate_names_match_the_baseline() {
        let s = survey(0);
        let dups = s.duplicate_names();
        let names: Vec<&str> = dups.keys().map(String::as_str).collect();
        assert_eq!(names, measured::MAC_ARM_DUPLICATE_NAMES);
        // THE hashbrown TOOTH. Counted over the RESOLVED graph, not looked up
        // in `dups`: hashbrown is down to one version and therefore has no key
        // in the duplicate map at all, so the old `dups["hashbrown"]` would
        // panic on the fix instead of asserting it. One requirement in the whole
        // tree ever asked for the second copy — upstream indexmap 2.14's
        // `hashbrown = "0.17"`, now pinned to "0.16" in vendor/indexmap — so a
        // 2 here means that vendored line came back, or a new parent brought a
        // third major of hashbrown with it. Either is 25,236 lines of duplicated
        // hash table in one binary, and either is worth failing for.
        let versions = s.third_party().filter(|p| p.name == "hashbrown").count();
        assert_eq!(
            versions,
            measured::MAC_ARM_HASHBROWN_VERSIONS,
            "hashbrown resolves at {versions} version(s) on mac-arm; the dedup \
             to a single 0.16.1 has regressed"
        );
    }

    #[test]
    fn linux_matches_the_measured_baseline() {
        assert_matches_baseline(1);
    }

    #[test]
    fn windows_and_both_wasm_modules_match_the_measured_baseline() {
        assert_matches_baseline(2);
        assert_matches_baseline(3);
        assert_matches_baseline(4);
    }

    /// THE CLAIM THE TWO wasm CELLS MAKE, CHECKED RATHER THAN ASSUMED.
    ///
    /// aterm ships THREE `cdylib` crates into the browser page — the census
    /// names all three in `aterm_census::wasm_census::WASM_ROOT_CRATES` — and
    /// forge gives only two of them a cell. That is honest only while the third,
    /// `aterm-effects-web`, brings nothing of its own: measured, its `wasm32`
    /// closure differs from `aterm-wasm`'s by exactly one node, its own root
    /// package, so every third-party package it ships is already inside the
    /// `wasm-cpu` row.
    ///
    /// Nothing about that is permanent. `aterm-effects-web` is a real crate with
    /// its own manifest, and the day it takes a dependency the CPU module does
    /// not have, a shipped browser module would be carrying third-party code no
    /// ratchet bounds — silently, because no cell is rooted there. This test is
    /// what makes that loud, and it names the packages.
    ///
    /// The same check runs for the CPU module against the GPU module, for the
    /// weaker reason that it is the containment the `wgpu`-is-the-whole-gap note
    /// on [`measured::WASM_GPU`] asserts in prose.
    #[test]
    fn the_wasm_cells_cover_every_shipped_web_crate() {
        let root = repo_root();
        let contained = |inner: &str, outer_cell_index: usize| {
            let outer = survey(outer_cell_index);
            let cell = crate::model::Cell {
                name: format!("probe-{inner}"),
                triple: "wasm32-unknown-unknown".to_string(),
                package: inner.to_string(),
            };
            let g = crate::resolve::graph(&root, &cell)
                .unwrap_or_else(|e| panic!("`{inner}` must resolve for wasm32 offline: {e}"));
            let escaped: Vec<String> = g
                .nodes
                .iter()
                .filter(|id| id.name != inner && !outer.graph.nodes.contains(id))
                .map(ToString::to_string)
                .collect();
            assert!(
                escaped.is_empty(),
                "`{inner}` is a SHIPPED wasm module whose graph has left the `{}` cell: {escaped:?}.                  Either add a cell rooted at `{inner}` to aterm_forge::resolve::default_cells and                  ratchet it in tools/forge-budget.tsv AND measured.rs, or put the dependency back                  inside a cell that is measured. A shipped module outside every cell is                  third-party surface with no ceiling.",
                outer.cell.name
            );
        };
        // Every census-listed browser root is judged, BY READING THE CONSTANT.
        // The first cut of this test hardcoded the two names it knew about, so a
        // FOURTH cdylib added to `WASM_ROOT_CRATES` would have shipped with no
        // cell, no ceiling and no failure — the exact silence the docstring
        // promises against. A root that IS a cell root is measured directly and
        // skipped here; every other root must fit inside the superset wasm cell
        // (index 4, `wasm-gpu`), whose row then bounds all of its third-party
        // packages. A bogus or unresolvable entry fails the resolve unwrap.
        // The census roster holds repo-relative DIRECTORY paths; the package
        // name is the final segment (true for every workspace member here, and
        // the resolve unwrap below fails loudly if that ever stops holding).
        let cells = crate::resolve::default_cells();
        for entry in aterm_census::WASM_ROOT_CRATES {
            let inner = entry.rsplit('/').next().expect("split never yields empty");
            if cells.iter().any(|c| c.package == inner) {
                continue;
            }
            contained(inner, 4);
        }
        // And the two STRONGER containments, pinned by name: the effects module
        // fits the CPU cell (not merely the GPU superset), and CPU ⊆ GPU is the
        // set-difference claim the `wgpu` note on [`measured::WASM_GPU`] makes.
        contained("aterm-effects-web", 3);
        contained("aterm-wasm", 4);
    }

    #[test]
    fn every_third_party_package_resolves_to_a_directory_with_source() {
        let s = survey(0);
        for id in s.third_party() {
            let f = &s.facts[id];
            assert!(
                f.root_dir.is_some(),
                "{id} has no directory — LOC would silently be 0"
            );
            assert!(f.loc > 0, "{id} measured 0 lines of Rust");
        }
    }

    #[test]
    fn the_vendored_forks_are_third_party_and_workspace_crates_are_not() {
        // THE FLIP: `indexmap` (this test's fork specimen) left the mac-arm
        // graph with wgpu, its only parents there. linux still resolves it.
        let s = survey(1);
        let fork = s
            .facts
            .iter()
            .find(|(id, _)| id.name == "indexmap")
            .expect("the indexmap fork is in the linux graph");
        assert!(
            fork.1.is_third_party,
            "a [patch.crates-io] fork is still upstream code"
        );
        let core = s
            .facts
            .iter()
            .find(|(id, _)| id.name == "aterm-core")
            .expect("aterm-core is in the graph");
        assert!(!core.1.is_third_party);
        assert!(
            core.1.loc > 0,
            "workspace crates are measured too, just not billed"
        );
    }

    #[test]
    fn licences_are_read_for_every_third_party_package() {
        let s = survey(0);
        let blank: Vec<String> = s
            .third_party()
            .filter(|id| s.facts[*id].license.is_empty())
            .map(ToString::to_string)
            .collect();
        assert!(
            blank.is_empty(),
            "third-party packages with no `license` field: {blank:?}"
        );
    }

    #[test]
    fn unsafe_tokens_are_measured_and_objc2_is_not_zero() {
        let s = survey(0);
        let objc2: u64 = s
            .third_party()
            .filter(|id| id.name.starts_with("objc2"))
            .map(|id| s.facts[id].unsafe_tokens)
            .sum();
        // A BLOCK count reads ~0 here; the token count is the honest one.
        assert!(objc2 > 1000, "objc2 family unsafe tokens = {objc2}");
    }
}

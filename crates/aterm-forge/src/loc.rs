// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-package facts: how much source, how much `unsafe`, does it run code at
//! build time, what licence, and is it ours.
//!
//! # What LOC means here
//!
//! Physical lines over every `*.rs` under the package's root directory,
//! including its own tests, examples, benches and anything a published
//! `.crate` carries outside `src/` (`loc_method = "rs-physical-all-files-v1"`):
//! the source aterm would own if it vendored the package, not what reaches
//! codegen. This is the number the budget ratchets.
//!
//! The walk descends into dot-directories (e.g. `smol_str-0.2.2/.github/ci.rs`
//! ships in the `.crate`, so it counts). This is where forge diverges from
//! [`aterm_census::collect_rs_files`], which skips dot-directories to avoid
//! `.claude/worktrees` clones inside the checkout; package roots hold none.
//!
//! # Why `unsafe` is counted as tokens
//!
//! `\bunsafe\b`, not `unsafe {` blocks: macro-generated FFI (e.g.
//! `extern_methods!`) emits `unsafe` without blocks. The word boundary keeps
//! lint names like `unsafe_op_in_unsafe_fn` from counting.

use crate::model::{Cell, CellSurvey, Graph, PkgFacts, PkgId};
use crate::resolve;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Where a package's source lives, in precedence order: a workspace member,
/// then its `[patch.crates-io]` target (both matched on name and version), then
/// the registry checkout, then `crates/<name>`.
///
/// Patch before registry: what compiles is the fork, and a registry-first
/// lookup would make the committed numbers depend on the local CARGO_HOME.
/// Fork-vs-upstream drift is audited by [`crate::attest`].
///
/// Member before registry: a first-party patched-in crate (package `tracing`
/// at `crates/aterm-tracing`) shares its name with the upstream it replaces, so
/// members are resolved by their manifest's `[package] name` and win.
///
/// `printed` is the directory `cargo tree` disclosed, used as a last resort;
/// callers holding resolve output should pass it.
pub fn package_dir(root: &Path, id: &PkgId, printed: Option<&Path>) -> Option<PathBuf> {
    // Name and version: `[patch.crates-io]` cannot satisfy every requirement,
    // so a second, registry-sourced copy of the same name can resolve. A
    // name-only hit would bill that third-party package to the member and drop
    // it from the third-party totals.
    if let Some((version, member)) = workspace_members(root).get(&id.name)
        && version == &id.version
    {
        return Some(member.clone());
    }
    // A patched package resolves to the path that compiles, before the
    // registry is consulted: the fork's lines are the third-party surface, and
    // they are readable from the repository alone on every machine. The patch
    // table is the authority, not the `vendor/` directory name. Name and
    // version here too, for the reason given in the member branch above.
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
    // No version-blind `vendor/<name>` fallback: every `vendor/` tree is a
    // patch target, so the only ids left are ones the version guard refused,
    // i.e. registry copies that must stay third-party.
    let member = root.join("crates").join(&id.name);
    if member.is_dir() {
        return Some(member);
    }
    printed.filter(|p| p.is_dir()).map(Path::to_path_buf)
}

/// Package name → (version, directory) for every `[patch.crates-io]` target.
///
/// The patch table, not the directory name, says where a patched package
/// lives (first-party replacements point at `crates/aterm-*`, forks at
/// `vendor/*`). Parsed with the same minimal TOML walk as
/// `attest::patch_paths`, but as a lookup. Memoized per root.
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
type MemberIndex = BTreeMap<String, (String, PathBuf)>;

/// `[package] name` → directory, for every `crates/*` member of this root.
///
/// Keyed on the manifest name, since it may differ from the directory
/// (`crates/aterm-tracing` is package `tracing`). Memoized per root.
fn workspace_members(root: &Path) -> &'static MemberIndex {
    static MEMBERS: OnceLock<Mutex<BTreeMap<PathBuf, &'static MemberIndex>>> = OnceLock::new();
    let cache = MEMBERS.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(map) = cache.lock()
        && let Some(hit) = map.get(root)
    {
        return hit;
    }
    // Members mostly inherit `version.workspace = true`; patched-in facades
    // state a literal. Resolve both so callers compare an exact version.
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

/// `[package] name` out of a manifest, parsed like [`manifest_facts`]. Empty
/// when the manifest states none (a virtual manifest).
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
/// suffix is globbed; all matches are searched.
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

/// Facts for every node in one graph, measured at the directories `cargo tree`
/// disclosed (`printed`).
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
    let Some(dir) = package_dir(root, id, printed) else {
        // Unresolvable means a registry package whose source was never
        // unpacked: third-party, LOC unknown and reported as 0.
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
        // Third-party iff not aterm's own code, decided by
        // [`crate::provenance`] rather than the path: `vendor/astream` is ours,
        // the other `vendor/` trees are forks.
        is_third_party: !crate::provenance::is_first_party(root, &dir),
        root_dir: Some(dir),
    }
}

/// The process-wide memo: on-disk facts do not change while forge runs, and
/// cells share most packages.
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

/// Test-only: [`survey_cell`] over the real workspace, on the once-per-process
/// resolution ([`resolve::real_workspace_graph_and_paths`]).
#[cfg(test)]
pub(crate) fn real_workspace_survey(cell: &Cell) -> Result<CellSurvey, String> {
    let root = resolve::real_workspace_root();
    let (graph, paths) = resolve::real_workspace_graph_and_paths(cell)?;
    let facts = facts_with_paths(&root, &graph, &paths);
    Ok(CellSurvey {
        cell: cell.clone(),
        graph,
        facts,
    })
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

/// Every `*.rs` under `dir`, skipping `target/` and `.git`; dot-directories are
/// included (see the module docs).
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

/// `package.license` and `[lib] proc-macro = true`, read with the policy file's
/// comment-preserving parser. `license.workspace = true` yields an empty
/// string.
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
    use crate::test_support::repo_root;

    fn survey(cell_index: usize) -> CellSurvey {
        let cells = default_cells();
        real_workspace_survey(&cells[cell_index])
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

        // Registry arm: `syn` is proc-macro-only and not a retirement target,
        // so it stays a registry package.
        let registry = package_dir(&root, &PkgId::new("syn", "2.0.117"), None)
            .expect("syn 2.0.117 is unpacked in this checkout's registry");
        assert!(registry.ends_with("syn-2.0.117"), "{}", registry.display());
        assert!(
            !registry.starts_with(&root),
            "registry sources live outside the repo"
        );

        // Member arm: `libc` is patched to a workspace member and resolves
        // there, not to a registry copy.
        let retired = package_dir(&root, &PkgId::new("libc", "0.2.186"), None)
            .expect("the patched `libc` resolves");
        assert_eq!(retired, root.join("crates").join("aterm-libc"));

        // Patch arm: a vendored fork answers from the path that compiles.
        let forked = package_dir(&root, &PkgId::new("winit", "0.30.13"), None)
            .expect("the patched winit resolves");
        assert_eq!(forked, root.join("vendor").join("winit"));

        // Only at the fork's own version: another version of the same name is
        // someone else's package.
        assert_ne!(
            package_dir(&root, &PkgId::new("winit", "0.0.0-not-published"), None),
            Some(root.join("vendor").join("winit")),
            "the version guard must refuse a name-only match"
        );

        let member = package_dir(&root, &PkgId::new("aterm-core", "0.47.0"), None)
            .expect("workspace members resolve under crates/");
        assert_eq!(member, root.join("crates").join("aterm-core"));

        assert_eq!(
            package_dir(&root, &PkgId::new("no-such-crate-anywhere", "1.0.0"), None),
            None
        );
    }

    /// Package `tracing 0.1.44` is a member at `crates/aterm-tracing`, and a
    /// registry checkout of upstream `tracing-0.1.44` may also exist. The
    /// member wins, by manifest name.
    #[test]
    fn a_first_party_patch_target_is_measured_at_the_member_not_the_registry_copy() {
        let root = repo_root();
        let dir = package_dir(&root, &PkgId::new("tracing", "0.1.44"), None)
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

    /// The member branch is version-gated: asking for a version the member is
    /// not falls through.
    #[test]
    fn a_member_does_not_answer_for_a_different_version_of_its_name() {
        let root = repo_root();
        let ours = package_dir(&root, &PkgId::new("tracing", "0.1.44"), None);
        assert_eq!(
            ours.as_deref(),
            Some(root.join("crates").join("aterm-tracing").as_path()),
            "control: the member DOES answer for its own version"
        );
        let other = package_dir(&root, &PkgId::new("tracing", "0.1.41"), None);
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
        let got = package_dir(&root, &PkgId::new("nope-not-real", "0.0.1"), Some(&hint));
        assert_eq!(got.as_deref(), Some(hint.as_path()));
    }

    /// Hold one cell at or under its row in [`measured::CELLS`] and its
    /// ceilings in `tools/forge-budget.tsv`, and return the survey.
    ///
    /// Every ratcheted third-party metric is a ceiling: a cell may shrink, not
    /// grow past either. Both are checked because `gate forge` is manual and
    /// `measured::ratchet_agreement` keeps the TSV at or under the measured
    /// row. `resolved` and `workspace` are not held (not surface). The mac-arm
    /// duplicate-name list below stays exact.
    fn assert_within_baseline(cell_index: usize) -> CellSurvey {
        use crate::measured::ratchet_agreement::{load, ratcheted, tsv_ceilings};
        let row = measured::CELLS[cell_index];
        let s = survey(cell_index);
        assert_eq!(s.cell.name, row.cell, "baseline row is for another cell");
        // In `ratcheted`'s order: third_party_packages, third_party_loc,
        // build_scripts, proc_macros, duplicate_names.
        let live = [
            s.third_party().count() as u64,
            s.third_party_loc(),
            s.build_scripts() as u64,
            s.proc_macros() as u64,
            s.duplicate_names().len() as u64,
        ];
        let tsv = tsv_ceilings(&load(), cell_index);
        let mut grown: Vec<String> = Vec::new();
        for ((got, (metric, in_row)), (_, in_tsv)) in live.into_iter().zip(ratcheted(&row)).zip(tsv)
        {
            if got > in_row {
                grown.push(format!("{metric} {got} > {in_row} (measured.rs)"));
            }
            if got > in_tsv {
                grown.push(format!(
                    "{metric} {got} > {in_tsv} (tools/forge-budget.tsv)"
                ));
            }
        }
        assert!(
            grown.is_empty(),
            "cell `{}` has grown past its measured baseline: {grown:?}. A deliberate \
             addition re-measures the row in measured.rs AND ratchets \
             tools/forge-budget.tsv (`targo --unverified forge budget --allow-regress`), with the \
             reason in the commit",
            row.cell
        );
        s
    }

    #[test]
    fn mac_arm_stays_within_the_measured_baseline() {
        let s = assert_within_baseline(0);
        // macOS carries the native a11y-appkit surface; AccessKit is linux-only
        // (see the linux test). Checked on the mac-arm survey, where it can fail.
        assert!(
            !s.graph.nodes.iter().any(|p| p.name == "accesskit"),
            "macOS must not carry AccessKit — it has the native a11y-appkit path"
        );
    }

    #[test]
    fn mac_arm_duplicate_names_match_the_baseline() {
        let s = survey(0);
        let dups = s.duplicate_names();
        let names: Vec<&str> = dups.keys().map(String::as_str).collect();
        assert_eq!(names, measured::MAC_ARM_DUPLICATE_NAMES);
        // Counted over the resolved graph (hashbrown has one version, so no key
        // in `dups`). A second version means vendor/indexmap's
        // `hashbrown = "0.16"` pin was undone or a new parent brought another
        // major.
        let versions = s.third_party().filter(|p| p.name == "hashbrown").count();
        assert_eq!(
            versions,
            measured::MAC_ARM_HASHBROWN_VERSIONS,
            "hashbrown resolves at {versions} version(s) on mac-arm; the dedup \
             to a single 0.16.1 has regressed"
        );
    }

    #[test]
    fn linux_stays_within_the_measured_baseline() {
        let s = assert_within_baseline(1);
        // AccessKit is an unconditional linux dependency
        // (crates/aterm-gui/Cargo.toml) for screen-reader support; this catches
        // the stack silently changing shape.
        for present in ["accesskit", "accesskit_unix", "accesskit_winit"] {
            assert!(
                s.graph.nodes.iter().any(|p| p.name == present),
                "`{present}` left the linux graph — linux carries the AccessKit \
                 tree unconditionally; if that changed, linux ships no a11y surface"
            );
        }
    }

    /// Every census-listed browser module fits inside a measured wasm cell,
    /// and the CPU module's graph is a subset of the GPU module's.
    #[test]
    fn the_wasm_cells_cover_every_shipped_web_crate() {
        let contained = |inner: &str, outer_cell_index: usize| {
            let outer = survey(outer_cell_index);
            let cell = crate::model::Cell {
                name: format!("probe-{inner}"),
                triple: "wasm32-unknown-unknown".to_string(),
                package: inner.to_string(),
            };
            let (g, _) = crate::resolve::real_workspace_graph_and_paths(&cell)
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
        let cells = crate::resolve::default_cells();
        for entry in aterm_census::WASM_ROOT_CRATES {
            let inner = entry.rsplit('/').next().expect("split never yields empty");
            if cells.iter().any(|c| c.package == inner) {
                continue;
            }
            contained(inner, 4);
        }
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

    /// `vendor/` holds both aterm's own code (`astream-broker`, `astream-cap`)
    /// and upstream forks (`winit`); `is_third_party` must tell them apart.
    #[test]
    fn a_first_party_vendored_crate_is_not_third_party_but_a_fork_still_is() {
        let s = survey(0);
        let by_name = |name: &str| {
            s.facts
                .iter()
                .find(|(id, _)| id.name == name)
                .unwrap_or_else(|| panic!("`{name}` must be in the mac-arm graph"))
                .1
        };
        for own in ["astream-broker", "astream-cap", "astream-wire"] {
            assert!(
                !by_name(own).is_third_party,
                "`{own}` is aterm's own code (provenance::FIRST_PARTY_VENDORED), vendored under \
                 vendor/ for a build reason, and must not be billed to the third-party surface"
            );
        }
        assert!(
            by_name("winit").is_third_party,
            "`vendor/winit` is a REDISTRIBUTED fork and must stay third-party — a provenance \
             rule that exempts everything under vendor/ would hide the largest single \
             dependency in this cell"
        );
        // And the chain the first-party crate DRAGS IN is still billed: being
        // aterm's own buys the crate's own lines, never its dependencies'.
        assert!(
            by_name("sha2").is_third_party,
            "sha2 is third-party no matter who reaches it"
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

    /// The unsafe-token instrument: the vendored `winit` fork counts far above
    /// zero, and the absent `objc2` family reads zero.
    #[test]
    fn unsafe_tokens_are_measured_and_the_objc2_family_is_gone() {
        let s = survey(0);
        let winit: u64 = s
            .third_party()
            .filter(|id| id.name == "winit")
            .map(|id| s.facts[id].unsafe_tokens)
            .sum();
        // A BLOCK count reads ~0 here; the token count is the honest one.
        assert!(winit > 1000, "winit unsafe tokens = {winit}");
        let family: Vec<String> = s
            .third_party()
            .filter(|id| {
                id.name.starts_with("objc2") || id.name == "objc-sys" || id.name == "block2"
            })
            .map(ToString::to_string)
            .collect();
        assert!(
            family.is_empty(),
            "the objc2 family is back in the shipped mac-arm graph: {family:?}"
        );
    }
}

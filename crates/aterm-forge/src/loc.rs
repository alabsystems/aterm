// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-package LOC, `unsafe` count, build-script, and license facts
//! ([`aterm_census::collect_rs_files`]).

use crate::model::{Cell, CellSurvey, Graph, PkgFacts, PkgId};
use crate::resolve;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Where a package's source lives, in precedence order ([`crate::attest`]).
pub fn package_dir(root: &Path, id: &PkgId, printed: Option<&Path>) -> Option<PathBuf> {
    if let Some(member) = workspace_members(root).get(&(id.name.clone(), id.version.clone())) {
        return Some(member.clone());
    }
    if let Some(dir) = patch_targets(root).get(&(id.name.clone(), id.version.clone()))
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
    printed.filter(|p| p.is_dir()).map(Path::to_path_buf)
}

/// (package, version) → directory for every `[patch.crates-io]` target.
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
    let doc = crate::read_regular_text(root.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<aterm_toml::edit::DocumentMut>().ok());
    if let Some(rows) = doc
        .as_ref()
        .and_then(|d| d.get("patch"))
        .and_then(|p| p.get("crates-io"))
        .and_then(aterm_toml::edit::Item::as_table_like)
    {
        for (key, row) in rows.iter() {
            let Some(path) = row.get("path").and_then(aterm_toml::edit::Item::as_str) else {
                continue;
            };
            let dir = root.join(path);
            let Some(version) = manifest_version(&dir) else {
                continue;
            };
            let name = crate::policy::patched_package(key, row).to_string();
            table.insert((name, version), dir);
        }
    }
    let leaked: &'static MemberIndex = Box::leak(Box::new(table));
    guard.insert(root.to_path_buf(), leaked);
    leaked
}

/// `[package] version` from a manifest directory, if it reads.
pub(crate) fn manifest_version(dir: &Path) -> Option<String> {
    let text = crate::read_regular_text(dir.join("Cargo.toml")).ok()?;
    package_version(&text)
}

/// (package name, version) → directory.
type MemberIndex = BTreeMap<(String, String), PathBuf>;

/// Map `(package name, version)` to its directory for every package under `crates/`.
fn workspace_members(root: &Path) -> &'static MemberIndex {
    static MEMBERS: OnceLock<Mutex<BTreeMap<PathBuf, &'static MemberIndex>>> = OnceLock::new();
    let cache = MEMBERS.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root)
    {
        return hit;
    }
    let workspace_version = crate::read_regular_text(root.join("Cargo.toml"))
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
    let mut found: MemberIndex = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            let dir = entry.path();
            let Ok(text) = crate::read_regular_text(dir.join("Cargo.toml")) else {
                continue;
            };
            let name = package_name(&text);
            if !name.is_empty() {
                let version = package_version(&text).unwrap_or_else(|| workspace_version.clone());
                found.insert((name, version), dir);
            }
        }
    }
    let leaked: &'static MemberIndex = Box::leak(Box::new(found));
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.to_path_buf(), leaked);
    leaked
}

/// Literal `[package] version` of a workspace member, or `None` when inherited.
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

/// `[package] name` out of a manifest, parsed like [`manifest_facts`].
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

/// `$CARGO_HOME/registry/src/index.crates.io-*`, discovered once.
fn registry_srcs() -> &'static [PathBuf] {
    static SRCS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    SRCS.get_or_init(|| {
        let Some(home) = cargo_home() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(home.join("registry").join("src")) {
            for entry in entries.flatten() {
                if !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("index.crates.io-"))
                {
                    continue;
                }
                let is_dir = entry
                    .file_type()
                    .map(|ft| ft.is_dir() || (ft.is_symlink() && entry.path().is_dir()))
                    .unwrap_or(false);
                if is_dir {
                    out.push(entry.path());
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

/// Measure facts for every node in `graph` using the `printed` directories from `cargo tree`.
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

/// Measure one package from scratch.
pub fn measure(root: &Path, id: &PkgId, printed: Option<&Path>) -> PkgFacts {
    let Some(dir) = package_dir(root, id, printed) else {
        return PkgFacts {
            is_third_party: true,
            ..PkgFacts::default()
        };
    };
    measure_directory(root, dir)
}

/// Measure the directory the resolver actually named, without falling back to
/// a same-named registry or workspace package from a different source.
fn measure_directory(root: &Path, dir: PathBuf) -> PkgFacts {
    let mut files = Vec::new();
    collect_rs(&dir, &mut files);
    let mut loc = 0u64;
    let mut unsafe_tokens = 0u64;
    for file in &files {
        let Ok(bytes) = crate::read_regular(file) else {
            continue;
        };
        loc += physical_lines(&bytes);
        unsafe_tokens += count_unsafe(&bytes);
    }
    let manifest = crate::read_regular_text(dir.join("Cargo.toml")).unwrap_or_default();
    let (license, is_proc_macro) = manifest_facts(&manifest);
    PkgFacts {
        loc,
        unsafe_tokens,
        has_build_rs: dir.join("build.rs").is_file(),
        is_proc_macro,
        license,
        is_third_party: !crate::provenance::is_first_party(root, &dir),
        root_dir: Some(dir),
    }
}

/// The process-wide memo.
fn cached_facts(root: &Path, id: &PkgId, printed: Option<&Path>) -> PkgFacts {
    static CACHE: OnceLock<Mutex<BTreeMap<(PathBuf, PkgId), PkgFacts>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let key = (root.to_path_buf(), id.clone());
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned()
    {
        return hit;
    }
    let fresh = measure(root, id, printed);
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, fresh.clone());
    fresh
}

/// Resolve the dependency graph and package facts for one cell (`survey`, `blame`).
pub fn survey_cell(cargo: &resolve::Cargo, root: &Path, cell: &Cell) -> Result<CellSurvey, String> {
    let mut log = String::new();
    let out = survey_cell_logged(cargo, root, cell, &mut log);
    if !log.is_empty() {
        eprint!("{log}");
    }
    out
}

/// [`survey_cell`] with the resolver's notes captured instead of printed.
pub fn survey_cell_logged(
    cargo: &resolve::Cargo,
    root: &Path,
    cell: &Cell,
    log: &mut String,
) -> Result<CellSurvey, String> {
    let (graph, mut paths) = resolve::graph_and_paths(cargo, root, cell, log)?;
    let mut resolved_git = Vec::new();
    if paths
        .values()
        .any(|path| path.to_string_lossy().contains("://"))
    {
        let packages = resolve::git_packages(cargo, root)?;
        for (id, path) in &mut paths {
            if !path.to_string_lossy().contains("://") {
                continue;
            }
            let package = packages
                .get(id)
                .ok_or_else(|| format!("Git package {id} has no resolved source directory"))?;
            if !resolve::matching_git_annotation(path, &package.source) {
                return Err(format!(
                    "Git package {id} source disagrees with its tree annotation"
                ));
            }
            if !package.directory.join("Cargo.toml").is_file() {
                return Err(format!("Git package {id} has no unpacked source manifest"));
            }
            resolved_git.push((id.clone(), package.clone()));
            *path = package.directory.clone();
        }
    }
    let mut facts = facts_with_paths(root, &graph, &paths);
    for (id, package) in resolved_git {
        let mut fact = measure_directory(root, package.directory);
        if crate::provenance::is_first_party_git(root, &id.name, &package.source) {
            fact.is_third_party = false;
            log.push_str(&format!(
                "    {id}: reviewed original Git component, same repository owner and full pinned commit\n"
            ));
        }
        facts.insert(id, fact);
    }
    Ok(CellSurvey {
        cell: cell.clone(),
        graph,
        facts,
    })
}

/// Every `*.rs` under `dir`, skipping `target/` and `.git`.
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
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Physical lines: every newline, plus a final line that lacks one.
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

/// `\bunsafe\b` occurrences.
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

const fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Read `package.license` and `[lib] proc-macro = true` from a manifest.
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
#[path = "loc_tests.rs"]
mod tests;

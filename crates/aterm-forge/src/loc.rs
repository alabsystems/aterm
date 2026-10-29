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
    let doc = std::fs::read_to_string(root.join("Cargo.toml"))
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
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    package_version(&text)
}

/// (package name, version) → directory.
type MemberIndex = BTreeMap<(String, String), PathBuf>;

/// Map `(package name, version)` to its directory for every package under `crates/`.
fn workspace_members(root: &Path) -> &'static MemberIndex {
    static MEMBERS: OnceLock<Mutex<BTreeMap<PathBuf, &'static MemberIndex>>> = OnceLock::new();
    let cache = MEMBERS.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(map) = cache.lock()
        && let Some(hit) = map.get(root)
    {
        return hit;
    }
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
    let mut found: MemberIndex = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            let dir = entry.path();
            let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
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
    if let Ok(mut map) = cache.lock() {
        map.insert(root.to_path_buf(), leaked);
    }
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
        is_third_party: !crate::provenance::is_first_party(root, &dir),
        root_dir: Some(dir),
    }
}

/// The process-wide memo.
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
    let (graph, paths) = resolve::graph_and_paths(cargo, root, cell, log)?;
    let facts = facts_with_paths(root, &graph, &paths);
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
        } else if path.extension().is_some_and(|e| e == "rs") {
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
mod tests {
    use super::*;

    #[test]
    fn manifest_facts_read_licence_and_the_proc_macro_flag() {
        let (lic, pm) = manifest_facts("[package]\nlicense = \"MIT OR Apache-2.0\"\n");
        assert_eq!(lic, "MIT OR Apache-2.0");
        assert!(!pm);
        let (_, pm) = manifest_facts("[package]\nname = \"x\"\n[lib]\nproc-macro = true\n");
        assert!(pm);
        let (lic, _) = manifest_facts("[package]\nlicense.workspace = true\n");
        assert_eq!(lic, "");
        assert_eq!(manifest_facts("this is not toml ][").0, "");
    }

    /// Where a package's source is measured, and whether it is billed as
    /// third-party, over a fixture workspace: a member answers by its MANIFEST
    /// name (`crates/aterm-tracing` is package `tracing`) and only at its own
    /// version; a patch target answers from the path that compiles, only at
    /// the fork's version; the printed directory is the last resort. Being
    /// first-party buys a crate its own lines, never what it drags in.
    #[test]
    fn package_dir_prefers_the_member_then_the_patch_and_bills_by_directory() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let w = |rel: &str, body: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, body).expect("write");
        };
        w(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n[workspace.package]\nversion = \"0.9.0\"\n\
             [patch.crates-io]\ntracing = { path = \"crates/aterm-tracing\" }\n\
             indexmap = { path = \"vendor/indexmap\" }\n",
        );
        w(
            "crates/aterm-tracing/Cargo.toml",
            "[package]\nname = \"tracing\"\nversion = \"0.1.44\"\n",
        );
        w(
            "crates/aterm-core/Cargo.toml",
            "[package]\nname = \"aterm-core\"\nversion.workspace = true\n",
        );
        w(
            "vendor/indexmap/Cargo.toml",
            "[package]\nname = \"indexmap\"\nversion_note = \"9.9.9\"\nversion = \"2.14.0\"\n",
        );
        w("vendor/astream/crates/astream-aead/src/lib.rs", "");
        w(
            "registry/chacha20poly1305-0.10.1/src/lib.rs",
            "pub fn seal() {}\n",
        );
        let dir = |name: &str, version: &str, printed: Option<&Path>| {
            package_dir(root, &PkgId::new(name, version), printed)
        };

        assert_eq!(
            dir("tracing", "0.1.44", None),
            Some(root.join("crates/aterm-tracing"))
        );
        assert_ne!(
            dir("tracing", "0.1.41", None),
            Some(root.join("crates/aterm-tracing")),
            "another version of the member's name is somebody else's source"
        );
        assert_eq!(
            dir("aterm-core", "0.9.0", None),
            Some(root.join("crates/aterm-core")),
            "an inherited version is the workspace's"
        );
        assert_eq!(
            dir("aterm-core", "9.9.9", None),
            None,
            "a member whose folder matches its package name still answers only at its own version"
        );
        assert_eq!(
            dir("indexmap", "2.14.0", None),
            Some(root.join("vendor/indexmap"))
        );
        assert_ne!(
            dir("indexmap", "0.0.0-not-published", None),
            Some(root.join("vendor/indexmap"))
        );
        let hint = root.join("registry/chacha20poly1305-0.10.1");
        assert_eq!(dir("nope-not-real", "0.0.1", Some(&hint)), Some(hint));

        let third =
            |id: PkgId, printed: &str| measure(root, &id, Some(&root.join(printed))).is_third_party;
        assert!(!third(
            PkgId::new("tracing", "0.1.44"),
            "crates/aterm-tracing"
        ));
        assert!(third(PkgId::new("indexmap", "2.14.0"), "vendor/indexmap"));
        assert!(!third(
            PkgId::new("astream-aead", "0.2.0"),
            "vendor/astream/crates/astream-aead"
        ));
        assert!(third(
            PkgId::new("chacha20poly1305", "0.10.1"),
            "registry/chacha20poly1305-0.10.1"
        ));
    }
}

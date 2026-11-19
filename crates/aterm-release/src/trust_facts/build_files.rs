// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bind a compiler consumer to the targets and regular files of its measured package.

use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ScriptSource {
    File(PathBuf),
    Generated,
    Unknown,
}

impl ScriptSource {
    fn from_metadata(package: &str, name: &str, path: Option<&Value>) -> Self {
        match (name.strip_prefix("metabuild-"), path) {
            // The pinned driver generates this one target from the package manifest
            // and serializes its source as an explicit null.
            (Some(owner), Some(Value::Null)) if owner == package => Self::Generated,
            (None, Some(Value::String(path)))
                if !path.contains('\0') && Path::new(path).is_absolute() =>
            {
                Self::File(PathBuf::from(path))
            }
            _ => Self::Unknown,
        }
    }

    /// The caller has already established that the manifest is keyed.
    pub(super) fn covered_by(&self, file: impl FnOnce(&Path) -> bool) -> bool {
        match self {
            Self::File(path) => file(path),
            Self::Generated => true,
            Self::Unknown => false,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Targets {
    examples: BTreeMap<String, bool>,
    scripts: BTreeMap<String, ScriptSource>,
}

fn library_types(types: &[Value]) -> bool {
    !types.is_empty()
        && types.iter().all(|kind| {
            matches!(
                kind.as_str(),
                Some("lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro")
            )
        })
}

impl Targets {
    fn from_metadata(package: &str, row: &Value) -> Option<Self> {
        let mut targets = Self::default();
        for target in row["targets"].as_array().filter(|rows| !rows.is_empty())? {
            let name = target["name"].as_str().filter(|name| !name.is_empty())?;
            let kind = target["kind"].as_array()?.as_slice();
            let types = target["crate_types"].as_array()?.as_slice();
            let binary = matches!(types, [ty] if ty.as_str() == Some("bin"));
            match kind {
                [k] if k.as_str() == Some("example") && (binary || library_types(types)) => {
                    if targets.examples.insert(name.to_owned(), binary).is_some() {
                        return None;
                    }
                }
                [k] if k.as_str() == Some("custom-build") && binary => {
                    // The pinned driver derives the first spelling from the script stem
                    // and the second from the package name for a generated metabuild.
                    let suffix = name
                        .strip_prefix("build-script-")
                        .or_else(|| name.strip_prefix("metabuild-"))?;
                    if suffix.is_empty() || suffix.contains(['/', '\0']) {
                        return None;
                    }
                    // Preserve every declared target, including one whose source cannot be identified.
                    let source = ScriptSource::from_metadata(package, name, target.get("src_path"));
                    if targets.scripts.insert(name.to_owned(), source).is_some() {
                        return None;
                    }
                }
                [k] if matches!(k.as_str(), Some("bin" | "test" | "bench")) && binary => {}
                kinds if library_types(kinds) && kinds == types => {}
                _ => return None,
            }
        }
        Some(targets)
    }
}

#[derive(Clone, Debug)]
struct Package {
    manifest: PathBuf,
    targets: Option<Targets>,
}

/// Only live Targo metadata supplies package locations and target kinds. They never enter the text plan.
#[derive(Clone, Debug, Default)]
pub(crate) struct Packages {
    root: PathBuf,
    physical: Option<PathBuf>,
    packages: BTreeMap<String, Option<Package>>,
}

impl Packages {
    pub(crate) fn from_metadata(meta: &aterm_json::Value, repo: &Path) -> Self {
        let mut packages = Self {
            root: std::path::absolute(repo).unwrap_or_default(),
            physical: repo.canonicalize().ok(),
            packages: BTreeMap::new(),
        };
        if let Some(rows) = meta.get("packages").and_then(aterm_json::Value::as_array) {
            for row in rows {
                let Some(name) = row.get("name").and_then(aterm_json::Value::as_str) else {
                    continue;
                };
                let package = row
                    .get("manifest_path")
                    .and_then(aterm_json::Value::as_str)
                    .map(Path::new)
                    .filter(|path| path.ends_with("Cargo.toml"))
                    .and_then(|path| packages.strip_root(path))
                    .filter(|rel| {
                        rel.components()
                            .all(|part| matches!(part, Component::Normal(_)))
                    })
                    .map(|path| Package {
                        manifest: path.to_owned(),
                        targets: Targets::from_metadata(name, row),
                    });
                // Distinct versions or sources can share a package name in Targo metadata.
                packages
                    .packages
                    .entry(name.to_owned())
                    .and_modify(|slot| *slot = None)
                    .or_insert(package);
            }
        }
        packages
    }

    fn targets(&self, package: &str) -> Option<&Targets> {
        self.packages.get(package)?.as_ref()?.targets.as_ref()
    }

    /// The example name here is the exact target name, before Rust crate-name normalization.
    pub(super) fn example_is_binary(&self, package: &str, name: &str) -> Option<bool> {
        self.targets(package)?.examples.get(name).copied()
    }

    /// Preserve the exact target spellings even when two scripts share a Rust crate name.
    /// An empty inventory establishes that the package has no custom-build target.
    /// Each entry records how its source can be checked against the tree and the pinned driver.
    pub(super) fn build_scripts(&self, package: &str) -> Option<&BTreeMap<String, ScriptSource>> {
        Some(&self.targets(package)?.scripts)
    }

    fn strip_root<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        if !path.is_absolute() || !self.root.is_absolute() {
            return None;
        }
        path.strip_prefix(&self.root)
            .ok()
            .or_else(|| path.strip_prefix(self.physical.as_ref()?).ok())
    }

    /// A regular blob's ancestors in the same Git tree are directories. Neither a
    /// symlink nor a path through one can appear in this set. Filing also requires the
    /// working checkout to match that tree, including its link and blob contents.
    pub(super) fn covers(&self, regular: &BTreeSet<PathBuf>, package: &str, path: &Path) -> bool {
        let Some(Some(package)) = self.packages.get(package) else {
            return false;
        };
        let manifest = &package.manifest;
        if !regular.contains(manifest) {
            return false;
        }
        // Targo bounds observed paths at 32 KiB. A trailing slash or /. requires a
        // directory even though Path::components and strip_prefix discard it.
        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.len() > 32 * 1024 || bytes.ends_with(b"/") || bytes.ends_with(b"/.") {
            return false;
        }
        let (mut resolved, input) = if path.is_absolute() {
            let Some(rel) = self.strip_root(path) else {
                return false;
            };
            (PathBuf::new(), rel)
        } else {
            let Some(dir) = manifest.parent() else {
                return false;
            };
            (dir.to_owned(), path)
        };
        for component in input.components() {
            match component {
                Component::Normal(name) => resolved.push(name),
                Component::CurDir => {}
                Component::ParentDir => {
                    // A keyed descendant proves every hop to this directory, even
                    // when the .. removes those components from the final path.
                    let directory = regular
                        .range::<Path, _>((Excluded(resolved.as_path()), Unbounded))
                        .next()
                        .is_some_and(|next| next.starts_with(&resolved));
                    if !directory || !resolved.pop() {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        regular.contains(&resolved)
    }
}

#[cfg(test)]
#[path = "build_files_tests.rs"]
mod tests;

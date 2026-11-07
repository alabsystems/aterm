// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conservative exclusion of packages outside the reader's Cargo dependency graph.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Paths in these packages cannot change the reader's compiled dependency closure.
/// Everything outside a known package, and every manifest, remains an input.
#[derive(Debug)]
pub(super) struct Inputs {
    unrelated: Vec<PathBuf>,
    related: Vec<PathBuf>,
}

impl Inputs {
    pub(super) fn from_metadata(bytes: &[u8], root: &Path) -> Option<Self> {
        let meta: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        let packages = meta.get("packages")?.as_array()?;
        let nodes = meta.get("resolve")?.get("nodes")?.as_array()?;
        let reader = packages
            .iter()
            .find(|p| p.get("name").and_then(|v| v.as_str()) == Some("aterm-gui"))?
            .get("id")?
            .as_str()?;
        let edges: BTreeMap<_, _> = nodes
            .iter()
            .map(|node| {
                Some((
                    node.get("id")?.as_str()?,
                    node.get("dependencies")?.as_array()?,
                ))
            })
            .collect::<Option<_>>()?;
        let mut seen = BTreeSet::new();
        let mut pending = vec![reader];
        while let Some(id) = pending.pop() {
            if seen.insert(id) {
                for dep in *edges.get(id)? {
                    pending.push(dep.as_str()?);
                }
            }
        }
        let mut unrelated = Vec::new();
        let mut related = Vec::new();
        for package in packages {
            let id = package.get("id")?.as_str()?;
            let manifest = Path::new(package.get("manifest_path")?.as_str()?);
            // Registry dependencies are sealed by Cargo.lock. An unrecognised local
            // path is never excluded: a diff there forces another judgment.
            if let Ok(path) = manifest.parent()?.strip_prefix(root) {
                if path.as_os_str().is_empty() {
                    continue;
                }
                if seen.contains(id) {
                    related.push(path.to_path_buf());
                } else {
                    unrelated.push(path.to_path_buf());
                }
            }
        }
        Some(Self { unrelated, related })
    }

    pub(super) fn ignores(&self, path: &Path) -> bool {
        // Any package manifest can alter resolution or feature unification. Keep
        // configuration files too, even inside an otherwise unrelated package.
        path.file_name().is_some_and(|name| name != "Cargo.toml")
            && !path.components().any(|part| part.as_os_str() == ".cargo")
            && !self.related.iter().any(|root| path.starts_with(root))
            && self.unrelated.iter().any(|root| path.starts_with(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_edges_keep_transitive_build_and_dev_dependencies_as_inputs() {
        let metadata = aterm_json::json!({
            "packages": [
                {"name":"aterm-gui", "id":"gui", "manifest_path":"/tree/crates/gui/Cargo.toml"},
                {"name":"helper", "id":"helper", "manifest_path":"/tree/crates/helper/Cargo.toml"},
                {"name":"leaf", "id":"leaf", "manifest_path":"/tree/crates/leaf/Cargo.toml"},
                {"name":"other", "id":"other", "manifest_path":"/tree/crates/other/Cargo.toml"}
            ],
            "resolve":{"nodes":[
                {"id":"gui", "dependencies":["helper"]},
                {"id":"helper", "dependencies":["leaf"]},
                {"id":"leaf", "dependencies":[]},
                {"id":"other", "dependencies":[]}
            ]}
        });
        let inputs =
            Inputs::from_metadata(metadata.to_string().as_bytes(), Path::new("/tree")).unwrap();
        assert!(inputs.ignores(Path::new("crates/other/src/lib.rs")));
        for path in [
            "crates/gui/src/lib.rs",
            "crates/helper/build.rs",
            "crates/leaf/src/lib.rs",
            "crates/other/Cargo.toml",
            "crates/other/.cargo/config.toml",
            "Cargo.lock",
            "new-package/src/lib.rs",
            "README.md",
        ] {
            assert!(!inputs.ignores(Path::new(path)), "{path}");
        }
        // Removing the leaf edge is the negative control: exactly that package
        // becomes excludable, so a hand list or a non-transitive walk fails above.
        let mut mutant = metadata;
        mutant
            .as_object_mut()
            .unwrap()
            .get_mut("resolve")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("nodes")
            .unwrap()
            .as_array_mut()
            .unwrap()[1]
            .as_object_mut()
            .unwrap()
            .insert("dependencies".into(), aterm_json::json!([]));
        assert!(
            Inputs::from_metadata(mutant.to_string().as_bytes(), Path::new("/tree"))
                .unwrap()
                .ignores(Path::new("crates/leaf/src/lib.rs"))
        );
    }
}

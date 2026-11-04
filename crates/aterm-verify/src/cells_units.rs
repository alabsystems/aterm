// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Target packages and full build inputs, projected from the shared typed Cargo unit graph.

use aterm_forge::model::PkgId;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Projection {
    pub targets: BTreeMap<String, BTreeSet<PkgId>>,
    pub inputs: BTreeMap<String, BTreeSet<PkgId>>,
}

/// Project exactly the requested roots, preserving indexed feature and host/target contexts.
/// # Errors
/// Malformed Cargo context, a missing or extra root, or an invalid package identity.
pub(super) fn read(
    text: &str,
    triple: &str,
    roots: &BTreeSet<String>,
) -> Result<Projection, String> {
    let graph = crate::unitgraph::Graph::read(text, &mut crate::unitgraph::Interner::default())?;
    for unit in &graph.units {
        if unit.platform.as_deref().is_some_and(|p| p != triple)
            || !matches!(unit.mode.as_str(), "check" | "build" | "run-custom-build")
        {
            return Err(
                "the all-targets check unit graph has an unexpected platform or mode".into(),
            );
        }
    }
    let package = |unit: &crate::unitgraph::Unit| -> Result<PkgId, String> {
        let fragment = unit
            .package_id
            .rsplit_once('#')
            .map(|(_, f)| f)
            .ok_or_else(|| format!("unit package identity has no version: {}", unit.package_id))?;
        let version = fragment.rsplit_once('@').map_or(fragment, |(_, v)| v);
        let name = crate::unitgraph::package_name(&unit.package_id);
        if name.is_empty() || version.is_empty() {
            return Err(format!(
                "invalid unit package identity: {}",
                unit.package_id
            ));
        }
        Ok(PkgId::new(name, version))
    };
    let target = |unit: &crate::unitgraph::Unit| {
        unit.platform.as_deref() == Some(triple)
            && matches!(unit.mode.as_str(), "check" | "build")
            && !unit
                .target_kinds
                .iter()
                .any(|k| matches!(k.as_str(), "custom-build" | "proc-macro"))
    };
    let mut seen = BTreeSet::new();
    let mut by_root: BTreeMap<String, BTreeSet<PkgId>> = BTreeMap::new();
    let mut inputs: BTreeMap<String, BTreeSet<PkgId>> = BTreeMap::new();
    for &root in &graph.roots {
        let unit = &graph.units[root];
        let id = package(unit)?;
        seen.insert(id.name.clone());
        let build = inputs.entry(id.name.clone()).or_default();
        let reached = by_root.entry(id.name).or_default();
        // Host macros have no target coverage, but their inputs affect consumers.
        if !(target(unit)
            || unit.platform.is_none() && unit.target_kinds.iter().any(|k| k == "proc-macro"))
        {
            return Err("a selected all-targets root has no target or proc-macro context".into());
        }
        let mut pending = vec![root];
        let mut visited = BTreeSet::new();
        while let Some(index) = pending.pop() {
            if !visited.insert(index) {
                continue;
            }
            let unit = &graph.units[index];
            build.insert(package(unit)?);
            if target(unit) {
                reached.insert(package(unit)?);
            }
            pending.extend(unit.dependencies.iter().copied());
        }
    }
    if seen != *roots {
        return Err(format!(
            "the all-targets unit graph's roots {seen:?} are not the requested members {roots:?}"
        ));
    }
    Ok(Projection {
        targets: by_root,
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_json::Value;

    const TRIPLE: &str = "aarch64-unknown-linux-gnu";

    fn unit(name: &str, dependencies: &[usize]) -> Value {
        aterm_json::json!({
            "pkg_id": format!("path+file:///fixture/{name}#1.0.0"),
            "target": {"name": name, "kind": ["lib"], "crate_types": ["lib"]},
            "platform": TRIPLE, "mode": "check", "profile": {}, "features": [],
            "dependencies": dependencies.iter().map(|i| aterm_json::json!({
                "index": i, "extern_crate_name": format!("dependency_{i}"),
                "public": false, "noprelude": false
            })).collect::<Vec<_>>()
        })
    }

    #[test]
    fn feature_instances_do_not_mix_the_dependencies_of_different_roots() {
        let mut units = vec![
            unit("a", &[2, 6]),
            unit("b", &[3]),
            unit("shared", &[4]),
            unit("shared", &[5]),
            unit("left", &[]),
            unit("right", &[]),
            unit("host", &[]),
        ];
        units[2]
            .as_object_mut()
            .unwrap()
            .insert("features".into(), aterm_json::json!(["left"]));
        units[3]
            .as_object_mut()
            .unwrap()
            .insert("features".into(), aterm_json::json!(["right"]));
        units[6]
            .as_object_mut()
            .unwrap()
            .insert("platform".into(), Value::Null);
        let text = aterm_json::json!({"version": 1, "units": units, "roots": [0, 1]}).to_string();
        let roots = BTreeSet::from(["a".into(), "b".into()]);
        let projection = read(&text, TRIPLE, &roots).expect("the unit graph");
        assert!(projection.inputs["a"].contains(&PkgId::new("host", "1.0.0")));
        let graph = projection.targets;
        let names = |root: &str| {
            graph[root]
                .iter()
                .map(|id| id.name.as_str())
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(names("a"), BTreeSet::from(["a", "shared", "left"]));
        assert_eq!(names("b"), BTreeSet::from(["b", "shared", "right"]));
        assert!(read(&text, TRIPLE, &BTreeSet::from(["a".into()])).is_err());
    }

    #[test]
    fn missing_roots_and_host_only_ordinary_roots_are_refused() {
        let roots = BTreeSet::from(["a".into()]);
        let graph = aterm_json::json!({"version": 1, "units": [unit("a", &[])], "roots": [9]});
        assert!(read(&graph.to_string(), TRIPLE, &roots).is_err());
        let mut host = unit("a", &[]);
        host.as_object_mut()
            .unwrap()
            .insert("platform".into(), Value::Null);
        let graph = aterm_json::json!({"version": 1, "units": [host], "roots": [0]});
        assert!(read(&graph.to_string(), TRIPLE, &roots).is_err());
    }
}

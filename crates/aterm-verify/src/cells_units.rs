// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The target dependencies of each selected member, projected from Cargo's compact unit graph.

use std::collections::{BTreeMap, BTreeSet};

use aterm_forge::model::PkgId;
use aterm_json::Value;

struct Unit {
    package: PkgId,
    target: bool,
    proc_macro: bool,
    dependencies: Vec<usize>,
}

fn index(value: &Value, count: usize) -> Result<usize, String> {
    value
        .as_u64()
        .and_then(|i| usize::try_from(i).ok())
        .filter(|i| *i < count)
        .ok_or_else(|| "the unit graph references a missing unit".to_string())
}

fn package(id: &str) -> Result<PkgId, String> {
    let name = super::package_id_name(id);
    let version = id
        .rsplit_once('#')
        .map(|(_, fragment)| fragment.rsplit('@').next().unwrap_or_default())
        .or_else(|| id.split_whitespace().nth(1))
        .filter(|version| !version.is_empty())
        .ok_or_else(|| format!("the unit graph has a package without a version: {id}"))?;
    if name.is_empty() {
        return Err("the unit graph has an unnamed package".into());
    }
    Ok(PkgId::new(name, version))
}

/// Read exactly `roots`' target dependencies. Unit indices preserve feature and host/target
/// distinctions until each root's traversal is complete; package names alone cannot do so.
/// # Errors
/// A malformed unit graph, a missing or extra root, or an ordinary root with no target unit.
pub(super) fn read(
    text: &str,
    triple: &str,
    roots: &BTreeSet<String>,
) -> Result<BTreeMap<String, BTreeSet<PkgId>>, String> {
    // Reuse the shared version, dependency-index and cycle validation.
    crate::unitgraph::Graph::read(text, &mut crate::unitgraph::Interner::default()).map(|_| ())?;
    let graph: Value = aterm_json::from_str(text).map_err(|error| error.to_string())?;
    let raw = graph
        .get("units")
        .and_then(Value::as_array)
        .ok_or("the unit graph lists no units")?;
    let units = raw
        .iter()
        .map(|unit| {
            let id = unit
                .get("pkg_id")
                .and_then(Value::as_str)
                .ok_or("unit has no package id")?;
            let kinds = unit
                .pointer("/target/kind")
                .and_then(Value::as_array)
                .filter(|kinds| !kinds.is_empty())
                .ok_or("unit has no target kinds")?
                .iter()
                .map(|kind| kind.as_str().ok_or("unit has a malformed target kind"))
                .collect::<Result<Vec<_>, _>>()?;
            let platform = unit.get("platform").ok_or("unit has no platform")?;
            if !platform.is_null() && platform.as_str().is_none() {
                return Err("unit has a malformed platform".to_string());
            }
            let proc_macro = kinds.contains(&"proc-macro");
            let target = platform.as_str() == Some(triple)
                && !proc_macro
                && !kinds.contains(&"custom-build");
            let dependencies = unit
                .get("dependencies")
                .and_then(Value::as_array)
                .ok_or("unit has no dependency list")?
                .iter()
                .map(|dep| index(dep.get("index").unwrap_or(&Value::Null), raw.len()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Unit {
                package: package(id)?,
                target,
                proc_macro,
                dependencies,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let root_units = graph
        .get("roots")
        .and_then(Value::as_array)
        .ok_or("the unit graph lists no roots")?
        .iter()
        .map(|root| index(root, units.len()))
        .collect::<Result<Vec<_>, _>>()?;
    let seen: BTreeSet<String> = root_units
        .iter()
        .map(|i| units[*i].package.name.clone())
        .collect();
    let covered: BTreeSet<String> = root_units
        .iter()
        .filter(|i| units[**i].target || units[**i].proc_macro)
        .map(|i| units[*i].package.name.clone())
        .collect();
    if seen != *roots || covered != *roots {
        return Err(format!(
            "unit graph roots {seen:?}, with target or macro roots {covered:?}, are not selected members {roots:?}"
        ));
    }
    let mut dependencies: BTreeMap<String, BTreeSet<PkgId>> = roots
        .iter()
        .map(|root| (root.clone(), BTreeSet::new()))
        .collect();
    for root in root_units {
        let packages = dependencies
            .get_mut(&units[root].package.name)
            .ok_or("the unit graph has an unselected root")?;
        let mut seen = BTreeSet::new();
        let mut todo = vec![root];
        while let Some(i) = todo.pop() {
            let unit = &units[i];
            if !unit.target || !seen.insert(i) {
                continue;
            }
            packages.insert(unit.package.clone());
            todo.extend(unit.dependencies.iter().copied());
        }
    }
    Ok(dependencies)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIPLE: &str = "aarch64-unknown-linux-gnu";

    fn unit(name: &str, dependencies: &[usize]) -> Value {
        aterm_json::json!({
            "pkg_id": format!("path+file:///fixture/{name}#1.0.0"),
            "target": {"name": name, "kind": ["lib"], "crate_types": ["lib"]},
            "platform": TRIPLE, "mode": "check", "profile": {}, "features": [],
            "dependencies": dependencies.iter().map(|i| aterm_json::json!({"index": i})).collect::<Vec<_>>()
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
        let graph = read(&text, TRIPLE, &roots).expect("the unit graph");
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

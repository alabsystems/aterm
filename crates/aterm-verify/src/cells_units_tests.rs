// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

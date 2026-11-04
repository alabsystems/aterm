// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compact unit closures preserve root-dev, target-version and host contexts.

use super::*;
use crate::unitgraph::tests::{Fixture, graph_in};

fn names(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn browser_impact_preserves_the_shipped_roots_default_feature_choices() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nresolver='2'\nmembers=['browser','engine','native-tool']\n",
    )
    .unwrap();
    for (name, deps) in [
        (
            "browser",
            "[dependencies]\nengine={path='../engine',default-features=false}\n",
        ),
        (
            "engine",
            "[features]\ndefault=['dep:native-tool']\n[build-dependencies]\nnative-tool={path='../native-tool',optional=true}\n",
        ),
        ("native-tool", ""),
    ] {
        std::fs::create_dir_all(root.join(name).join("src")).unwrap();
        std::fs::write(root.join(name).join("src/lib.rs"), "").unwrap();
        std::fs::write(
            root.join(name).join("Cargo.toml"),
            format!("[package]\nname='{name}'\nversion='1.0.0'\nedition='2021'\n{deps}"),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("engine/build.rs"),
        "fn main() { panic!(\"graph resolution must not execute this\"); }",
    )
    .unwrap();
    std::fs::write(root.join("Cargo.lock"), "version=4\n[[package]]\nname='browser'\nversion='1.0.0'\ndependencies=['engine']\n[[package]]\nname='engine'\nversion='1.0.0'\ndependencies=['native-tool']\n[[package]]\nname='native-tool'\nversion='1.0.0'\n").unwrap();
    let mut ctx = Ctx::new(
        root.to_owned(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        root.join("scratch"),
    );
    ctx.tools.targo = PathBuf::from(env!("CARGO"));
    ctx.path_env = std::env::var_os("PATH").unwrap_or_default();
    let cell = Cell {
        name: "browser".into(),
        triple: "wasm32-unknown-unknown".into(),
        package: "browser".into(),
    };
    let members = Members {
        names: names(&["browser", "engine", "native-tool"]),
        ..Members::default()
    };
    let normal = names(&["browser", "engine"]);
    let graph = member_graph(&ctx, &cell, &members, &normal, false).unwrap();
    assert_eq!(graph.selected, ["browser"]);
    assert_eq!(
        graph.dependencies.keys().cloned().collect::<BTreeSet<_>>(),
        names(&["browser"])
    );
    assert!(
        !graph.dependencies["browser"]
            .iter()
            .any(|id| id.name == "native-tool")
    );
    let changed = crate::Scope::changed("main", vec!["native-tool".into()], true);
    assert!(wanted_in_cell(&normal, Some(&graph.dependencies), &changed).is_empty());
}

#[test]
fn exact_root_units_keep_dev_bench_and_example_edges_out_of_other_roots() {
    let units: &[Fixture<'_>] = &[
        ("app", "app", "lib", "check", "0", &[], &[4, 10, 11]),
        ("app", "app", "lib", "check", "0", &["test"], &[4, 5]),
        ("app", "probe", "bench", "check", "0", &[], &[4, 6]),
        ("app", "sample", "example", "check", "0", &[], &[4, 6]),
        ("shared", "shared", "lib", "check", "0", &["target"], &[]),
        ("oracle", "oracle", "lib", "check", "0", &[], &[]),
        ("bench-only", "bench_only", "lib", "check", "0", &[], &[]),
        ("later", "later", "lib", "check", "0", &[], &[4]),
        ("shared", "shared", "lib", "build", "0", &["host"], &[9]),
        ("host-only", "host_only", "lib", "build", "0", &[], &[]),
        ("pm", "pm", "proc-macro", "build", "0", &[], &[8]),
        ("shared-two", "shared", "lib", "check", "0", &[], &[]),
        ("pm", "pm", "proc-macro", "check", "0", &[], &[8]),
    ];
    let platforms = [
        Some("cell"),
        Some("cell"),
        Some("cell"),
        Some("cell"),
        Some("cell"),
        Some("cell"),
        Some("cell"),
        Some("cell"),
        None,
        None,
        None,
        Some("cell"),
        None,
    ];
    let raw = graph_in(units, &[0, 1, 2, 3, 7, 12], &platforms).replace(
        "path+file:///w/crates/shared-two#0.1.0",
        "registry+https://example.invalid/index#shared@2.0.0",
    );
    let expected = names(&["app", "later", "pm"]);
    let projection = units::read(&raw, "cell", &expected).unwrap();
    assert_eq!(
        dependents_in_graph(&projection.inputs, &names(&["host-only"])),
        names(&["app", "pm"])
    );
    let graph = projection.targets;
    assert_eq!(
        dependents_in_graph(&graph, &names(&["shared"])),
        names(&["app", "later"])
    );
    assert_eq!(
        dependents_in_graph(&graph, &names(&["oracle"])),
        names(&["app"])
    );
    assert_eq!(
        dependents_in_graph(&graph, &names(&["bench-only"])),
        names(&["app"])
    );
    assert!(dependents_in_graph(&graph, &names(&["host-only"])).is_empty());
    assert!(graph["pm"].is_empty());
    assert_eq!(
        graph["later"],
        BTreeSet::from([PkgId::new("later", "0.1.0"), PkgId::new("shared", "0.1.0")])
    );
    let nodes: BTreeSet<_> = graph.values().flatten().cloned().collect();
    assert!(nodes.contains(&PkgId::new("shared", "0.1.0")));
    assert!(nodes.contains(&PkgId::new("shared", "2.0.0")));
    assert!(!nodes.contains(&PkgId::new("host-only", "0.1.0")));
    assert!(!nodes.contains(&PkgId::new("pm", "0.1.0")));
    assert!(units::read(&raw, "cell", &names(&["app", "later"])).is_err());
    assert!(units::read(&raw, "other-target", &expected).is_err());
    assert!(
        units::read(
            &raw.replace("\"platform\":null", "\"platform\":\"cell\""),
            "cell",
            &expected
        )
        .is_err()
    );
}

#[test]
fn shared_diamond_closures_do_not_expand_dependency_paths() {
    // Each pair depends on both nodes in the next pair. Unrolling paths is exponential;
    // Cargo's unit table and the closure walk retain each indexed node once per root.
    const PAIRS: usize = 64;
    let packages: Vec<String> = (0..PAIRS * 2).map(|n| format!("node{n}")).collect();
    let edges: Vec<Vec<usize>> = (0..PAIRS * 2)
        .map(|n| {
            let next = (n / 2 + 1) * 2;
            if next < PAIRS * 2 {
                vec![next, next + 1]
            } else {
                Vec::new()
            }
        })
        .collect();
    let units: Vec<Fixture<'_>> = packages
        .iter()
        .zip(&edges)
        .map(|(name, deps)| {
            (
                name.as_str(),
                name.as_str(),
                "lib",
                "check",
                "0",
                &[][..],
                deps.as_slice(),
            )
        })
        .collect();
    let raw = graph_in(&units, &[0, 1], &vec![Some("cell"); units.len()]);
    let graph = units::read(&raw, "cell", &names(&["node0", "node1"]))
        .unwrap()
        .targets;
    assert_eq!(
        graph
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>()
            .len(),
        PAIRS * 2
    );
    assert_eq!(graph["node0"].len(), PAIRS * 2 - 1);
    assert_eq!(graph["node1"].len(), PAIRS * 2 - 1);
    assert_eq!(
        dependents_in_graph(&graph, &names(&["node127"])),
        names(&["node0", "node1"])
    );
    assert_eq!(
        dependents_in_graph(&graph, &names(&["node0"])),
        names(&["node0"])
    );
}

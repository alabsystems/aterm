// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::scope::Scope;

#[test]
fn new_independent_exclusion_provenance_changes_still_pull_the_real_attestation() {
    let root = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets the test manifest directory"),
    )
    .join("../..");
    assert_eq!(
        forge_pull(&DEV, false, &root, |package| package == "aterm-forge"),
        Some("aterm-forge".into()),
        "a narrow provenance-policy change must not hide the notary's verdict"
    );
}

#[test]
fn a_dev_selection_touching_a_fork_pulls_forge_and_nothing_weaker_does() {
    let packages = || Ok(vec!["aterm-window".to_string(), "vendored".to_string()]);
    assert_eq!(
        forge_pull_reason(&DEV, false, packages, |p| p == "aterm-window"),
        Some("aterm-window".to_string())
    );
    assert_eq!(
        forge_pull_reason(&DEV, false, packages, |p| p == "dependent-only"),
        None,
        "a dependent in the cone did not itself change the fork"
    );
    assert_eq!(
        forge_pull_reason(
            &[Check::Lint],
            false,
            || panic!("a non-dev check must not parse the fork registries"),
            |_| true,
        ),
        None,
        "an explicitly narrow check stays narrow"
    );
    assert_eq!(
        forge_pull_reason(
            &DEV,
            true,
            || panic!("an exact release/as-release set must not parse the fork registries"),
            |_| true,
        ),
        None,
        "release/as-release selections pull nothing"
    );
    assert!(
        forge_pull_reason(&DEV, false, || Err("bad policy".into()), |_| false).is_some(),
        "an unreadable authority must schedule the check that diagnoses it"
    );
}

/// The release is its tree half then its bundle half, which share no check, and a release
/// check is in the bundle half exactly when it plans a timed stage: the tree half runs nothing
/// that wants a quiet machine. Negative control: appkit back in the tree half (the set before
/// B3) plans its timed redraw and objc stages there, and the second law fails.
#[test]
fn the_release_is_its_tree_half_and_its_timed_bundle_half() {
    assert_eq!(RELEASE[..RELEASE_TREE.len()], RELEASE_TREE);
    assert_eq!(RELEASE[RELEASE_TREE.len()..], RELEASE_BUNDLE);
    assert!(RELEASE_TREE.iter().all(|c| !RELEASE_BUNDLE.contains(c)));
    for c in RELEASE {
        let timed = stages_of(&[c]).into_iter().any(StageId::timed);
        assert_eq!(RELEASE_BUNDLE.contains(&c), timed, "{c:?}");
    }
}

#[test]
fn list_renders_each_checks_needs_from_the_tools_table() {
    let test = needs_of(Check::Test);
    for want in [
        "fish (verify provision)",
        "bash-5.3 (verify provision)",
        "pwsh (verify provision)",
        "cc (CLT)",
        "zstd (unprovided)",
        "vim (OS)",
    ] {
        assert!(test.contains(want), "{want} in {test}");
    }
    // The embedded verifier's tests run in the Linux guest, so the dev test check
    // needs neither of its tools on a Mac.
    assert!(
        !test.contains("python3") && !test.contains("openssl3"),
        "{test}"
    );
    assert_eq!(
        needs_of(Check::Linux),
        "Apple silicon with Rosetta, limactl (verify provision --linux), network for the first \
         provision"
    );
    // Census needs no tool; a partial clone's baseline import names the network.
    let census = needs_of(Check::Census);
    assert!(
        census.starts_with("in a blobless partial clone") && census.contains("network"),
        "{census}"
    );
    let table = list_table();
    assert!(!table.contains("brew"), "{table}");
    for spec in CHECKS {
        let needs = needs_of(spec.check);
        assert!(
            !spec.needs.contains(&"node") && !needs.split(", ").any(|w| w.starts_with("node ")),
            "{}: {needs}",
            spec.name
        );
    }
    assert!(!table.lines().any(|l| l.starts_with("node ")), "{table}");
    for check in [Check::Test, Check::WasmSmoke, Check::WasmGlue] {
        assert!(
            needs_of(check).contains("chrome-headless-shell (verify provision)"),
            "{}",
            needs_of(check)
        );
    }
    let fish = aterm_uds::host::tool("fish");
    let line = table
        .lines()
        .find(|l| l.starts_with("fish "))
        .unwrap_or_else(|| panic!("no fish row in {table}"));
    assert!(
        line.contains(" verify provision ") && line.ends_with(fish.why),
        "{line}"
    );
    assert_eq!(table.lines().filter(|l| l.starts_with("fish ")).count(), 1);
}

#[test]
fn every_word_names_one_thing() {
    assert_eq!(lookup("dev"), Some(Word::Dev));
    assert_eq!(lookup("release"), Some(Word::Release));
    assert_eq!(lookup("list"), Some(Word::List));
    assert_eq!(lookup("lint"), Some(Word::Check(Check::Lint)));
    assert_eq!(lookup("wasm-smoke"), Some(Word::Check(Check::WasmSmoke)));
    assert_eq!(lookup("wasm-glue"), Some(Word::Check(Check::WasmGlue)));
    assert_eq!(lookup("browser"), Some(Word::Check(Check::Browser)));
    assert_eq!(lookup("contract-probe"), None);
    assert_eq!(lookup("tippy"), None);
    let mut names: Vec<&str> = CHECKS.iter().map(|s| s.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), CHECKS.len());
    for s in &CHECKS {
        assert_eq!(s.check.spec().name, s.name);
        assert!(!s.stages.is_empty(), "{}", s.name);
    }
}

#[test]
fn a_dev_lint_of_a_selection_that_compiles_pulls_the_other_oses_cells() {
    assert!(pulls_cells(&DEV, false, true));
    assert!(pulls_cells(&[Check::Lint], false, true));
    assert_eq!(
        with(&DEV, Check::Cells),
        [DEV.as_slice(), &[Check::Cells]].concat()
    );
    assert_eq!(
        with(&[Check::Lint, Check::WasmSmoke], Check::Cells),
        [Check::Lint, Check::Cells, Check::WasmSmoke],
        "in CHECKS order"
    );
    assert!(
        !pulls_cells(&DEV, false, false),
        "an empty cone builds nothing"
    );
    assert!(!pulls_cells(&[Check::Test], false, true), "cells is a lint");
    assert!(
        !pulls_cells(&[Check::Lint, Check::Cells], false, true),
        "named: every cell, as `verify cells` checks"
    );
    assert!(
        !pulls_cells(&RELEASE, true, true),
        "release runs its set and nothing more"
    );
}

#[test]
fn a_selection_holding_a_web_module_runs_its_replay_with_its_tests() {
    use crate::changed::{Manifests, Member, Members, Replan, Selection, cone, select};
    const DEPENDENTS: [(&str, &str); 7] = [
        (
            "aterm-types",
            "aterm-grid aterm-core aterm-gui aterm-wasm aterm-gpu-web",
        ),
        (
            "aterm-grid",
            "aterm-core aterm-gui aterm-wasm aterm-gpu-web",
        ),
        ("aterm-core", "aterm-gui aterm-wasm aterm-gpu-web"),
        ("aterm-gui", ""),
        ("aterm-wasm", ""),
        ("aterm-gpu-web", ""),
        ("aterm-wasm-abi", ""),
    ];
    let members = Members::new(
        DEPENDENTS
            .iter()
            .map(|(n, _)| Member {
                name: n.to_string(),
                dir: format!("crates/{n}"),
                has_lib: true,
            })
            .collect(),
    );
    let manifests: Manifests = DEPENDENTS
        .iter()
        .map(|(n, _)| format!("crates/{n}"))
        .collect();
    let scope_after = |changed: &str| {
        let dependents = |name: &str| {
            let (_, users) = DEPENDENTS.iter().find(|(dep, _)| *dep == name)?;
            Some(crate::changed::Reach {
                links: users
                    .split_whitespace()
                    .chain([name])
                    .map(String::from)
                    .collect(),
                ..crate::changed::Reach::default()
            })
        };
        let sel = select(
            &[format!("crates/{changed}/src/lib.rs")],
            &std::collections::BTreeSet::new(),
            &manifests,
            &members,
            &|| Some(Vec::new()),
            &dependents,
            &|_| Replan::Nothing,
            &|_, _| false,
            &std::collections::BTreeMap::new,
        );
        assert!(
            matches!(sel, Selection::Narrowed { .. }),
            "{changed}: {sel:?}"
        );
        cone("origin/main", &sel).0
    };
    let dev_after = |changed: &str| {
        let scope = scope_after(changed);
        with_pulled(&DEV, false, |p| scope.includes_crate(p))
    };
    for engine in ["aterm-grid", "aterm-core", "aterm-types", "aterm-wasm"] {
        assert_eq!(
            dev_after(engine),
            [DEV.as_slice(), &[Check::WasmSmoke]].concat(),
            "a change to {engine}"
        );
    }
    assert_eq!(
        dev_after("aterm-gui"),
        DEV,
        "no web module builds on aterm-gui"
    );
    let abi = scope_after("aterm-wasm-abi");
    assert_eq!(
        pulled(&DEV, |p| abi.includes_crate(p)),
        [(Check::WasmSmoke, "aterm-wasm-abi")],
        "the modules' host boundary, which they build on wasm32 alone"
    );
    let core = scope_after("aterm-core");
    let holds = |p: &str| core.includes_crate(p);
    assert_eq!(pulled(&DEV, holds), [(Check::WasmSmoke, "aterm-wasm")]);
    assert_eq!(with_pulled(&[Check::Lint], false, holds), [Check::Lint]);
    assert_eq!(
        with_pulled(&[Check::Test, Check::WasmSmoke], false, holds),
        [Check::Test, Check::WasmSmoke],
        "named once"
    );
    let narrow = Scope::packages(&["aterm-core".to_string()]);
    assert_eq!(with_pulled(&DEV, false, |p| narrow.includes_crate(p)), DEV);
    assert_eq!(
        pulled(&DEV, |p| p == "aterm-gpu" || p == "aterm-gpu-web"),
        [(Check::WasmSmoke, "aterm-gpu-web")]
    );
    assert_eq!(
        with_pulled(&RELEASE, true, |_| true),
        RELEASE,
        "release runs its set: the stock pin's wasm32 std is no release prerequisite"
    );
}

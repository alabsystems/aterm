// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn lock_count_preserves_forks_and_includes_reviewed_git_components() {
    let root = aterm_tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[workspace.package]\nrepository = \"https://github.com/our-account/aterm\"\n",
    )
    .unwrap();
    let hash = "1234567890123456789012345678901234567890";
    let source = |repo: &str, rev: &str| format!("git+https://github.com/{repo}?rev={rev}#{hash}");
    let entry = |name: &str, source: Option<String>| crate::policy::LockEntry {
        name: name.to_owned(),
        version: "0.1.0".to_owned(),
        source,
    };
    assert!(super::first_party_lock_entry(
        root.path(),
        &entry(
            "trust-build-cpu",
            Some(source("our-account/trust.git", hash))
        ),
        &[]
    ));
    for foreign in [
        entry("ring", Some(source("our-account/ring.git", hash))),
        entry(
            "trust-build-cpu",
            Some(source("other-account/trust.git", hash)),
        ),
        entry(
            "trust-build-cpu",
            Some(source("our-account/trust.git", "main")),
        ),
        entry(
            "trust-build-cpu",
            Some("registry+https://github.com/rust-lang/crates.io-index".into()),
        ),
    ] {
        assert!(!super::first_party_lock_entry(root.path(), &foreign, &[]));
    }
    assert!(super::first_party_lock_entry(
        root.path(),
        &entry("ours", None),
        &[]
    ));
    assert!(!super::first_party_lock_entry(
        root.path(),
        &entry("vendored", None),
        &["vendored"]
    ));
}
use super::*;

fn row(scope: &str, metric: &str, ceiling: u64) -> Row {
    Row {
        scope: scope.to_string(),
        metric: metric.to_string(),
        ceiling,
        regress_reason: None,
    }
}

fn live_with(scope: &str, metric: &str, value: u64) -> Live {
    let mut l = Live::default();
    l.scopes.push(scope.to_string());
    l.set(scope, metric, value);
    l
}

const REASON: &str = "wgpu 29 pulled naga's new backend in for the WebGPU path";

#[test]
fn a_row_round_trips_through_render() {
    let text = format!(
        "shipped.aarch64-apple-darwin\tthird_party_packages\t28\npatch\tentries\t7\t{REASON}\n"
    );
    let rows = parse(&text).unwrap();
    assert_eq!(
        rows[0],
        row("shipped.aarch64-apple-darwin", "third_party_packages", 28)
    );
    assert_eq!(rows[1].regress_reason.as_deref(), Some(REASON));
    assert_eq!(render(&rows), text);
}

#[test]
fn a_malformed_line_is_refused_by_line_number() {
    for (name, text, line) in [
        (
            "two columns",
            "lock\tthird_party_packages\t556\npatch\tentries\n",
            2,
        ),
        ("a word for a ceiling", "patch\tentries\tsix\n", 1),
        ("spaces for tabs", "patch entries 6\n", 1),
        ("a comment", "patch\tentries\t6\n# and here is why\n", 2),
        (
            "a duplicate row",
            "patch\tentries\t6\npatch\tentries\t7\n",
            2,
        ),
        ("an empty reason", "patch\tentries\t6\t\n", 1),
    ] {
        let e = parse(text).expect_err(name);
        assert!(
            e.starts_with(&format!("{BUDGET_PATH}:{line}:")),
            "{name}: {e}"
        );
    }
}

#[test]
fn an_unknown_metric_or_scope_is_refused() {
    let live = live_with("lock", "packages", 624);
    assert!(validate_metric(&row("lock", "lines_of_code", 1), &live).is_err());
    assert!(validate_metric(&row("shipped.sparc-unknown-none", "packages", 1), &live).is_err());
}

#[test]
fn update_lowers_a_ceiling_and_only_update_writes() {
    let live = live_with("patch", "entries", 4);
    let p = plan(&[row("patch", "entries", 6)], &live, true, None);
    assert!(p.ok && p.changed);
    assert_eq!(p.rows[0].ceiling, 4);
    let p = plan(&[row("patch", "entries", 6)], &live, false, None);
    assert!(p.ok && !p.changed);
    assert_eq!(p.rows[0].ceiling, 6);
}

#[test]
fn a_raise_without_a_reason_is_red_and_writes_nothing() {
    let live = live_with("patch", "entries", 9);
    let p = plan(&[row("patch", "entries", 6)], &live, true, None);
    assert!(!p.ok);
    assert!(!p.changed, "a raise must never be written without a reason");
    assert_eq!(p.rows[0].ceiling, 6);
}

#[test]
fn a_reason_is_accepted_and_recorded() {
    let reason = validate_reason(true, Some(REASON)).unwrap();
    assert_eq!(reason.as_deref(), Some(REASON));
    let live = live_with("patch", "entries", 9);
    let p = plan(
        &[row("patch", "entries", 6)],
        &live,
        true,
        reason.as_deref(),
    );
    assert!(p.ok && p.changed);
    assert_eq!(p.rows[0].ceiling, 9);
    assert_eq!(p.raised, 1);
    assert!(render(&p.rows).contains(REASON));
}

#[test]
fn a_tab_in_the_reason_is_refused_because_the_file_is_tsv() {
    let r = format!("{REASON}\tand more");
    assert!(validate_reason(true, Some(&r)).is_err());
}

#[test]
fn an_unmeasured_scope_is_never_a_pass() {
    let mut live = Live::default();
    live.unavailable.insert(
        "shipped.x86_64-pc-windows-msvc".to_string(),
        "no such target".to_string(),
    );
    let p = plan(
        &[row(
            "shipped.x86_64-pc-windows-msvc",
            "third_party_packages",
            10,
        )],
        &live,
        false,
        None,
    );
    assert!(!p.ok);
}

#[test]
fn a_missing_armed_row_is_red_and_an_unarmed_one_advisory() {
    let scope = "shipped.aarch64-unknown-linux-gnu";
    let mut live = live_with(scope, "build_scripts", 25);
    live.set(scope, "packages", 400);
    live.set(scope, "proc_macros", 7);
    let rows = [row(scope, "build_scripts", 25)];
    for update in [false, true] {
        let p = plan(&rows, &live, update, None);
        assert!(!p.ok, "a deleted row must not leave the run green");
        assert_eq!(p.problems.len(), 1, "{:?}", p.problems);
        assert!(
            p.problems[0].contains(&format!("{scope}\tproc_macros\t7")),
            "{}",
            p.problems[0]
        );
        assert!(!p.changed, "--update never adds a row");
    }
    assert_eq!(
        unratcheted(&rows, &live),
        [(scope.to_string(), "packages".to_string(), 400)]
    );
}

#[test]
fn an_unpacked_third_party_package_marks_the_scope_unavailable() {
    use crate::model::{Graph, PkgFacts, PkgId};
    let scope = "shipped.aarch64-apple-darwin";
    let root_id = PkgId::new("aterm", "0.1.0");
    let dep_id = PkgId::new("unpacked-macro", "0.3.1");
    let mut graph = Graph {
        root: root_id.clone(),
        ..Graph::default()
    };
    graph.nodes.insert(root_id.clone());
    graph.nodes.insert(dep_id.clone());
    graph
        .edges
        .entry(root_id.clone())
        .or_default()
        .insert(dep_id.clone());
    let mut facts = BTreeMap::new();
    facts.insert(
        root_id,
        PkgFacts {
            is_third_party: false,
            root_dir: Some(std::path::PathBuf::from("/repo/crates/aterm")),
            ..PkgFacts::default()
        },
    );
    facts.insert(
        dep_id,
        PkgFacts {
            is_third_party: true,
            root_dir: None,
            ..PkgFacts::default()
        },
    );
    let survey = CellSurvey {
        cell: Cell {
            name: "mac-arm".to_string(),
            triple: "aarch64-apple-darwin".to_string(),
            package: "aterm".to_string(),
        },
        graph,
        facts,
    };

    let mut live = Live::default();
    live.scopes.push(scope.to_string());
    record_surveyed_cell(&mut live, scope, Ok(survey));
    assert!(live.get(scope, "proc_macros").is_none());
    assert!(
        live.unavailable[scope].contains("unpacked-macro@0.3.1"),
        "{:?}",
        live.unavailable
    );

    let rows = [row(scope, "proc_macros", 5)];
    for update in [false, true] {
        let p = plan(&rows, &live, update, None);
        assert!(!p.ok, "unmeasured third-party source must fail closed");
        assert!(
            !p.changed,
            "--update must not lower ceilings on unmeasured sources"
        );
        assert!(
            p.problems[0].contains("unpacked-macro@0.3.1"),
            "{:?}",
            p.problems
        );
    }
}

#[test]
fn write_body_replaces_budget_without_leaving_staging_dirs() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write_body(root, "first\n").expect("first write");
    write_body(root, "second\n").expect("second write");
    assert_eq!(
        std::fs::read_to_string(root.join(BUDGET_PATH)).expect("read"),
        "second\n"
    );
    let entries: Vec<_> = std::fs::read_dir(root.join("tools"))
        .expect("tools dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["forge-budget.tsv".to_string()]);
}

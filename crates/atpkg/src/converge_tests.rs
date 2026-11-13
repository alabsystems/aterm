// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_finding_reports_as_its_item_and_only_an_essential_change_fails() {
    let rows = report(vec![
        Finding::ok(
            PATH,
            Class::Person,
            "the managed bin/ on PATH",
            "managed bin/ is on PATH",
        ),
        Finding::change(
            of(SHIMS, "cargo"),
            Class::Relay,
            "the bin/ shim cargo",
            "broken bin shim /p/bin/cargo — its build is gone",
            "forwarding to a build in the store",
            Remedy::Repair,
        )
        .essential(),
        Finding::change(
            of(STUBS, "cargo"),
            Class::Relay,
            "the reroute stub cargo",
            "reroute stub cargo missing; the next aterm session lays it",
            "laid",
            Remedy::Repair,
        ),
        Finding::unread(
            DISK,
            Class::Person,
            "disk headroom",
            "could not query free space",
        )
        .essential(),
        Finding::note(RUSTUP, Class::Person, "rustup", "no rustup on PATH"),
    ]);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            PATH,
            "base.pkg.shims@cargo",
            "base.pkg.stubs@cargo",
            DISK,
            RUSTUP
        ]
    );
    let failing: Vec<&str> = rows
        .iter()
        .filter(|r| r.fails_surface())
        .map(|r| r.id.as_str())
        .collect();
    assert_eq!(
        failing,
        ["base.pkg.shims@cargo"],
        "the advisory and the unread row do not fail"
    );
    assert!(!passes(&rows));
    assert_eq!(
        rows[1].from.as_deref(),
        Some("broken bin shim /p/bin/cargo")
    );
    assert_eq!(
        rows[1].explain.as_ref().map(|e| &e.remedy),
        Some(&Remedy::Repair)
    );
    assert_eq!(rows[0].to.as_deref(), Some("managed bin/ is on PATH"));
    assert_eq!(rows[0].family, "pkg");
    assert!(rows.iter().all(|r| r.layer == Some(Layer::Base)));
    assert!(passes(&rows[2..]));
}

#[test]
fn a_subject_found_twice_keeps_both_rows_under_distinct_ids() {
    let rows = report(vec![
        Finding::note(CONFIG, Class::Person, "the [packages] table", "a"),
        Finding::note(CONFIG, Class::Person, "the [packages] table", "b"),
        Finding::note(of(STUBS, "x"), Class::Relay, "the reroute stub x", "c"),
        Finding::note(of(STUBS, "x"), Class::Relay, "the reroute stub x", "d"),
    ]);
    let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            CONFIG,
            "base.pkg.config@2",
            "base.pkg.stubs@x",
            "base.pkg.stubs@x.2"
        ]
    );
}

#[test]
fn an_id_names_its_item_and_its_subjects_only() {
    assert!(is("base.pkg.store", STORE));
    assert!(is("base.pkg.store@ay", STORE));
    assert!(!is("base.pkg.store.shim@ay", STORE));
    assert!(!is("base.pkg.stores", STORE));
    assert_eq!(family_of("base.shell.rc@.zshrc"), "shell");
    assert_eq!(family_of(CLT), "developer");
    assert_eq!(
        family_of("base.agents.claude-defaults@effortLevel"),
        "agents"
    );
}

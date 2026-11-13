// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(local: u64, sid: &str, name: &str, from: Option<&str>) -> Row {
    Row {
        local,
        sid: sid.to_string(),
        name: name.to_string(),
        from: from.map(str::to_string),
        task: None,
    }
}

#[test]
fn workers_name_their_manager_and_a_manager_counts_them() {
    let rows = [
        row(1, "s-aa", "lab-sweep", None),
        row(2, "s-bb", "bench", Some("s-aa")),
        row(3, "s-cc", "fmt", Some("s-aa")),
        row(4, "s-dd", "lint", Some("s-aa")),
        row(5, "s-ee", "remote", Some("s-1e91ffff")),
        row(6, "s-ff", "self", Some("s-ff")),
        row(7, "s-00", "ctl", None),
    ];
    let teams = table(&rows);
    let manager = &teams[&1];
    assert_eq!((manager.workers, manager.manager.as_ref()), (3, None));
    assert_eq!(&*manager.workers_mark, "\u{21c9}3");
    assert_eq!(
        manager.members,
        [2, 3, 4],
        "its workers, in the order they were made"
    );
    for worker in [2, 3, 4] {
        let boss = teams[&worker].manager.as_ref().expect("a worker");
        assert_eq!((boss.local, &*boss.label), (Some(1), "lab-sweep"));
        assert_eq!(teams[&worker].workers, 0);
    }
    let remote = teams[&5]
        .manager
        .as_ref()
        .expect("a worker of a sid not here");
    assert_eq!((remote.local, &*remote.label), (None, "s-1e91"));
    assert!(!teams.contains_key(&6), "a row naming itself is no team");
    assert!(!teams.contains_key(&7), "from=ctl names no chip");
    assert_eq!(
        teams[&2].strip(),
        crate::tab_bar::TabTeam {
            worker: true,
            workers: 0
        }
    );
}

#[test]
fn a_managers_workers_are_listed_in_the_order_they_were_made() {
    let rows = [
        row(4, "s-dd", "lint", Some("s-aa")),
        row(1, "s-aa", "lab-sweep", None),
        row(3, "s-cc", "fmt", Some("s-aa")),
        row(2, "s-bb", "bench", Some("s-aa")),
    ];
    assert_eq!(table(&rows)[&1].members, [2, 3, 4]);
}

#[test]
fn a_managers_name_is_sanitized() {
    let rows = [
        row(
            1,
            "s-aa",
            "evil\u{202e}\u{1b}[31m\u{25c6}role-that-is-far-too-long",
            None,
        ),
        row(2, "s-bb", "w", Some("s-aa")),
    ];
    let label = table(&rows)[&2].manager.clone().expect("a worker").label;
    assert!(!label.contains('\u{202e}') && !label.contains('\u{1b}'));
    assert!(!label.contains('\u{25c6}'));
    assert!(label.chars().count() <= LABEL_CELLS, "{label:?}");
}

#[test]
fn the_term_moves_with_what_the_chip_says() {
    let rows = |name: &str| {
        table(&[
            row(1, "s-aa", name, None),
            row(2, "s-bb", "w", Some("s-aa")),
        ])
    };
    assert_eq!(rows("a")[&2].term(), rows("a")[&2].term());
    assert_ne!(rows("a")[&2].term(), rows("b")[&2].term());
    assert_ne!(Team::default().term(), 0);
}

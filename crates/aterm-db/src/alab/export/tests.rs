// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exports' seal, drift, roster hints and compare-before-write plan, pure; the vault writer
//! end to end is `command::tests::the_export_publishes_once_and_check_reports_its_drift`.

use super::*;
use crate::typed::owned::OwnedRow;
use aterm_secrets::swarm_roster::SwarmMember;

/// A sealed export holds its seal; a file the lab moved past is `stale`, one edited by hand
/// `edited`, one never generated `unsealed`, none `missing`, the same bytes `same`.
/// NEGATIVE CONTROL: with `drift` reading every differing file as `Stale` (no seal check), the
/// hand edit reads `stale` (`left: Stale right: Edited`).
#[test]
fn drift_tells_a_moved_lab_from_a_hand_edit() {
    let first = sealed("[[account]]\nlabel = \"a\"\n").unwrap();
    assert!(matches!(seal::check(&first), seal::Seal::Holds { line: 1 }));
    assert!(first.lines().next().unwrap().contains(GENERATOR));
    let moved = sealed("[[account]]\nlabel = \"b\"\n").unwrap();
    assert_eq!(drift(Some(&first), &first), Drift::Same);
    assert_eq!(drift(Some(&first), &moved), Drift::Stale);
    let edited = first.replace("\"a\"", "\"z\"");
    assert_eq!(drift(Some(&edited), &moved), Drift::Edited);
    assert_eq!(drift(Some("[[account]]\n"), &moved), Drift::Unsealed);
    assert_eq!(drift(None, &moved), Drift::Missing);
}

fn member(label: &str, node: Option<&str>, hint: Option<&str>) -> SwarmMember {
    SwarmMember {
        recipient: format!("age1{label}"),
        vault_name: label.to_owned(),
        label: label.to_owned(),
        aliases: Vec::new(),
        hint: None,
        hw_hint: hint.map(str::to_owned),
        admitted_ms: 0,
        claimed_label: None,
        claimed_at_ms: None,
        node_id: node.map(str::to_owned),
        node_key: None,
    }
}

fn facts(rows: &[(&str, Value)]) -> OwnedRead {
    let mut read = OwnedRead::default();
    for (n, (uuid, value)) in rows.iter().enumerate() {
        read.rows.insert(
            (*uuid).to_owned(),
            OwnedRow {
                machine: [n as u8; 16],
                actor: [n as u8; 48],
                value: value.clone(),
                rank: (0, 1, 1),
                operation: [0; 32],
            },
        );
    }
    read
}

/// The roster's `hw_hint` comes from the facts rows (§4.7) by the member's fleet node alone: a
/// row stating only a label binds nothing (a retired Mac's row may state a new Mac's label), nor
/// does a row merged into another machine; a node two hints name, or none, keeps what it has, and
/// one already holding its hint is no change.
/// NEGATIVE CONTROL: with `hints` taking the first hint found rather than requiring one, m5's
/// clone pair writes a hint (`left: ["m3", "m5"] right: ["m3"]`); with `hints` binding a member
/// with no node id by its label, m7 takes the retired row's hint (`left: ["m3", "m7"]`).
#[test]
fn the_roster_takes_the_one_hint_its_facts_rows_state() {
    let a = "aaaaaaaaaaaaaaaa";
    let b = "bbbbbbbbbbbbbbbb";
    let node = |n: u8| format!("{n:02x}").repeat(16);
    let mut roster = SwarmRoster {
        members: vec![
            member("m3", Some(&node(3)), None),
            member("m4", Some(&node(4)), Some(a)),
            member("m5", Some(&node(5)), None),
            member("m6", Some(&node(6)), Some(b)),
            member("m7", None, None),
            member("m8", Some(&node(8)), None),
        ],
        ..SwarmRoster::default()
    };
    let read = facts(&[
        (
            "u1",
            json!({"n": 1, "label": "m30", "node": node(3), "hw_hint": a}),
        ),
        (
            "u2",
            json!({"n": 1, "label": "m4", "node": node(4), "hw_hint": a}),
        ),
        (
            "u3",
            json!({"n": 1, "label": "m5", "node": node(5), "hw_hint": a}),
        ),
        (
            "u4",
            json!({"n": 1, "label": "m5", "node": node(5), "hw_hint": b}),
        ),
        ("u5", json!({"n": 1, "label": "m7", "hw_hint": a})),
        (
            "u6",
            json!({"n": 1, "label": "m6", "node": node(6), "hw_hint": "not-hex"}),
        ),
        (
            "u7",
            json!({"n": 1, "label": "m8", "node": node(8), "hw_hint": a,
                "merged_into": "mc-0000000000000001"}),
        ),
    ]);
    let changed = hints(&mut roster, &read);
    assert_eq!(
        changed,
        ["m3"],
        "by node; m4 already holds its; m5 is ambiguous; m7 has no node; m8's row is merged"
    );
    assert_eq!(roster.members[0].hw_hint.as_deref(), Some(a));
    assert_eq!(roster.members[2].hw_hint, None);
    assert_eq!(roster.members[3].hw_hint.as_deref(), Some(b), "kept");
    assert_eq!(roster.members[4].hw_hint, None, "a label is no identity");
    assert_eq!(roster.members[5].hw_hint, None);
    assert!(
        hints(&mut roster, &read).is_empty(),
        "a second pass changes nothing"
    );
}

/// Only the held files whose bytes differ are planned, the roster only where a hint changed: an
/// unchanged lab plans nothing to publish, and a file the vault does not hold is reported missing
/// and never created (an older Mac would switch its source to it).
/// NEGATIVE CONTROL: with `plan` keeping every rendered file, the unchanged one is planned
/// (`left: 3 right: 1`); with `plan` writing a missing file, `three.toml` is planned
/// (`left: 2 right: 1`).
#[test]
fn an_export_writes_only_what_changed() {
    let a = sealed("a\n").unwrap();
    let b = sealed("b\n").unwrap();
    let rendered = vec![
        ("one.toml".to_owned(), a.clone()),
        ("two.toml".to_owned(), b.clone()),
        ("three.toml".to_owned(), b.clone()),
    ];
    let current: BTreeMap<String, String> = [
        ("one.toml".to_owned(), a.clone()),
        ("two.toml".to_owned(), a.clone()),
    ]
    .into();
    let planned = plan(&rendered, &current, None, &OwnedRead::default());
    assert_eq!(planned.files, [("two.toml".to_owned(), b.clone())]);
    assert_eq!(
        planned.drift,
        [
            ("one.toml".to_owned(), Drift::Same),
            ("two.toml".to_owned(), Drift::Stale),
            ("three.toml".to_owned(), Drift::Missing)
        ]
    );
    assert!(planned.roster.is_none());
    let all: BTreeMap<String, String> = rendered.iter().cloned().collect();
    let again = plan(
        &rendered,
        &all,
        Some(&SwarmRoster::default()),
        &OwnedRead::default(),
    );
    assert!(again.files.is_empty() && again.roster.is_none());
}

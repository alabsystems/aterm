// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView over hand-built rows; every address is synthetic (`example.test`).

use super::*;
use crate::{
    database::Change,
    typed::{
        grid::tests::{Book, put},
        keys,
        owned::OwnedRow,
    },
};
use aterm_json::json;

const ME: &str = "0123456789abcdef0123456789abcdef";

fn facts(mac: &str, merged: Option<&str>) -> OwnedRead {
    let value = match merged {
        Some(merged) => json!({"n": 1, "mac": mac, "merged_into": merged}),
        None => json!({"n": 1, "mac": mac}),
    };
    let mut read = OwnedRead::default();
    read.rows.insert(
        ME.to_owned(),
        OwnedRow {
            machine: [1; 16],
            actor: [1; 48],
            value,
            rank: (0, 1, 1),
            operation: [0; 32],
        },
    );
    read
}

fn machine(label: &str) -> (String, Vec<Change>) {
    let pk = keys::machine_pk(label);
    let changes = vec![
        put(&MACHINE, &pk, "label", json!(label)),
        put(&MACHINE, &pk, "status", json!("active")),
        put(&MACHINE, &pk, "row", json!("live")),
    ];
    (pk, changes)
}

fn account(local: &str, realm: &str) -> (String, Vec<Change>) {
    let email = format!("{local}@example.test");
    let address = keys::address_pk(&email).unwrap();
    let pk = keys::account_pk(realm, &email).unwrap();
    let changes = vec![
        put(&ADDRESS, &address, "addr", json!(email)),
        put(&ADDRESS, &address, "row", json!("live")),
        put(&ACCOUNT, &pk, "realm", json!(realm)),
        put(&ACCOUNT, &pk, "login", json!(address)),
        put(&ACCOUNT, &pk, "status", json!("active")),
        put(&ACCOUNT, &pk, "row", json!("live")),
    ];
    (pk, changes)
}

fn prefer(machine: &str, account: &str, rank: Option<u64>) -> Vec<Change> {
    let pk = keys::preference_pk(machine, account).unwrap();
    let mut changes = vec![
        put(&PREFERENCE, &pk, "machine", json!(machine)),
        put(&PREFERENCE, &pk, "account", json!(account)),
        put(&PREFERENCE, &pk, "why", json!("owner")),
        put(&PREFERENCE, &pk, "row", json!("live")),
    ];
    if let Some(rank) = rank {
        changes.push(put(&PREFERENCE, &pk, "rank", json!(rank)));
    }
    changes
}

fn cells(changes: Vec<Change>) -> Cells {
    let mut book = Book::new();
    book.write(1, &[], changes);
    book.cells()
}

/// This Mac is the machine its own facts row binds, its `merged_into` when it names one; its
/// preferences come in rank order with an unranked one last; the Macs preferring an account sort
/// by rank, then by label number (`m3` before `m22`); an ended preference, or one on a retired
/// machine, is no preference; an account reads its default label, which names no machine.
/// NEGATIVE CONTROL: with `read` taking `mac` before `merged_into`, `me` is m22's row, not m3's:
/// `` left: Some("mc-…") right: Some("mc-…") ``; with the preference sort's `rank.is_none()` key
/// removed, the unranked account comes first.
#[test]
fn a_mac_reads_its_preferences_from_its_facts_binding() {
    let (m22, mut all) = machine("m22");
    let (m3, more) = machine("m3");
    all.extend(more);
    let (gone, more) = machine("m9");
    all.extend(more);
    all.push(put(&MACHINE, &gone, "row", json!("retired")));
    let (ann, more) = account("ann", "claude");
    all.extend(more);
    let (bob, more) = account("bob", "claude");
    all.extend(more);
    let (cy, more) = account("cy", "kimi:global");
    all.extend(more);
    all.extend(prefer(&m3, &ann, None));
    all.extend(prefer(&m3, &bob, Some(2)));
    all.extend(prefer(&m22, &ann, Some(1)));
    all.extend(prefer(&gone, &ann, Some(1)));
    all.extend(prefer(&m3, &cy, Some(1)));
    let ended = keys::preference_pk(&m22, &bob).unwrap();
    all.extend(prefer(&m22, &bob, Some(1)));
    all.push(put(&PREFERENCE, &ended, "row", json!("ended")));
    let cells = cells(all);

    let view = LabView::read(
        &cells,
        &facts(&m22, Some(&m3)),
        &OwnedRead::default(),
        Some(ME),
    );
    assert_eq!(view.me.as_deref(), Some(m3.as_str()));
    assert_eq!(view.my_label(), Some("m3"));
    let mine: Vec<&str> = view.preferred_by(&m3).map(|p| p.account.as_str()).collect();
    assert_eq!(mine, [cy.as_str(), bob.as_str(), ann.as_str()]);
    let preferring: Vec<&str> = view
        .preferring(&ann)
        .iter()
        .map(|p| p.machine.as_str())
        .collect();
    assert_eq!(preferring, [m22.as_str(), m3.as_str()], "m9 is retired");
    assert!(
        view.preferring(&bob).iter().all(|p| p.machine == m3),
        "ended"
    );
    let label = view.accounts[&ann].label.clone().unwrap();
    assert_eq!(label, keys::default_account_label("claude", &ann).unwrap());
    assert!(!label.contains("m3") && !label.contains("ann"));

    let unbound = LabView::read(
        &cells,
        &OwnedRead::default(),
        &OwnedRead::default(),
        Some(ME),
    );
    assert_eq!(unbound.me, None, "no facts row: no machine is this Mac");
    let retired = LabView::read(&cells, &facts(&gone, None), &OwnedRead::default(), Some(ME));
    assert_eq!(retired.me, None, "a retired machine is no Mac's binding");
}

/// An alias's auth codes arrive where the address it is linked with receives them: the chain is
/// read nearest first and stops at a retired link or a cycle.
/// NEGATIVE CONTROL: without the `live()` filter in `alias_targets`, the retired link is
/// followed: `` left: ["hub@…", "gone@…"] right: ["hub@…"] ``.
#[test]
fn an_alias_names_the_addresses_its_codes_arrive_at() {
    let pk = |local: &str| keys::address_pk(&format!("{local}@example.test")).unwrap();
    let addr = |local: &str| format!("{local}@example.test");
    let mut all = Vec::new();
    for local in ["ann", "hub", "gone", "la", "lb"] {
        all.push(put(&ADDRESS, &pk(local), "addr", json!(addr(local))));
    }
    all.push(put(&ADDRESS, &pk("ann"), "alias_of", json!(pk("hub"))));
    all.push(put(&ADDRESS, &pk("hub"), "alias_of", json!(pk("gone"))));
    all.push(put(&ADDRESS, &pk("gone"), "row", json!("retired")));
    all.push(put(&ADDRESS, &pk("la"), "alias_of", json!(pk("lb"))));
    all.push(put(&ADDRESS, &pk("lb"), "alias_of", json!(pk("la"))));
    let view = LabView::read(
        &cells(all),
        &OwnedRead::default(),
        &OwnedRead::default(),
        None,
    );
    assert_eq!(view.alias_targets(&addr("ann")), [addr("hub")]);
    assert_eq!(view.alias_targets(&addr("la")), [addr("lb")]);
    assert!(view.alias_targets(&addr("nobody")).is_empty());
}

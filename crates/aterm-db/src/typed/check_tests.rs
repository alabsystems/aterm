// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rules every write this Mac authors passes ([`super::refused`]), on the derived paths
//! (`att::Handle::commit_derived`, which the importer, `adopt` and `prefer` write through) as on
//! an owner's, against real Commons labs: isolated scratch homes, no live vault, fleet or
//! Commons. Every key is synthetic.

use super::*;
use crate::{
    alab::registry::ACCOUNT,
    typed::{Derive, Writer, Writers, cell::CellValue},
};
use aterm_json::json;
// The Commons labs, and so the write paths, are unix-only (`att`, `table`).
#[cfg(unix)]
use crate::{
    alab::{
        Registry,
        registry::{ACCOUNT_OBS, MACHINE},
    },
    att::{Handle, REGISTRY},
    table::tests::{Lab, repair_commons_first},
    typed::grid::tests::{MC, machine, put},
};
#[cfg(unix)]
use aterm_secrets::commons::roster::Role;

/// A machine row these tests make, and one no row of theirs holds.
#[cfg(unix)]
const OTHER: &str = "mc-0000000000000008";
#[cfg(unix)]
const MISSING: &str = "mc-00000000000000ff";
#[cfg(unix)]
const MB1: &str = "mb-0000000000000001";
#[cfg(unix)]
const MB2: &str = "mb-0000000000000002";
#[cfg(unix)]
const MB3: &str = "mb-0000000000000003";
#[cfg(unix)]
const MB4: &str = "mb-0000000000000004";

#[cfg(unix)]
fn mailbox_row(mailbox: &Collection, pk: &str, label: &str) -> Vec<Change> {
    vec![
        put(mailbox, pk, "label", json!(label)),
        put(mailbox, pk, "service", json!("proton")),
        put(mailbox, pk, "code_path", json!("manual")),
        put(mailbox, pk, "row", json!("live")),
    ]
}

#[cfg(unix)]
fn repair_registry(lab: &Lab) {
    let catalog = crate::catalog::scope(lab.bootstrap(0).trust()).digest();
    repair_commons_first(lab, 1, catalog);
    let table = REGISTRY.descriptor(lab.bootstrap(0).trust()).unwrap().scope;
    repair_commons_first(lab, 1, table.digest());
}

/// A plan written through the derived path (the importer's, `adopt`'s, `prefer`'s) passes the
/// rules every table keeps, as an owner's grid save does: a write that makes two live mailboxes
/// share their unique `label`, one whose `Ref` cell or owned row names a row the writer's view
/// does not hold, and one that retires a machine a live machine names are each refused, name
/// no value, and append nothing. The same write freed of what it breaks is written: a label a
/// row it retires gave up, a row it makes or holds, a retire whose referrer ends with it, a
/// rewrite of this Mac's owned row that leaves a reference already dangling as it is. Two
/// rows that two Macs each wrote alone and a sync brought together are never refused: `check`
/// reports their shared label, and a later write of either that leaves the label alone is
/// written.
/// NEGATIVE CONTROL: with `Compiled::rules` judging only `Author::Owner` writes (the derived
/// path skipping `typed::check::refused`, as it did before), the first derived write is
/// written: `` a write that shares a unique label is refused: Applied { records: 1, .. } ``.
/// With `unique_clash` refusing a pair that already shared its set (`shared_before` ignored),
/// the last write is refused: `` a write that leaves a clash sync made as it is is written ``.
/// With `refused` not calling `retired_referenced`, the retire is written: `` retiring a
/// referenced row is refused: Applied { records: 1, .. } ``. With `missing_row` judging an
/// owned row's column even where it states what this Mac's row already holds, the rewrite is
/// refused: `` a rewrite of an owned row that leaves its dangling reference is written ``.
#[cfg(unix)]
#[test]
fn a_derived_write_that_breaks_a_unique_set_or_a_reference_is_refused() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let mailbox = handle.compiled().collection("mailbox").unwrap();
    let mut other = machine(OTHER);
    other[0] = put(&MACHINE, OTHER, "label", json!("m8"));
    owner
        .write(vec![
            machine(MC),
            other,
            mailbox_row(mailbox, MB1, "proton-1"),
            mailbox_row(mailbox, MB2, "proton-2"),
        ])
        .unwrap();
    let derived = |handle: &Handle, rows: Vec<Vec<Change>>| {
        handle
            .commit_derived(false, |_| Ok(((), rows)))
            .map(|((), applied)| applied)
    };
    let refused = |rows: Vec<Vec<Change>>, said: &str| {
        let written = handle.history().unwrap().len();
        let refusal = derived(&handle, rows).expect_err(said).to_string();
        assert_eq!(handle.history().unwrap().len(), written, "nothing written");
        refusal
    };

    // Unique: a second live row holding a label is refused, naming the other row and the
    // column, never the label.
    let refusal = refused(
        vec![vec![put(mailbox, MB2, "label", json!("proton-1"))]],
        "a write that shares a unique label is refused",
    );
    assert!(
        refusal.contains(&format!(
            "mailbox.label: row {MB2}: live row {MB1} holds the same label; only one live row may"
        )),
        "{refusal}"
    );
    assert!(!refusal.contains("proton-1"), "a refusal quotes no value");
    // A row born holding it is refused alike.
    refused(
        vec![mailbox_row(mailbox, MB3, "proton-2")],
        "a birth that shares a unique label is refused",
    );
    // A label the same write retires its holder of is free.
    derived(
        &handle,
        vec![
            vec![put(mailbox, MB1, "row", json!("retired"))],
            vec![put(mailbox, MB2, "label", json!("proton-1"))],
        ],
    )
    .unwrap();
    // Making the retired holder live again would share it.
    let refusal = refused(
        vec![vec![put(mailbox, MB1, "row", json!("live"))]],
        "a revival that shares a unique label is refused",
    );
    assert!(
        refusal.contains(&format!("live row {MB2} holds")),
        "{refusal}"
    );

    // A reference in a cell to a row the view does not hold.
    let refusal = refused(
        vec![vec![put(&MACHINE, MC, "merged_into", json!(MISSING))]],
        "a cell naming a missing row is refused",
    );
    assert!(
        refusal.contains(&format!(
            "machine.merged_into: row {MC}: no such machine row"
        )),
        "{refusal}"
    );
    // A reference inside an owned row.
    let account = "ac-00000000000000ff";
    let obs = Change {
        collection: ACCOUNT_OBS.name.to_owned(),
        key: format!("{account}.{}", "0".repeat(32)),
        value: Some(json!({"n": 1, "account": account, "cli": "claude", "tabs": 1})),
    };
    let refusal = refused(
        vec![vec![obs]],
        "an owned row naming a missing row is refused",
    );
    assert!(
        refusal.contains("account-obs.account: row ") && refusal.contains("no such account row"),
        "{refusal}"
    );
    // This Mac's own owned row already naming a missing row (an unchecked writer, standing for
    // what a concurrent write left): a rewrite that leaves the reference as it is is written.
    let uuid = {
        let bootstrap = lab.bootstrap(0);
        let session = crate::table::Session::open(&bootstrap).unwrap();
        let actor = session.actor();
        let members = session.members().unwrap();
        let binding = members.iter().find(|b| b.actor() == actor).unwrap();
        crate::typed::keys::uuid32(binding.machine())
    };
    let mine = |n: u64| Change {
        collection: ACCOUNT_OBS.name.to_owned(),
        key: format!("{account}.{uuid}"),
        value: Some(json!({"n": n, "account": account, "cli": "claude", "tabs": 1})),
    };
    owner.write(vec![vec![mine(1)]]).unwrap();
    assert!(
        handle.view().unwrap().owned[ACCOUNT_OBS.name]
            .rows
            .contains_key(&format!("{account}.{uuid}")),
        "the view holds this Mac's dangling owned row"
    );
    derived(&handle, vec![vec![mine(2)]])
        .expect("a rewrite of an owned row that leaves its dangling reference is written");

    // Retiring a row a live row names is refused; ending the referrer with it is written.
    derived(
        &handle,
        vec![vec![put(&MACHINE, MC, "merged_into", json!(OTHER))]],
    )
    .unwrap();
    let refusal = refused(
        vec![vec![put(&MACHINE, OTHER, "row", json!("retired"))]],
        "retiring a referenced row is refused",
    );
    assert!(
        refusal.contains(&format!(
            "machine.row: row {OTHER}: live machine row {MC} names this row"
        )),
        "{refusal}"
    );
    derived(
        &handle,
        vec![
            vec![put(&MACHINE, MC, "row", json!("merged"))],
            vec![put(&MACHINE, OTHER, "row", json!("retired"))],
        ],
    )
    .unwrap();

    // Two Macs each write a row alone; sync brings them together and refuses neither.
    repair_registry(&lab);
    let peer = Handle::at(lab.paths[1].clone(), &REGISTRY);
    derived(&handle, vec![mailbox_row(mailbox, MB3, "proton-3")]).unwrap();
    derived(&peer, vec![mailbox_row(mailbox, MB4, "proton-3")]).unwrap();
    let table = REGISTRY.descriptor(lab.bootstrap(0).trust()).unwrap().scope;
    repair_commons_first(&lab, 1, table.digest());
    let view = handle.view().unwrap();
    let projected = crate::typed::cell::project(mailbox, &view.cells);
    let found = findings(&[mailbox], &BTreeMap::from([(mailbox.name, &projected)]));
    assert!(
        found.contains(&Finding::Unique {
            collection: mailbox.name.to_owned(),
            columns: vec!["label".to_owned()],
            pks: vec![MB3.to_owned(), MB4.to_owned()],
        }),
        "{found:?}"
    );
    derived(
        &handle,
        vec![vec![put(mailbox, MB3, "note", json!("shelf"))]],
    )
    .expect("a write that leaves a clash sync made as it is is written");
}

/// The owner's word making a live row name a row that is not live is refused, as retiring a row a
/// live row names is ([`retired_referenced`]), from the other side: a revival of a preference
/// whose machine the owner retired (a grid edit of its `row` cell, a revert of an `unprefer`) is
/// refused, names the preference, its `machine` column and the machine's key, says the machine
/// is not live, quotes no value, and appends nothing. The same revival beside the machine's own
/// revival is written. Only what a write makes is judged: a live preference that a concurrent
/// write (here an unchecked one) left naming a retired machine takes a write of its `rank`.
/// NEGATIVE CONTROL: with `refused` not calling `names_not_live`, the revival is written:
/// `` reviving a preference onto a retired machine is refused: Applied { records: 1, cells: 1,
/// .. } ``.
#[cfg(unix)]
#[test]
fn an_owner_write_reviving_a_preference_onto_a_retired_machine_is_refused() {
    use crate::alab::registry::PREFERENCE;
    let lab = Lab::new(&[Role::Writer]);
    let owner = Registry::at(lab.paths[0].clone());
    owner.open().unwrap();
    let handle = Handle::at(lab.paths[0].clone(), &REGISTRY);
    let account = "ac-0000000000000001";
    let pf = keys::preference_pk(OTHER, account).unwrap();
    let mut other = machine(OTHER);
    other[0] = put(&MACHINE, OTHER, "label", json!("m8"));
    let preference = |pk: &str, row: &str| {
        vec![
            put(&PREFERENCE, pk, "machine", json!(OTHER)),
            put(&PREFERENCE, pk, "account", json!(account)),
            put(&PREFERENCE, pk, "why", json!("owner")),
            put(&PREFERENCE, pk, "row", json!(row)),
        ]
    };
    owner
        .write(vec![
            machine(MC),
            other,
            vec![
                put(&ACCOUNT, account, "provider", json!("anthropic")),
                put(&ACCOUNT, account, "realm", json!("claude")),
                put(&ACCOUNT, account, "row", json!("live")),
            ],
            preference(&pf, "ended"),
            vec![put(&MACHINE, OTHER, "row", json!("retired"))],
        ])
        .unwrap();
    let written = handle.history().unwrap().len();
    let refusal = handle
        .commit_owner(|_| Ok(((), vec![vec![put(&PREFERENCE, &pf, "row", json!("live"))]])))
        .map(|((), applied)| applied)
        .expect_err("reviving a preference onto a retired machine is refused")
        .to_string();
    assert_eq!(handle.history().unwrap().len(), written, "nothing written");
    assert!(
        refusal.contains(&format!(
            "preference.machine: row {pf}: names machine row {OTHER}, which is not live"
        )),
        "{refusal}"
    );
    assert!(!refusal.contains("m8"), "a refusal quotes no value");

    // Made live with its machine, it is written.
    handle
        .commit_owner(|_| {
            Ok((
                (),
                vec![
                    vec![put(&MACHINE, OTHER, "row", json!("live"))],
                    vec![put(&PREFERENCE, &pf, "row", json!("live"))],
                ],
            ))
        })
        .expect("a revival beside its machine's is written");

    // A live preference left naming a retired machine takes a write that leaves the reference.
    let stray = keys::preference_pk(MC, account).unwrap();
    owner
        .write(vec![
            vec![
                put(&PREFERENCE, &stray, "machine", json!(MC)),
                put(&PREFERENCE, &stray, "account", json!(account)),
                put(&PREFERENCE, &stray, "why", json!("owner")),
                put(&PREFERENCE, &stray, "row", json!("live")),
            ],
            vec![put(&MACHINE, MC, "row", json!("retired"))],
        ])
        .unwrap();
    handle
        .commit_owner(|_| Ok(((), vec![vec![put(&PREFERENCE, &stray, "rank", json!(2))]])))
        .expect("a write that leaves a reference a concurrent write made is written");
}

crate::att_collection!(
    /// A log collection naming accounts, for the rule over log rows (the registry holds none).
    SEEN = Collection {
        name: "seen-test",
        version: 1,
        kind: Kind::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[Column::new("account", Ty::Ref("account")).writer(Writer::Mine)],
        unique: &[],
        max_row: 1024,
    }
);

const LOGGED: Table = Table {
    application: "test",
    role: "logged",
    collections: &[&ACCOUNT, &SEEN],
    faces: &[],
    self_quota: 24,
    ends: &[],
};

/// A log row's reference is judged as a cell's: one naming an account the view does not hold
/// is refused, one naming a held account is not, whatever an earlier entry said.
/// NEGATIVE CONTROL: with `missing_row` skipping every collection that is not a cells
/// collection (as it did before), the entry naming the missing account passes:
/// `` a log row naming a missing row is refused ``.
#[test]
fn a_log_row_naming_a_missing_row_is_refused() {
    let mut view = View::empty_for_test();
    view.cells.insert(
        (ACCOUNT.name.to_owned(), "ac-1.row".to_owned()),
        crate::database::Cell {
            stopped: false,
            candidates: vec![crate::database::Candidate {
                operation: [1; 32],
                value: CellValue::owner(1, json!("live")).encode(),
            }],
        },
    );
    let entry = |account: &str| {
        vec![vec![Change {
            collection: SEEN.name.to_owned(),
            key: format!("{}.1", "0".repeat(32)),
            value: Some(json!({"account": account})),
        }]]
    };
    let found = refused(&LOGGED, &entry("ac-2"), &view, |_, _| false)
        .expect("a log row naming a missing row is refused");
    assert_eq!(
        (found.collection, found.column.as_str(), found.rule.as_str()),
        (SEEN.name, "account", "no such account row")
    );
    assert_eq!(refused(&LOGGED, &entry("ac-1"), &view, |_, _| false), None);
}

/// `cells` holding each of `changes` as its one candidate, as a view holds what one Mac wrote.
fn held(changes: &[Change]) -> crate::typed::cell::Cells {
    changes
        .iter()
        .map(|change| {
            (
                (change.collection.clone(), change.key.clone()),
                crate::database::Cell {
                    stopped: false,
                    candidates: vec![crate::database::Candidate {
                        operation: [1; 32],
                        value: change.value.clone().unwrap(),
                    }],
                },
            )
        })
        .collect()
}

/// Two live accounts a sync left sharing one label keep it through a write that leaves the
/// label alone or moves one of them off it, but one write that moves both onto another shared
/// label makes a clash of its own and is refused: a pair counts as sharing before only where
/// both held this same value.
/// NEGATIVE CONTROL: with `shared_before` asking only whether the two rows shared a value before
/// (any value), the move of both is written:
/// `` a write that moves both of a clash onto a new shared label is refused ``.
#[test]
fn a_write_that_moves_both_rows_of_a_clash_onto_a_new_shared_value_is_refused() {
    use crate::typed::grid::tests::put;
    let mut view = View::empty_for_test();
    view.cells = held(&[
        put(&ACCOUNT, "ac-1", "row", json!("live")),
        put(&ACCOUNT, "ac-1", "label", json!("x")),
        put(&ACCOUNT, "ac-2", "row", json!("live")),
        put(&ACCOUNT, "ac-2", "label", json!("x")),
    ]);
    let label = |pk: &str, label: &str| put(&ACCOUNT, pk, "label", json!(label));
    let both = vec![vec![label("ac-1", "y"), label("ac-2", "y")]];
    let found = refused(&LOGGED, &both, &view, |_, _| false)
        .expect("a write that moves both of a clash onto a new shared label is refused");
    assert_eq!(
        (found.collection, found.column.as_str()),
        (ACCOUNT.name, "label")
    );
    for leaves in [
        vec![vec![put(&ACCOUNT, "ac-1", "note", json!("shelf"))]],
        vec![vec![label("ac-1", "y")]],
        vec![vec![label("ac-1", "x")]],
    ] {
        assert_eq!(refused(&LOGGED, &leaves, &view, |_, _| false), None);
    }
}

/// A machine and the CLIs its `clis` set names, for the rule over set members.
#[cfg(unix)]
const MACHINES: Table = Table {
    application: "test",
    role: "machines",
    collections: &[&MACHINE, &crate::alab::registry::CLI],
    faces: &[],
    self_quota: 24,
    ends: &[],
};

/// A write to a live row is judged only on the references it adds: one that restates a `clis`
/// member, or a `merged_into` value, the row already holds naming a retired row (a revert that
/// rewrites what is there, an owner's save of an unchanged cell) adds nothing and is not refused,
/// as a write that leaves the column alone is not; a member it makes present or a value it puts
/// in place of another naming a retired row is refused.
/// NEGATIVE CONTROL: with `names_not_live` judging every present member and every value a write
/// writes (`buf.retain` keeping what `had` holds), the restated member is refused: ``
/// assertion `left == right` failed: restating what the row holds adds no reference left:
/// Some(Refused { collection: "machine", pk: "mc-0000000000000009", column: "clis", rule: "names
/// cli row claude, which is not live; point it at a live row, leave it empty, or make that row
/// live first" }) right: None ``.
#[cfg(unix)]
#[test]
fn a_write_restating_a_reference_a_live_row_holds_is_not_refused_for_it() {
    use crate::{alab::registry::CLI, typed::grid::tests::elem};
    let mut changes = machine(MC);
    changes.extend([
        put(&MACHINE, MC, "merged_into", json!(OTHER)),
        put(&MACHINE, OTHER, "label", json!("m8")),
        put(&MACHINE, OTHER, "row", json!("retired")),
        put(&MACHINE, MISSING, "label", json!("m7")),
        put(&MACHINE, MISSING, "row", json!("retired")),
        put(&CLI, "claude", "row", json!("retired")),
        put(&CLI, "codex", "row", json!("live")),
        put(&CLI, "grok", "row", json!("retired")),
    ]);
    let cells = held(&changes);
    for restates in [
        vec![vec![elem(&MACHINE, MC, "clis", "claude")]],
        vec![vec![put(&MACHINE, MC, "merged_into", json!(OTHER))]],
        vec![vec![elem(&MACHINE, MC, "clis", "codex")]],
    ] {
        assert_eq!(
            names_not_live(&MACHINES, &restates, &cells),
            None,
            "restating what the row holds adds no reference"
        );
    }
    for (adds, column, target) in [
        (elem(&MACHINE, MC, "clis", "grok"), "clis", "cli row grok"),
        (
            put(&MACHINE, MC, "merged_into", json!(MISSING)),
            "merged_into",
            "machine row mc-00000000000000ff",
        ),
    ] {
        let found = names_not_live(&MACHINES, &[vec![adds]], &cells)
            .expect("a reference a write adds to a retired row is refused");
        assert_eq!((found.collection, found.pk.as_str()), (MACHINE.name, MC));
        assert_eq!(found.column, column);
        assert!(
            found
                .rule
                .starts_with(&format!("names {target}, which is not live")),
            "{}",
            found.rule
        );
    }
}

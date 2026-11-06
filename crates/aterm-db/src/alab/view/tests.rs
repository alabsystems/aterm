// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView over hand-built rows; every address is synthetic (`example.test`).

use super::*;
use crate::alab::checks;
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

#[test]
fn a_bound_machine_projects_nickname_text_without_changing_its_spelling_or_grammar() {
    for nickname in [
        None,
        Some(Value::Null),
        Some(json!("MissBunny")),
        Some(json!("m86")),
    ] {
        let (pk, mut changes) = machine("m86");
        if let Some(value) = &nickname {
            changes.push(put(&MACHINE, &pk, "nickname", value.clone()));
        }
        let view = LabView::read(
            &cells(changes),
            &facts(&pk, None),
            &OwnedRead::default(),
            Some(ME),
        );
        let bound = view.my_machine().expect("the existing facts binding");
        assert_eq!(bound.pk, pk);
        assert_eq!(bound.label.as_deref(), Some("m86"));
        let expected =
            nickname
                .as_ref()
                .and_then(Value::as_str)
                .map_or(MachineNickname::Unset, |word| MachineNickname::Named {
                    word: word.to_owned(),
                    automatic: false,
                });
        assert_eq!(bound.nickname, expected);
        assert_eq!(view.my_label(), Some("m86"));
    }
}

#[test]
fn a_bound_machine_keeps_clash_labels_and_hides_conflicted_cells() {
    let (pk, mut changes) = machine("m40-3fa2");
    changes.push(put(&MACHINE, &pk, "nickname", json!("MissBunny")));
    let mut book = Book::new();
    let base = book.write(1, &[], changes);
    let view = LabView::read(
        &book.cells(),
        &facts(&pk, None),
        &OwnedRead::default(),
        Some(ME),
    );
    assert_eq!(
        view.my_machine().expect("the bound row").label.as_deref(),
        Some("m40-3fa2"),
        "the database preserves the registry label for naming policy to judge"
    );
    book.write(
        2,
        &[base],
        vec![
            put(&MACHINE, &pk, "label", json!("m86")),
            put(&MACHINE, &pk, "nickname", json!("Heron")),
        ],
    );
    book.write(
        3,
        &[base],
        vec![
            put(&MACHINE, &pk, "label", json!("m87")),
            put(&MACHINE, &pk, "nickname", json!("Otter")),
        ],
    );
    let view = LabView::read(
        &book.cells(),
        &facts(&pk, None),
        &OwnedRead::default(),
        Some(ME),
    );
    let bound = view
        .my_machine()
        .expect("a label conflict does not invent a different binding");
    assert_eq!(bound.pk, pk);
    assert_eq!(bound.label, None);
    assert_eq!(bound.nickname, MachineNickname::Conflict);
    assert_eq!(view.my_label(), None);
}

#[test]
fn a_missing_or_retired_machine_is_not_this_macs_bound_name() {
    let (pk, mut changes) = machine("m86");
    changes.push(put(&MACHINE, &pk, "nickname", json!("MissBunny")));
    let live = cells(changes.clone());
    for own_facts in [OwnedRead::default(), facts("mc-0000000000000000", None)] {
        let view = LabView::read(&live, &own_facts, &OwnedRead::default(), Some(ME));
        assert!(view.my_machine().is_none());
        assert_eq!(view.my_label(), None);
    }
    changes.push(put(&MACHINE, &pk, "row", json!("retired")));
    let retired = LabView::read(
        &cells(changes),
        &facts(&pk, None),
        &OwnedRead::default(),
        Some(ME),
    );
    assert!(retired.my_machine().is_none());
    assert_eq!(retired.my_label(), None);
    assert_eq!(
        retired.machines[&pk].nickname,
        MachineNickname::Named {
            word: "MissBunny".into(),
            automatic: false
        },
        "the row remains available for history, not as a live binding"
    );
}

#[test]
fn a_bound_nickname_retains_automatic_provenance_until_the_owner_chooses_it() {
    use crate::typed::cell::{CellKey, CellValue};

    let (pk, mut changes) = machine("m86");
    changes.push(
        cell::change(
            &MACHINE,
            CellKey {
                pk: &pk,
                col: "nickname",
                elem: None,
            },
            &CellValue::automatic(1, json!("kestrel")),
        )
        .expect("an automatic nickname"),
    );
    let mut book = Book::new();
    let base = book.write(1, &[], changes);
    let view = LabView::read(
        &book.cells(),
        &facts(&pk, None),
        &OwnedRead::default(),
        Some(ME),
    );
    assert_eq!(
        view.my_machine().expect("the bound row").nickname,
        MachineNickname::Named {
            word: "kestrel".into(),
            automatic: true
        }
    );
    book.write(
        2,
        &[base],
        vec![put(&MACHINE, &pk, "nickname", json!("kestrel"))],
    );
    let view = LabView::read(
        &book.cells(),
        &facts(&pk, None),
        &OwnedRead::default(),
        Some(ME),
    );
    assert_eq!(
        view.my_machine().expect("the bound row").nickname,
        MachineNickname::Named {
            word: "kestrel".into(),
            automatic: false
        },
        "choosing an unchanged word still records the owner's provenance"
    );
}

#[test]
fn unreadable_name_candidates_never_become_an_unset_or_authoritative_value() {
    for column in ["label", "nickname"] {
        for mixed in [false, true] {
            let pk = keys::machine_pk("m86");
            let (other_column, other_word) = if column == "label" {
                ("nickname", "MissBunny")
            } else {
                ("label", "m86")
            };
            let word = if column == "label" {
                "m86"
            } else {
                "MissBunny"
            };
            let mut book = Book::new();
            let base = book.write(
                1,
                &[],
                vec![
                    put(&MACHINE, &pk, "row", json!("live")),
                    put(&MACHINE, &pk, "status", json!("active")),
                    put(&MACHINE, &pk, other_column, json!(other_word)),
                ],
            );
            let mut malformed = put(&MACHINE, &pk, column, json!(word));
            // A stored cell envelope with an unknown field cannot be decoded. The typed
            // projection retains that unreadability even beside a valid concurrent value.
            malformed.value = Some(json!({"r": 1, "x": word, "unknown": true}));
            book.write(2, &[base], vec![malformed]);
            if mixed {
                book.write(3, &[base], vec![put(&MACHINE, &pk, column, json!(word))]);
            }
            let cells = book.cells();
            let projected = cell::project(&MACHINE, &cells);
            let row = &projected.rows[&pk];
            assert!(row.raw.contains(column));
            if mixed {
                assert_eq!(
                    row.get(column).and_then(Value::as_str),
                    Some(word),
                    "the otherwise-readable sibling must not conceal the raw candidate"
                );
            } else {
                assert!(row.get(column).is_none());
            }
            let view = LabView::read(&cells, &facts(&pk, None), &OwnedRead::default(), Some(ME));
            let bound = view
                .my_machine()
                .expect("the authenticated binding remains available");
            if column == "label" {
                assert_eq!(bound.label, None);
                assert_eq!(
                    bound.nickname,
                    MachineNickname::Named {
                        word: "MissBunny".into(),
                        automatic: false
                    }
                );
            } else {
                assert_eq!(bound.nickname, MachineNickname::Conflict);
                assert_eq!(bound.label.as_deref(), Some("m86"));
            }
        }
    }
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
/// NEGATIVE CONTROL: without the `live()` filter in `alias_targets_of`, the retired link is
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

/// An address row goes by the address it says now: a renamed alias hands over its chain under its
/// new address and none under its old one, its chain names each target by what it says now, and
/// the lab places it by its new address; an address two rows say is placed nowhere.
/// NEGATIVE CONTROLS: with `alias_targets` keyed by `keys::address_pk` again, the renamed alias's
/// new address hands over no chain: `` left: [] right: ["hub2@example.test"] ``; with
/// `mailboxes` placing every row's address (its `address_index.row` check `true ||`), the address
/// two rows say is placed: `` {"ann2@example.test": "mb-hub", "hub2@example.test": "mb-x"} ``
/// (the last assertion's message).
#[test]
fn a_renamed_alias_goes_by_its_new_address() {
    let pk = |local: &str| keys::address_pk(&format!("{local}@example.test")).unwrap();
    let addr = |local: &str| format!("{local}@example.test");
    let all = vec![
        put(&ADDRESS, &pk("ann"), "addr", json!(addr("ann2"))),
        put(&ADDRESS, &pk("ann"), "alias_of", json!(pk("hub"))),
        put(&ADDRESS, &pk("hub"), "addr", json!(addr("hub2"))),
        put(&ADDRESS, &pk("hub"), "mailbox", json!("mb-hub")),
        put(&ADDRESS, &pk("x"), "addr", json!(addr("hub2"))),
        put(&ADDRESS, &pk("x"), "mailbox", json!("mb-x")),
    ];
    let view = LabView::read(
        &cells(all),
        &OwnedRead::default(),
        &OwnedRead::default(),
        None,
    );
    assert_eq!(view.alias_targets(&addr("ann2")), [addr("hub2")]);
    assert!(view.alias_targets(&addr("ann")).is_empty());
    assert_eq!(
        view.aliases(),
        BTreeMap::from([(addr("ann2"), vec![addr("hub2")])])
    );
    let placed = view.mailboxes();
    assert_eq!(
        placed.get(&addr("ann2")).map(String::as_str),
        Some("mb-hub")
    );
    assert!(!placed.contains_key(&addr("ann")));
    assert!(!placed.contains_key("hub2@example.test"), "{placed:?}");
}

/// An alias address two rows say routes nowhere, as it is placed nowhere: `aliases` hands the
/// harness a chain only from the one row an address names, never one of two rows picked by key
/// order, and `alias_targets` reads none for it either.
/// NEGATIVE CONTROL: with `aliases` keeping every row's address (its `address_index.row` filter
/// `true ||`), the address two rows say routes: `` assertion `left == right` failed left:
/// {"dup@example.test": ["hub@example.test"], "one@example.test": ["hub@example.test"]} right:
/// {"one@example.test": ["hub@example.test"]} ``.
#[test]
fn an_alias_address_two_rows_say_routes_nowhere() {
    let pk = |local: &str| keys::address_pk(&format!("{local}@example.test")).unwrap();
    let addr = |local: &str| format!("{local}@example.test");
    let all = vec![
        put(&ADDRESS, &pk("hub"), "addr", json!(addr("hub"))),
        put(&ADDRESS, &pk("hub"), "mailbox", json!("mb-hub")),
        put(&ADDRESS, &pk("one"), "addr", json!(addr("one"))),
        put(&ADDRESS, &pk("one"), "alias_of", json!(pk("hub"))),
        put(&ADDRESS, &pk("dup1"), "addr", json!(addr("dup"))),
        put(&ADDRESS, &pk("dup1"), "alias_of", json!(pk("hub"))),
        put(&ADDRESS, &pk("dup2"), "addr", json!(addr("dup"))),
        put(&ADDRESS, &pk("dup2"), "alias_of", json!(pk("one"))),
    ];
    let view = LabView::read(
        &cells(all),
        &OwnedRead::default(),
        &OwnedRead::default(),
        None,
    );
    assert!(view.alias_targets(&addr("dup")).is_empty());
    assert_eq!(
        view.aliases(),
        BTreeMap::from([(addr("one"), vec![addr("hub")])])
    );
    assert!(!view.mailboxes().contains_key(&addr("dup")));
}

/// The mailboxes the lab itself places an address in: its `mailbox` column, and an alias the
/// mailbox of the address it is linked with; a `mailbox_hint` is a guess and decides nothing, and
/// an address the lab places nowhere is not listed.
/// NEGATIVE CONTROL: with `mailboxes` keeping a `How::Hint`, the hinted address is listed
/// (`left: 3 right: 2`).
#[test]
fn the_lab_places_an_address_in_a_mailbox_only_by_its_own_rows() {
    let pk = |local: &str| keys::address_pk(&format!("{local}@example.test")).unwrap();
    let addr = |local: &str| format!("{local}@example.test");
    let mut all = Vec::new();
    for local in ["ann", "al", "hinted", "plain"] {
        all.push(put(&ADDRESS, &pk(local), "addr", json!(addr(local))));
        all.push(put(&ADDRESS, &pk(local), "row", json!("live")));
    }
    all.push(put(&ADDRESS, &pk("ann"), "mailbox", json!("mb-hub")));
    all.push(put(&ADDRESS, &pk("al"), "alias_of", json!(pk("ann"))));
    all.push(put(
        &ADDRESS,
        &pk("hinted"),
        "mailbox_hint",
        json!("mb-guess"),
    ));
    let view = LabView::read(
        &cells(all),
        &OwnedRead::default(),
        &OwnedRead::default(),
        None,
    );
    let placed = view.mailboxes();
    assert_eq!(placed.len(), 2, "{placed:?}");
    assert_eq!(placed.get(&addr("ann")).map(String::as_str), Some("mb-hub"));
    assert_eq!(placed.get(&addr("al")).map(String::as_str), Some("mb-hub"));
}

/// The registry rows a `FleetMachineAssignment` mail-area `state` stands for, for the login `ann`
/// and its chain: `ann` an alias of `hub` where `link` is 1 or of itself where it is 2, `hub` one of `root` where `hop` is 1 or of
/// `ann` where it is 2, `hub` and `root` retired where `hg` and `rg`, and `ann`'s and `root`'s
/// own `mailbox` set where `lm` and `mb`. Every address has a row, so a link always lands on one.
fn chain_cells(state: &aterm_spec::interp::State) -> Cells {
    let pk = |local: &str| keys::address_pk(&format!("{local}@example.test")).unwrap();
    let mut all = Vec::new();
    for (local, gone) in [("ann", 0), ("hub", state["hg"]), ("root", state["rg"])] {
        all.push(put(
            &ADDRESS,
            &pk(local),
            "addr",
            json!(format!("{local}@example.test")),
        ));
        let row = if gone == 1 { "retired" } else { "live" };
        all.push(put(&ADDRESS, &pk(local), "row", json!(row)));
    }
    match state["link"] {
        1 => all.push(put(&ADDRESS, &pk("ann"), "alias_of", json!(pk("hub")))),
        2 => all.push(put(&ADDRESS, &pk("ann"), "alias_of", json!(pk("ann")))),
        _ => {}
    }
    match state["hop"] {
        1 => all.push(put(&ADDRESS, &pk("hub"), "alias_of", json!(pk("root")))),
        2 => all.push(put(&ADDRESS, &pk("hub"), "alias_of", json!(pk("ann")))),
        _ => {}
    }
    if state["lm"] == 1 {
        all.push(put(&ADDRESS, &pk("ann"), "mailbox", json!("mb-ann")));
    }
    if state["mb"] == 1 {
        all.push(put(&ADDRESS, &pk("root"), "mailbox", json!("mb-root")));
    }
    cells(all)
}

/// The `Follow` step's outputs from the chain the harness is handed (`aliases`), the lab's
/// mailbox for `ann` (`mailboxes`) and the findings `check` reports, onto `state`; `None` for a
/// chain no model state names. The whole chain is read: none, `hub`, or `hub` then the address
/// `hub` links to in `state` (`root` where `hop` is 1, `ann` round the cycle where it is 2), so a
/// chain that skips or repeats an address (`[root]` past a retired `hub`, `[ann]` round a
/// self-link) reads as no step at all.
fn follow_project(
    state: &aterm_spec::interp::State,
    chain: &[String],
    mailbox: Option<&str>,
    findings: &[checks::Finding],
) -> Option<aterm_spec::interp::State> {
    let addr = |local: &str| format!("{local}@example.test");
    let second = match state["hop"] {
        1 => Some(addr("root")),
        2 => Some(addr("ann")),
        _ => None,
    };
    let (t1, t2) = match chain {
        [] => (0, 0),
        [first] if *first == addr("hub") => (1, 0),
        [first, next] if *first == addr("hub") && Some(next) == second.as_ref() => (1, 1),
        _ => return None,
    };
    let mut next = state.clone();
    next.insert("fol", 1);
    next.insert("t1", t1);
    next.insert("t2", t2);
    let eff = match mailbox {
        None => 0,
        Some("mb-ann") => 1,
        Some("mb-root") => 2,
        Some(other) => panic!("a mailbox the bed never gives: {other}"),
    };
    next.insert("eff", eff);
    let kind = |kind: &str| i64::from(findings.iter().any(|f| f.kind == kind));
    next.insert("frt", kind("alias-of-retired"));
    next.insert("fcy", kind("alias-cycle"));
    Some(next)
}

/// The mail area's projection of `action` in this crate: `Follow` is a read of the registry rows
/// the state stands for, as the harness's lab is filled ([`LabView::aliases`],
/// [`LabView::mailboxes`]) and as `check` reads them ([`checks::lab`]). The rows themselves, the
/// routes and the resolution are the harness's (`aterm-agent`'s
/// `alab_roster::assignment_tests::an_alias_resolves_to_one_effective_mailbox`), so the walk takes
/// the model's one step for each.
pub(crate) fn follow_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    if action == "Follow" {
        let cells = chain_cells(state);
        let view = LabView::read(&cells, &OwnedRead::default(), &OwnedRead::default(), None);
        let chain = view
            .aliases()
            .remove("ann@example.test")
            .unwrap_or_default();
        let placed = view.mailboxes();
        let mailbox = placed.get("ann@example.test").map(String::as_str);
        return follow_project(state, &chain, mailbox, &checks::lab(&cells).0)
            .unwrap_or_else(|| panic!("a chain no state names: {chain:?} from {state:?}"));
    }
    let model = aterm_models_harness::fleet_machine_assignment_model();
    let [next] = &model.successors(action, state)[..] else {
        panic!("{action} is no deterministic mail step at {state:?}");
    };
    next.clone()
}

/// An alias chain resolves to one effective mailbox (§0, §6): every read of the registry a
/// `FleetMachineAssignment` mail-area state stands for is a model `Follow` successor. The chain
/// the harness is handed goes to its end, never into a retired address and never round a cycle
/// back to the alias, and an alias of itself is handed no chain; the lab's mailbox is the alias's own, else the chain's end's; and `check`
/// reports exactly a link to a retired address (`alias-of-retired`) and a cycle (`alias-cycle`),
/// a self-link included.
/// NEGATIVE CONTROLS: a reader of the raw `alias_of` cells (no liveness) hands over a retired
/// `hub`, breaking `RetiredNeverFollowed`; a reader that steps past a retired link instead of
/// stopping there hands over `[root]`, which no `Follow` successor names; a check of the alias's own link alone misses a live
/// `hub` linked to a retired `root`, breaking `BrokenLinkReported`. Neither is a `Follow`
/// successor. Run against the code, `alias_targets_of` without its `live()` filter,
/// `alias_targets_of` stepping past a retired link, `alias_targets_of` with an empty seen set
/// (`[ann]` round a self-link) and `checks::lab` without its `alias_cycle` arm each fail the walk.
#[test]
fn an_alias_chain_takes_the_harness_models_steps() {
    let model = aterm_models_harness::fleet_machine_assignment_model();
    let states: Vec<_> = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .filter(|state| state["area"] == 4)
        .collect();
    assert!(states.len() > 1000, "{} mail states", states.len());
    let mut follows = 0;
    for state in &states {
        for action in &model.actions {
            if !model.action_enabled(action.name, state) {
                continue;
            }
            follows += usize::from(action.name == "Follow");
            let next = follow_transition(state, action.name);
            assert!(
                model.successors(action.name, state).contains(&next),
                "{} from {state:?} steps to {next:?}",
                action.name
            );
        }
    }
    assert!(follows > 500, "{follows} reads");

    let bed = |want: &[(&str, i64)]| {
        states
            .iter()
            .find(|s| s["fol"] == 0 && want.iter().all(|(k, v)| s[*k] == *v))
            .expect("the unread bed")
            .clone()
    };
    let refused =
        |state: &aterm_spec::interp::State, next: aterm_spec::interp::State, law: &str| {
            assert!(!model.check_invariant(law, &next), "{law}: {next:?}");
            assert!(!model.successors("Follow", state).contains(&next));
        };
    // A reader of the raw cells: every `alias_of` followed, live or not, until a repeat.
    let retired_hub = bed(&[("link", 1), ("hg", 1), ("hop", 1), ("rg", 0)]);
    let cells = chain_cells(&retired_hub);
    let rows = cell::project(&ADDRESS, &cells);
    let mut raw = Vec::new();
    let mut at = keys::address_pk("ann@example.test").unwrap();
    let mut seen = BTreeSet::from([at.clone()]);
    while let Some(next) = rows.rows.get(&at).and_then(|row| text(row, "alias_of")) {
        if !seen.insert(next.clone()) {
            break;
        }
        raw.extend(rows.rows.get(&next).and_then(|row| text(row, "addr")));
        at = next;
    }
    let view = LabView::read(&cells, &OwnedRead::default(), &OwnedRead::default(), None);
    let placed = view.mailboxes();
    let mailbox = placed.get("ann@example.test").map(String::as_str);
    let found = checks::lab(&cells).0;
    refused(
        &retired_hub,
        follow_project(&retired_hub, &raw, mailbox, &found).expect("hub then root"),
        "RetiredNeverFollowed",
    );
    // A reader that steps past a retired link: `[root]`, a chain no state names.
    let mut skipping = Vec::new();
    let mut at = keys::address_pk("ann@example.test").unwrap();
    let mut seen = BTreeSet::from([at.clone()]);
    while let Some(next) = rows.rows.get(&at).and_then(|row| text(row, "alias_of")) {
        if !seen.insert(next.clone()) {
            break;
        }
        let row = rows.rows.get(&next).expect("every address has a row");
        if row.live() {
            skipping.extend(text(row, "addr"));
        }
        at = next;
    }
    assert_eq!(skipping, ["root@example.test"]);
    assert_eq!(
        follow_project(&retired_hub, &skipping, mailbox, &found),
        None
    );
    // A check of the alias's own link alone.
    let retired_root = bed(&[("link", 1), ("hg", 0), ("hop", 1), ("rg", 1)]);
    let cells = chain_cells(&retired_root);
    let view = LabView::read(&cells, &OwnedRead::default(), &OwnedRead::default(), None);
    let chain = view
        .aliases()
        .remove("ann@example.test")
        .unwrap_or_default();
    let placed = view.mailboxes();
    let ann = keys::address_pk("ann@example.test").unwrap();
    let own_only: Vec<_> = checks::lab(&cells)
        .0
        .into_iter()
        .filter(|f| f.pks.first() == Some(&ann))
        .collect();
    refused(
        &retired_root,
        follow_project(
            &retired_root,
            &chain,
            placed.get("ann@example.test").map(String::as_str),
            &own_only,
        )
        .expect("the live chain"),
        "BrokenLinkReported",
    );
}

/// `place` rows read beside the preferences (unix: the journal is).
#[cfg(unix)]
mod placements {
    use super::*;
    use crate::alab::journal::place::{Placed, Places, Why};

    /// Facts rows of two uuids: `ME` binds `mine`, `OTHER` binds `theirs`.
    fn two_facts(mine: &str, theirs: &str) -> OwnedRead {
        let mut read = facts(mine, None);
        read.rows.insert(
            OTHER.to_owned(),
            OwnedRow {
                machine: [2; 16],
                actor: [2; 48],
                value: json!({"n": 1, "mac": theirs}),
                rank: (0, 1, 1),
                operation: [0; 32],
            },
        );
        read
    }

    const OTHER: &str = "fedcba9876543210fedcba9876543210";

    fn placed(uuid: &str, account: &str, why: Why) -> Placed {
        Placed {
            uuid: uuid.to_owned(),
            account: account.to_owned(),
            why,
            since: 10,
            until: 1_000,
        }
    }

    /// "Which Macs hold A" and "what runs on m2" read the place rows beside the preferences (§7): a
    /// Mac holding an account it does not prefer is listed after the Macs that prefer it; a place row
    /// of a Mac that prefers the account too says nothing more; a row whose writer no facts row binds
    /// names no Mac.
    /// NEGATIVE CONTROL: with `placements` not dropping a row whose machine prefers the account,
    /// `runs_on(m3)` lists `ann` twice (`left: 2 right: 1`); with `holding` reading preferences only,
    /// `bob` is held by nobody (`left: [] right: ["mc-…"]`).
    #[test]
    fn which_macs_hold_an_account_reads_place_rows_beside_preferences() {
        let (m22, mut all) = machine("m22");
        let (m3, more) = machine("m3");
        all.extend(more);
        let (ann, more) = account("ann", "claude");
        all.extend(more);
        let (bob, more) = account("bob", "claude");
        all.extend(more);
        all.extend(prefer(&m3, &ann, Some(1)));
        let view = LabView::read(
            &cells(all),
            &two_facts(&m3, &m22),
            &OwnedRead::default(),
            Some(ME),
        );
        let places = Places {
            rows: vec![
                placed(ME, &ann, Why::Fill),
                placed(OTHER, &ann, Why::Borrow),
                placed(ME, &bob, Why::Fill),
                placed("00000000000000000000000000000001", &bob, Why::Fill),
            ],
        };
        assert_eq!(view.holding(&ann, &places), [m3.clone(), m22.clone()]);
        assert_eq!(view.holding(&bob, &places), std::slice::from_ref(&m3));
        let (preferred, running) = view.runs_on(&m3, &places);
        assert_eq!(preferred.len(), 1);
        let ran: Vec<&str> = running.iter().map(|p| p.account.as_str()).collect();
        assert_eq!(ran, [bob.as_str()], "ann is m3's preference already");
        let (_, theirs) = view.runs_on(&m22, &places);
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].why, Why::Borrow);
    }
}

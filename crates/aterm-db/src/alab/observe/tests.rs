// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account-observation planner over fixtures (`docs/DESIGN-account-discovery-2026-11-09.md`,
//! step 1), from the live bug on m65: unicorn runs in a tab no row names. Every address is
//! synthetic (`proton.example`); no real registry is read.

use super::*;
use crate::{
    alab::registry::{ACCOUNT, ACCOUNT_OBS, ADDRESS},
    typed::{
        cell::{self, CellValue, Cells},
        grid::tests::{Book, put},
        keys,
        owned::{OwnedRead, OwnedRow},
    },
};
use aterm_json::{Value, json};

const DAY: u64 = 86_400_000;
const NOW: u64 = 400 * DAY;
const ME: &str = "11111111111111111111111111111111";
const THEM: &str = "22222222222222222222222222222222";
const UNICORN: &str = "unicorn.m15.2@proton.example";
const BRONTO: &str = "brontosaurus.m22@proton.example";

fn addr_pk(addr: &str) -> String {
    keys::address_pk(addr).unwrap()
}

fn account_pk(addr: &str) -> String {
    keys::account_pk("claude", &keys::claims_email(None, Some(addr)).unwrap()).unwrap()
}

fn sighting(email: &str, state: &str, tabs: u64, at: u64) -> Sighting {
    Sighting {
        cli: "claude".into(),
        realm: "claude".into(),
        email: email.into(),
        state: state.into(),
        tabs,
        src: "profile".into(),
        at_ms: at,
    }
}

/// A registry holding bronto's address and its account (`m22` in the harness's roster).
fn bronto_registry() -> Vec<Change> {
    let address = addr_pk(BRONTO);
    let account = account_pk(BRONTO);
    vec![
        put(&ADDRESS, &address, "addr", json!(BRONTO)),
        put(&ACCOUNT, &account, "provider", json!(keys::ANTHROPIC)),
        put(&ACCOUNT, &account, "realm", json!("claude")),
        put(&ACCOUNT, &account, "login", json!(address)),
        put(&ACCOUNT, &account, "status", json!("active")),
    ]
}

fn cells(changes: Vec<Change>) -> Cells {
    let mut book = Book::new();
    book.write(1, &[], changes);
    book.cells()
}

/// This Mac's owned rows after `changes`, as `View::read` would hold them.
fn obs_after(mut read: OwnedRead, changes: &[Change]) -> OwnedRead {
    for change in changes.iter().filter(|c| c.collection == ACCOUNT_OBS.name) {
        read.rows.insert(
            change.key.clone(),
            OwnedRow {
                machine: [0; 16],
                actor: [0; 48],
                value: change.value.clone().unwrap(),
                rank: (0, 1, 1),
                operation: [0; 32],
            },
        );
    }
    read
}

fn run(uuid: &str, sightings: &[Sighting], cells: &Cells, obs: &OwnedRead, now: u64) -> Plan {
    plan(
        &Input {
            uuid,
            sightings,
            now_ms: now,
            room: true,
        },
        cells,
        obs,
    )
    .unwrap()
}

fn of(plan: &Plan, collection: &str) -> usize {
    plan.changes
        .iter()
        .filter(|c| c.collection == collection)
        .count()
}

/// A login an account row already names gets this Mac's `account-obs` row and nothing else,
/// keyed `<account>.<uuid>`, saying the account, CLI, state, tabs and source.
/// NEGATIVE CONTROL: with `matched` reading only `identity`, bronto is born again:
/// `` left: 5 right: 0 ``.
#[test]
fn existing_row_marks_obs_only() {
    let held = cells(bronto_registry());
    let plan = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(of(&plan, ACCOUNT.name), 0);
    assert_eq!(of(&plan, ADDRESS.name), 0);
    assert_eq!((plan.born, plan.marked), (0, 1));
    let row = plan.changes[0].value.as_ref().unwrap();
    assert_eq!(plan.changes[0].key, format!("{}.{ME}", account_pk(BRONTO)));
    assert_eq!(row["account"], json!(account_pk(BRONTO)));
    assert_eq!(
        (
            row["cli"].clone(),
            row["state"].clone(),
            row["tabs"].clone(),
            row["src"].clone()
        ),
        (
            json!("claude"),
            json!("running"),
            json!(1),
            json!("profile")
        )
    );
    assert_eq!(
        (row["first_at"].clone(), row["at"].clone()),
        (json!(NOW), json!(NOW))
    );
}

/// THE LIVE BUG: unicorn has no address row and no account row. Its address is born, then its
/// account (provider, realm, login, `pending`, live: its Birth columns alone), every cell
/// automatic, then this Mac's row of it; the address text is only in `addr`.
/// NEGATIVE CONTROL: with the address birth left out, the account's login names no row:
/// `` assertion failed: addresses.rows[&login].live() ``.
#[test]
fn unknown_address_births_address_and_account() {
    let held = cells(bronto_registry());
    let plan = run(
        ME,
        &[sighting(UNICORN, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!((plan.born, plan.marked), (1, 1));
    assert_eq!(of(&plan, ADDRESS.name), 4);
    for change in plan
        .changes
        .iter()
        .filter(|c| c.collection != ACCOUNT_OBS.name)
    {
        let value = CellValue::decode(change.value.as_ref().unwrap()).unwrap();
        assert!(value.a, "a birth cell is automatic: {}", change.key);
    }
    let text = aterm_json::to_string(&json!(
        plan.changes
            .iter()
            .map(|c| (c.key.clone(), c.value.clone()))
            .collect::<Vec<_>>()
    ))
    .unwrap();
    assert_eq!(text.matches(UNICORN).count(), 1, "{text}");
    let applied = cells([bronto_registry(), plan.changes.clone()].concat());
    let addresses = cell::project(&ADDRESS, &applied);
    let accounts = cell::project(&ACCOUNT, &applied);
    let account = &accounts.rows[&account_pk(UNICORN)];
    let login = account
        .get("login")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    assert!(addresses.rows[&login].live());
    assert_eq!(account.get("status"), Some(&json!("pending")));
    assert_eq!(account.get("realm"), Some(&json!("claude")));
    assert_eq!(
        account.get("label"),
        None,
        "the label is the owner's verb's"
    );
    assert!(account.live());
    // Bronto's row is untouched.
    assert_eq!(
        accounts.rows[&account_pk(BRONTO)].get("login"),
        Some(&json!(addr_pk(BRONTO)))
    );
}

/// A Muse Code login no row names is born as a Meta account, its provider the realm's, and its
/// `account-obs` row says the CLI. The birth writes no service: that column is the owner's.
/// NEGATIVE CONTROL: with `account_birth` taking every provider from the `claude` realm, the
/// birth names Anthropic: `` left: Some(String("anthropic")) right: Some(String("meta")) ``.
#[test]
fn a_muse_login_is_born_as_a_meta_account() {
    let held = cells(bronto_registry());
    let seen = Sighting {
        cli: "muse".into(),
        realm: keys::META.into(),
        email: UNICORN.into(),
        state: "idle".into(),
        tabs: 0,
        src: "auth-file".into(),
        at_ms: NOW,
    };
    let plan = run(ME, &[seen], &held, &OwnedRead::default(), NOW);
    assert_eq!((plan.born, plan.marked), (1, 1));
    let pk = keys::account_pk(
        keys::META,
        &keys::claims_email(None, Some(UNICORN)).unwrap(),
    )
    .unwrap();
    let applied = cells([bronto_registry(), plan.changes.clone()].concat());
    let accounts = cell::project(&ACCOUNT, &applied);
    let account = &accounts.rows[&pk];
    assert_eq!(account.get("provider"), Some(&json!(keys::META)));
    assert_eq!(account.get("realm"), Some(&json!(keys::META)));
    assert!(!account.fields.contains_key("services"));
    assert!(account.live());
    let row = plan
        .changes
        .iter()
        .find(|c| c.collection == ACCOUNT_OBS.name)
        .and_then(|c| c.value.as_ref())
        .unwrap();
    assert_eq!(
        (
            row["account"].clone(),
            row["cli"].clone(),
            row["state"].clone()
        ),
        (json!(pk), json!("muse"), json!("idle"))
    );
}

/// Two live accounts at the realm say one address (one by its login, one by its identity):
/// nothing is written and the pass names both by key.
/// NEGATIVE CONTROL: with `matched` taking the first row, a mark is written: `` left: 1 right:
/// 0 ``.
#[test]
fn ambiguous_finding_no_write() {
    let other = "ac-00000000000000ff";
    let mut changes = bronto_registry();
    changes.extend([
        put(&ACCOUNT, other, "realm", json!("claude")),
        put(
            &ACCOUNT,
            other,
            "login",
            json!(addr_pk("alias@proton.example")),
        ),
        put(&ACCOUNT, other, "identity", json!(addr_pk(BRONTO))),
    ]);
    let plan = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &cells(changes),
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(plan.changes.len(), 0);
    assert_eq!(plan.findings.len(), 1);
    assert_eq!(plan.findings[0].kind, "account-ambiguous");
    let mut want = vec![account_pk(BRONTO), other.to_owned()];
    want.sort();
    assert_eq!(plan.findings[0].pks, want);
}

/// With observation's share of the quota spent, no account is born, no first row is written and
/// the pass says `quota-spent`; a row that only changed waits and is no finding.
/// NEGATIVE CONTROL: with `room` unread, unicorn is born: `` left: 1 right: 0 ``.
#[test]
fn reserve_spent_finding() {
    let held = cells(bronto_registry());
    let spent = |sightings: &[Sighting], obs: &OwnedRead| {
        plan(
            &Input {
                uuid: ME,
                sightings,
                now_ms: NOW,
                room: false,
            },
            &held,
            obs,
        )
        .unwrap()
    };
    let unborn = spent(
        &[sighting(UNICORN, "running", 1, NOW)],
        &OwnedRead::default(),
    );
    assert_eq!((unborn.born, unborn.marked), (0, 0));
    assert!(unborn.changes.is_empty());
    assert!(unborn.findings.iter().any(|f| f.kind == "quota-spent"));
    let unmarked = spent(
        &[sighting(BRONTO, "running", 1, NOW)],
        &OwnedRead::default(),
    );
    assert!(unmarked.changes.is_empty());
    assert!(unmarked.findings.iter().any(|f| f.kind == "quota-spent"));
    let first = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(first.marked, 1);
    let obs = obs_after(OwnedRead::default(), &first.changes);
    let changed = spent(&[sighting(BRONTO, "idle", 0, NOW)], &obs);
    assert!(changed.changes.is_empty());
    assert_eq!((changed.rested, changed.findings.len()), (1, 0));
}

/// A second pass over what the first wrote writes nothing.
/// NEGATIVE CONTROL: with the `changed` test always true, the second pass marks again:
/// `` left: 1 right: 0 ``.
#[test]
fn idempotent_second_pass() {
    let seen = [sighting(UNICORN, "running", 1, NOW)];
    let first = run(
        ME,
        &seen,
        &cells(bronto_registry()),
        &OwnedRead::default(),
        NOW,
    );
    let applied = cells([bronto_registry(), first.changes.clone()].concat());
    let obs = obs_after(OwnedRead::default(), &first.changes);
    let second = run(ME, &seen, &applied, &obs, NOW + 60_000);
    assert_eq!(second.changes.len(), 0, "{:?}", second.changes);
    assert_eq!((second.born, second.marked), (0, 0));
}

/// One account read two ways on one Mac (a tab through its profile, a tab through a vault token,
/// or two CLIs) rewrites nothing: the row says the account and its state, and a pass that reads
/// it another way spends no record of this Mac's lifetime quota.
/// Nor does it wait to ride the next record: the CLI and source are no change of the row.
/// NEGATIVE CONTROL: with a `cli` or `src` difference counted as a change, the row waits as a
/// rider: `` left: (0, 1) right: (0, 0) ``.
#[test]
fn one_account_read_two_ways_rewrites_nothing() {
    let held = cells(bronto_registry());
    let first = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(first.changes.len(), 1);
    let obs = obs_after(OwnedRead::default(), &first.changes);
    let by_token = Sighting {
        src: "vault-token".into(),
        cli: "codex".into(),
        ..sighting(BRONTO, "running", 1, NOW + 60_000)
    };
    let again = run(ME, &[by_token], &held, &obs, NOW + 60_000);
    assert_eq!(again.changes.len(), 0);
    assert_eq!((again.marked, again.rested), (0, 0));
}

/// THE QUOTA BUG: a row that exists never costs a record of its own. A tab closing and opening
/// again (running to idle and back, one tab to none and back) writes nothing, as does a rest
/// period passing; the row is rewritten, its first sighting kept, beside the next account born or
/// first marked, where what it says changed or its rest period passed, and not where neither did.
/// NEGATIVE CONTROL: with a changed row written by itself, the close writes a record: `` left: 1
/// right: 0 ``; with riders left out of a birth's record, `` left: (1, 1) right: (1, 2) ``.
#[test]
fn a_row_is_rewritten_only_beside_a_birth_or_a_first_row() {
    let held = cells(bronto_registry());
    let first = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(first.changes.len(), 1);
    let obs = obs_after(OwnedRead::default(), &first.changes);
    let closed = [sighting(BRONTO, "idle", 0, NOW + 60_000)];
    let close = run(ME, &closed, &held, &obs, NOW + 60_000);
    assert_eq!(close.changes.len(), 0, "{:?}", close.changes);
    assert_eq!((close.marked, close.rested), (0, 1));
    let reopened = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW + 120_000)],
        &held,
        &obs,
        NOW + 120_000,
    );
    assert_eq!((reopened.changes.len(), reopened.rested), (0, 0));
    let later = NOW + REWRITE_DAYS * DAY;
    let rested = run(
        ME,
        &[sighting(BRONTO, "running", 1, later)],
        &held,
        &obs,
        later,
    );
    assert_eq!((rested.changes.len(), rested.rested), (0, 1));
    // Unicorn's birth writes a record, and bronto's changed row rides it.
    let with_birth = run(
        ME,
        &[
            sighting(BRONTO, "idle", 0, NOW + 60_000),
            sighting(UNICORN, "running", 1, NOW + 60_000),
        ],
        &held,
        &obs,
        NOW + 60_000,
    );
    assert_eq!((with_birth.born, with_birth.marked), (1, 2));
    let key = format!("{}.{ME}", account_pk(BRONTO));
    let row = with_birth
        .changes
        .iter()
        .find(|c| c.key == key)
        .and_then(|c| c.value.as_ref())
        .unwrap();
    assert_eq!(
        (
            row["n"].clone(),
            row["first_at"].clone(),
            row["state"].clone()
        ),
        (json!(2), json!(NOW), json!("idle"))
    );
    // A row that neither changed nor rested does not ride.
    let unchanged = run(
        ME,
        &[
            sighting(BRONTO, "running", 1, NOW + 60_000),
            sighting(UNICORN, "running", 1, NOW + 60_000),
        ],
        &held,
        &obs,
        NOW + 60_000,
    );
    assert_eq!((unchanged.born, unchanged.marked), (1, 1));
    // A count that moves within its bucket (two tabs to three) is no change.
    let two = run(
        ME,
        &[sighting(BRONTO, "running", 2, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    let obs = obs_after(OwnedRead::default(), &two.changes);
    let three = run(
        ME,
        &[
            sighting(BRONTO, "running", 3, NOW + 60_000),
            sighting(UNICORN, "running", 1, NOW + 60_000),
        ],
        &held,
        &obs,
        NOW + 60_000,
    );
    assert_eq!((three.born, three.marked), (1, 1));
}

/// Two Macs that observe one unknown login birth the same address and account cells, so their
/// records merge into one row with no conflicted cell; each writes only its own mark.
/// NEGATIVE CONTROL: with the birth's `status` taken from the sighting's state (`running` on one
/// Mac, `idle` on the other, each as the word it is), the two births differ: `` assertion
/// `left == right` failed ``.
#[test]
fn concurrent_births_converge() {
    let held = cells(bronto_registry());
    let mine = run(
        ME,
        &[sighting(UNICORN, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    let theirs = run(
        THEM,
        &[sighting(UNICORN, "idle", 0, NOW + 5)],
        &held,
        &OwnedRead::default(),
        NOW + 5,
    );
    let births = |p: &Plan| -> Vec<Change> {
        p.changes
            .iter()
            .filter(|c| c.collection != ACCOUNT_OBS.name)
            .cloned()
            .collect()
    };
    assert_eq!(births(&mine), births(&theirs));
    let mut book = Book::new();
    let base = book.write(1, &[], bronto_registry());
    book.write(2, &[base], mine.changes.clone());
    book.write(3, &[base], theirs.changes.clone());
    let accounts = cell::project(&ACCOUNT, &book.cells());
    let row = &accounts.rows[&account_pk(UNICORN)];
    for (col, field) in &row.fields {
        assert!(!field.conflicted(), "{col}");
    }
    let keys: Vec<&String> = mine
        .changes
        .iter()
        .chain(&theirs.changes)
        .filter(|c| c.collection == ACCOUNT_OBS.name)
        .map(|c| &c.key)
        .collect();
    assert!(keys[0].ends_with(ME) && keys[1].ends_with(THEM), "{keys:?}");
}

/// An owner's concurrent `status` beats the birth's `pending` by the column's rule, and a pass
/// over a row the owner already wrote never writes its cells.
/// NEGATIVE CONTROL: with `status` given no conflict rule, the field holds: `` left: None right:
/// Some(String("paused")) ``.
#[test]
fn owner_cell_beats_birth() {
    let held = cells(bronto_registry());
    let born = run(
        ME,
        &[sighting(UNICORN, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    let mut book = Book::new();
    let base = book.write(1, &[], bronto_registry());
    book.write(2, &[base], born.changes.clone());
    book.write(
        3,
        &[base],
        vec![put(
            &ACCOUNT,
            &account_pk(UNICORN),
            "status",
            json!("paused"),
        )],
    );
    let accounts = cell::project(&ACCOUNT, &book.cells());
    assert_eq!(
        accounts.rows[&account_pk(UNICORN)].get("status"),
        Some(&json!("paused"))
    );
    // Once the owner's row is there, the pass only marks.
    let later = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(of(&later, ACCOUNT.name), 0);
}

/// A login of an account the owner retired is that account's, and the account stays retired: the
/// pass writes this Mac's first row of it and births nothing, and once that record lands after
/// the owner's retire (it does: this Mac reads the row as retired only from that write), the row
/// is still retired with the owner's status, and the next pass writes nothing. A birth there was
/// appended after the retire, so its automatic `row=live` would replace the owner's word where no
/// authority rule holds it inert, and the account would come back in use.
///
/// NEGATIVE CONTROL: `matched` without its not-live arm plans the birth at the retired row's own
/// key: the first `assert_eq!` reads its account cells (`left: 5 right: 0`).
#[test]
fn a_login_of_a_retired_account_marks_it_and_never_revives_it() {
    let pk = account_pk(BRONTO);
    let mut book = Book::new();
    let first_write = book.write(1, &[], bronto_registry());
    let retire = book.write(
        2,
        &[first_write],
        vec![
            put(&ACCOUNT, &pk, LIFECYCLE, json!("retired")),
            put(&ACCOUNT, &pk, "status", json!("banned")),
        ],
    );
    let held = book.cells();
    let first = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(of(&first, ACCOUNT.name), 0, "{:?}", first.changes);
    assert_eq!(of(&first, ADDRESS.name), 0);
    assert_eq!((first.born, of(&first, ACCOUNT_OBS.name)), (0, 1));

    book.write(3, &[retire], first.changes.clone());
    let after = book.cells();
    let accounts = cell::project(&ACCOUNT, &after);
    let row = &accounts.rows[&pk];
    assert!(!row.live(), "{row:?}");
    assert_eq!(row.get("status"), Some(&json!("banned")));
    let again = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW + DAY)],
        &after,
        &obs_after(OwnedRead::default(), &first.changes),
        NOW + DAY,
    );
    assert!(again.changes.is_empty(), "{:?}", again.changes);
}

/// A login whose one address row the owner retired, with no account yet: the account is born
/// without a `login`, since a live account naming a retired address would have the rules every
/// write passes refuse the pass (`typed::check::names_not_live`), and every later pass that
/// planned it again. The plan passes those rules; the address stays retired and unwritten; and
/// the next pass finds the account by its key and writes no second birth over it.
/// NEGATIVE CONTROL: with `matched`'s retired-holder arm removed, the birth names the retired
/// address and the rules refuse it: `` assertion `left == right` failed: the birth passes the
/// rules every write passes left: Some(Refused { collection: "account", pk: "ac-…", column:
/// "login", rule: "names address row ad-…, which is not live; point it at a live row, leave it
/// empty, or make that row live first" }) right: None ``.
#[test]
fn a_login_whose_only_address_row_is_retired_is_born_without_it() {
    let address = addr_pk(BRONTO);
    let pk = account_pk(BRONTO);
    let held = cells(vec![
        put(&ADDRESS, &address, "addr", json!(BRONTO)),
        put(&ADDRESS, &address, LIFECYCLE, json!("retired")),
    ]);
    let first = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!((first.born, first.marked), (1, 1));
    assert_eq!(of(&first, ADDRESS.name), 0, "{:?}", first.changes);
    let mut view = crate::typed::View::empty_for_test();
    view.cells = held.clone();
    assert_eq!(
        crate::typed::check::refused(
            crate::att::REGISTRY.table,
            std::slice::from_ref(&first.changes),
            &view,
            crate::alab::seeds::compiled,
        ),
        None,
        "the birth passes the rules every write passes"
    );
    let after = cells(
        [
            vec![
                put(&ADDRESS, &address, "addr", json!(BRONTO)),
                put(&ADDRESS, &address, LIFECYCLE, json!("retired")),
            ],
            first.changes.clone(),
        ]
        .concat(),
    );
    let accounts = cell::project(&ACCOUNT, &after);
    assert!(accounts.rows[&pk].live());
    assert_eq!(accounts.rows[&pk].get("login"), None);
    assert!(!cell::project(&ADDRESS, &after).rows[&address].live());
    let again = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW + DAY)],
        &after,
        &obs_after(OwnedRead::default(), &first.changes),
        NOW + DAY,
    );
    assert_eq!(again.born, 0);
    assert_eq!(of(&again, ACCOUNT.name), 0, "{:?}", again.changes);
}

/// The account a pass gave birth to without a login, its address then retired, is still the
/// sighting's row by its key once the owner makes the address live again: no live row at the realm
/// says the address (the account has no login), but a pass writes no second birth over the row,
/// whose automatic `status=active` and default label would replace the owner's `paused` and
/// `mine`. The pass marks the account and births nothing, and the owner's cells stand.
/// NEGATIVE CONTROL: with `matched` finding a row at the birth's key only where it is not live
/// (`accounts.rows.get(&pk).is_some_and(|row| !row.live())`, as it did), the live address takes
/// the plain holder arm and the birth is written again: `` assertion `left == right` failed: a
/// second birth over the existing account: ["ac-….provider", "ac-….realm", "ac-….login",
/// "ac-….status", "ac-….services.claude-code", "ac-….row", "ac-….label"] left: 1 right: 0 ``.
#[test]
fn an_account_born_without_its_retired_login_is_not_born_again_once_the_address_is_live() {
    let address = addr_pk(BRONTO);
    let pk = account_pk(BRONTO);
    let mut book = Book::new();
    let retired = book.write(
        1,
        &[],
        vec![
            put(&ADDRESS, &address, "addr", json!(BRONTO)),
            put(&ADDRESS, &address, LIFECYCLE, json!("retired")),
        ],
    );
    let first = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW)],
        &book.cells(),
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(first.born, 1);
    let born = book.write(2, &[retired], first.changes.clone());
    book.write(
        3,
        &[retired, born],
        vec![
            put(&ACCOUNT, &pk, "status", json!("paused")),
            put(&ACCOUNT, &pk, "label", json!("mine")),
            put(&ADDRESS, &address, LIFECYCLE, json!("live")),
        ],
    );
    let again = run(
        ME,
        &[sighting(BRONTO, "running", 1, NOW + DAY)],
        &book.cells(),
        &obs_after(OwnedRead::default(), &first.changes),
        NOW + DAY,
    );
    let keys: Vec<&str> = again.changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(
        again.born, 0,
        "a second birth over the existing account: {keys:?}"
    );
    assert_eq!(of(&again, ACCOUNT.name), 0, "{keys:?}");
    assert_eq!(of(&again, ADDRESS.name), 0, "{keys:?}");
    // The pass's rewrite of this Mac's row, if any, lands after the owner's word and leaves it.
    let seen = book.all();
    book.write(2, &seen, again.changes);
    let row = &cell::project(&ACCOUNT, &book.cells()).rows[&pk];
    assert!(row.live());
    assert_eq!(row.get("status"), Some(&json!("paused")));
    assert_eq!(row.get("label"), Some(&json!("mine")));
}

/// Every row a plan writes to `account-obs` carries this Mac's uuid as its suffix, the one key
/// shape `SelfSuffix` admits from this signer.
/// NEGATIVE CONTROL: with the key made `<uuid>.<account>`, `` assertion failed: key.ends_with(ME)
/// ``.
#[test]
fn obs_row_is_self_suffix_only() {
    assert_eq!(ACCOUNT_OBS.writers, crate::typed::Writers::SelfSuffix);
    let held = cells(bronto_registry());
    let plan = run(
        ME,
        &[
            sighting(UNICORN, "running", 1, NOW),
            sighting(BRONTO, "idle", 0, NOW),
        ],
        &held,
        &OwnedRead::default(),
        NOW,
    );
    let marks: Vec<&Change> = plan
        .changes
        .iter()
        .filter(|c| c.collection == ACCOUNT_OBS.name)
        .collect();
    assert_eq!(marks.len(), 2);
    for change in marks {
        assert!(change.key.ends_with(&format!(".{ME}")), "{}", change.key);
    }
}

/// `aterm db alab check` names what a pass could not write: two accounts saying one address
/// (`account-ambiguous`, the address then the accounts) and a Mac's row of an account its quota
/// left unborn (`quota-spent`); a row keyed by another account is a key mismatch.
/// NEGATIVE CONTROL: with `account_obs` not wired into `command::run`'s `"check"` arm the command
/// is silent; with its `saying` keyed by address alone, a second realm's row joins:
/// `` left: 3 right: 2 ``.
#[test]
fn checks_name_ambiguous_and_quota_spent() {
    let other = "ac-00000000000000ff";
    let codex = "ac-00000000000000ee";
    let mut changes = bronto_registry();
    changes.extend([
        put(&ACCOUNT, other, "realm", json!("claude")),
        put(&ACCOUNT, other, "identity", json!(addr_pk(BRONTO))),
        put(&ACCOUNT, codex, "realm", json!("codex")),
        put(&ACCOUNT, codex, "login", json!(addr_pk(BRONTO))),
    ]);
    let accounts = cell::project(&ACCOUNT, &cells(changes));
    let spent = run(
        ME,
        &[sighting(UNICORN, "running", 1, NOW)],
        &cells(bronto_registry()),
        &OwnedRead::default(),
        NOW,
    );
    let mut obs = obs_after(OwnedRead::default(), &spent.changes);
    let stray = format!("{}.{ME}", account_pk(BRONTO));
    let mut row = obs.rows.values().next().unwrap().clone();
    if let Some(fields) = row.value.as_object_mut() {
        fields.insert("account".into(), json!(other));
    }
    obs.rows.insert(stray.clone(), row);
    let found = crate::alab::checks::account_obs(&obs, &accounts);
    let kinds: Vec<(&str, &[String])> = found.iter().map(|f| (f.kind, f.pks.as_slice())).collect();
    let mut want_ambiguous = vec![addr_pk(BRONTO), account_pk(BRONTO), other.to_owned()];
    want_ambiguous[1..].sort();
    assert!(
        kinds.contains(&("account-ambiguous", want_ambiguous.as_slice())),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&("quota-spent", [account_pk(UNICORN)].as_slice())),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&("account-obs-key-mismatch", [stray].as_slice())),
        "{kinds:?}"
    );
    assert_eq!(
        found
            .iter()
            .filter(|f| f.kind == "account-ambiguous")
            .count(),
        1
    );
}

/// THE QUOTA BUG, at the store: a tab closing and opening again writes no record, however often;
/// observation writes at most [`OBSERVE_RESERVE`] records of this Mac's quota, then says
/// `quota-spent` and writes nothing; and what it spent leaves mailbox discovery its room.
/// NEGATIVE CONTROL: with `room` taken from the quota alone (no count of this Mac's
/// `account-obs` records), the births go on past the share and `records` is more than it.
#[test]
fn observation_spends_at_most_its_share_and_a_reopened_tab_nothing() {
    use crate::{
        alab::{Registry, discover::REGISTRATION_RESERVE, registry::SELF_QUOTA},
        table::tests::Lab,
    };
    use aterm_secrets::commons::roster::Role;
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry.write(vec![bronto_registry()]).unwrap();
    let config = lab.paths[0].home.join("config.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let pass = |seen: Sighting, now: u64| registry.observe_accounts(&config, &[seen], now).unwrap();
    let first = pass(sighting(BRONTO, "running", 1, NOW), NOW);
    assert_eq!((first.records, first.marked), (1, 1));
    for n in 1..=20 {
        let now = NOW + n * 60_000;
        let (state, tabs) = if n % 2 == 1 {
            ("idle", 0)
        } else {
            ("running", 1)
        };
        let flipped = pass(sighting(BRONTO, state, tabs, now), now);
        assert_eq!(flipped.records, 0, "flip {n}");
        assert!(flipped.findings.is_empty(), "{:?}", flipped.findings);
    }
    let mut records = first.records;
    let mut last = first;
    for n in 0..SELF_QUOTA {
        let now = NOW + DAY + n as u64;
        last = pass(
            sighting(&format!("u{n}@proton.example"), "running", 1, now),
            now,
        );
        records += last.records;
    }
    assert_eq!(records, OBSERVE_RESERVE);
    assert_eq!((last.records, last.born), (0, 0));
    assert!(last.findings.iter().any(|f| f.kind == "quota-spent"));
    let view = registry.view().unwrap();
    let spent = view.quota.spent.values().copied().max().unwrap();
    assert!(
        spent <= SELF_QUOTA - REGISTRATION_RESERVE - OBSERVE_RESERVE,
        "spent {spent}: discovery keeps half of what registration leaves"
    );
}

/// An observed login is born where a live account holds a label, through the pass's real
/// write path, which every derived write's rules guard (`Compiled::rules`, `unique_clash`): the
/// birth writes no label (version 2's `label` is the owner's verb's), so no live account's label
/// refuses it, the pass is written, `check` finds no shared label, and the next pass is not
/// refused either. Two births of one pass write no label between them.
/// NEGATIVE CONTROL: with `account_birth` also writing an automatic `label` of `bronto`, the
/// pass's rules refuse the whole pass (`Registry::observe_accounts`, `authored`): `` a birth
/// beside a live account's label is written: Error("account.realm: row ac-…: live row ac-…
/// holds the same realm, label; only one live row may") ``.
#[test]
fn an_observed_birth_writes_no_label_so_a_live_accounts_label_never_refuses_it() {
    use crate::{alab::Registry, table::tests::Lab, typed::check};
    use aterm_secrets::commons::roster::Role;
    let born = account_pk(UNICORN);
    let mut held = bronto_registry();
    held.push(put(&ACCOUNT, &account_pk(BRONTO), "label", json!("bronto")));
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry.write(vec![held]).unwrap();
    let config = lab.paths[0].home.join("config.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let observed = registry
        .observe_accounts(&config, &[sighting(UNICORN, "running", 1, NOW)], NOW)
        .expect("a birth beside a live account's label is written");
    assert_eq!((observed.records, observed.born), (1, 1));
    let view = registry.view().unwrap();
    let accounts = cell::project(&ACCOUNT, &view.cells);
    assert!(accounts.rows[&born].live());
    assert_eq!(
        accounts.rows[&born].get("label"),
        None,
        "born without a label"
    );
    let found = check::findings(
        &[&ACCOUNT],
        &std::collections::BTreeMap::from([(ACCOUNT.name, &accounts)]),
    );
    assert!(
        !found.iter().any(|f| f.kind() == "unique"),
        "no shared label: {found:?}"
    );
    let again = registry
        .observe_accounts(
            &config,
            &[sighting(UNICORN, "idle", 0, NOW + DAY)],
            NOW + DAY,
        )
        .expect("the next pass is not refused");
    assert_eq!(again.born, 0);

    // Two logins of one pass.
    let plan = run(
        ME,
        &[
            sighting("u0@proton.example", "running", 1, NOW),
            sighting("u1@proton.example", "running", 1, NOW),
        ],
        &cells(bronto_registry()),
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(plan.born, 2);
    let labels = plan
        .changes
        .iter()
        .filter(|c| c.collection == ACCOUNT.name && c.key.ends_with(".label"))
        .count();
    assert_eq!(labels, 0, "no birth writes a label");
}

fn seen_by(cli: &str, realm: &str, email: &str, state: &str, src: &str, at: u64) -> Sighting {
    Sighting {
        cli: cli.into(),
        realm: realm.into(),
        email: email.into(),
        state: state.into(),
        tabs: u64::from(state == "running"),
        src: src.into(),
        at_ms: at,
    }
}

/// SF-18's exit test: every account a Mac saw or used is a registry row, with nobody listing it.
///
/// One Mac sees four accounts: a CLI login place (`signed in`), a Codex tab running another
/// (`used by a tab`), one signed in only to one of aterm's own browser profiles (`browser only`),
/// and bronto, which a row names and the owner assigned to another Mac, running in a tab. Its
/// writer is fenced (a succession still preserving old records, as on m22): each pass refuses,
/// and what it saw waits in this Mac's kept file, across a process's end, for the pass after the
/// succession completes. That pass gives birth to each account no row named, writes this Mac's
/// row of each saying how it was seen, and changes no assignment. `check` names each account
/// seen and not held before, and nothing after.
///
/// NEGATIVE CONTROLS, each run: with `kept::through` handing `taken` alone (no kept file), the
/// second fenced pass hands nothing: `` left: 0 right: 4 ``; with `browser` not a source word,
/// the browser-only account is skipped: `` left: (None, 2, 3) right: (None, 3, 4) ``; with
/// `checks::observed` reading no sighting, the check before the pass is `` left: [] ``.
#[test]
fn every_account_a_mac_saw_or_used_is_a_registry_row() {
    use crate::{
        alab::{
            Registry, checks,
            registry::{MACHINE, PREFERENCE},
        },
        generation::file_seams,
        table::tests::Lab,
    };
    use aterm_secrets::commons::roster::Role;
    const SIGNED: &str = "signed.m22@proton.example";
    const USED: &str = "used.m22@proton.example";
    const BROWSED: &str = "browsed.m22@proton.example";
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let paths = lab.paths[0].clone();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    // Bronto, assigned by the owner to m65.
    let m65 = keys::machine_pk("m65");
    let bronto = account_pk(BRONTO);
    let link = keys::preference_pk(&m65, &bronto).unwrap();
    let mut owner = bronto_registry();
    owner.extend([
        put(&MACHINE, &m65, "label", json!("m65")),
        put(&MACHINE, &m65, "status", json!("active")),
        put(&PREFERENCE, &link, "machine", json!(m65)),
        put(&PREFERENCE, &link, "account", json!(bronto)),
        put(&PREFERENCE, &link, "why", json!("owner")),
    ]);
    registry.write(vec![owner]).unwrap();
    let config = paths.home.join("config.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let state = aterm_tempfile::tempdir().unwrap();
    let dir = kept::under(state.path());
    let seen = [
        seen_by("claude", "claude", SIGNED, "idle", "profile", NOW),
        seen_by("codex", "codex", USED, "running", "auth-file", NOW),
        seen_by("claude", "claude", BROWSED, "idle", BROWSER, NOW),
        seen_by("claude", "claude", BRONTO, "running", "profile", NOW),
    ];
    let codex_pk = |addr: &str| {
        keys::account_pk("codex", &keys::claims_email(None, Some(addr)).unwrap()).unwrap()
    };
    let unborn = vec![account_pk(BROWSED), account_pk(SIGNED), codex_pk(USED)];

    // Before any pass, `check` names what this Mac saw that the registry does not hold.
    {
        let mut kept = kept::Kept::default();
        kept.hold(&seen);
        let view = registry.view().unwrap();
        let uuid = registry.own_uuid().unwrap();
        let found = checks::observed(
            &kept.observed(NOW),
            Some(&uuid),
            &view.cells,
            view.owned
                .get(ACCOUNT_OBS.name)
                .unwrap_or(&OwnedRead::default()),
            NOW,
        );
        let mut want: Vec<(&str, Vec<String>)> = unborn
            .iter()
            .map(|pk| ("account-seen-unregistered", vec![pk.clone()]))
            .collect();
        want.push(("account-seen-unmarked", vec![bronto.clone()]));
        want.sort();
        let mut got: Vec<(&str, Vec<String>)> = found
            .iter()
            .map(|f| (f.finding.kind, f.finding.pks.clone()))
            .collect();
        got.sort();
        assert_eq!(got, want);
        assert!(
            found
                .iter()
                .all(|f| checks::sentence(f.finding.kind).is_some())
        );
    }

    // Fenced: a succession whose completion no home is known to hold, as on m22. The pass
    // refuses, and its sightings wait in the file.
    file_seams::fence_pending(&paths, &lab.bootstrap(0));
    file_seams::seat(&paths, &lab.paths[1]);
    file_seams::complete(&paths);
    file_seams::invalidate_completion(&paths);
    let (handed, done) = kept::through(&dir, &seen, NOW, |pending| {
        registry.observe_accounts(&config, pending, NOW)
    });
    assert_eq!(handed.len(), 4);
    let refused = done.expect_err("a fenced writer authors nothing");
    assert!(
        refused
            .to_string()
            .contains("succession is still preserving"),
        "{refused}"
    );
    // Another pass while fenced, with nothing new gathered: still all four, still refused.
    let (handed, done) = kept::through(&dir, &[], NOW + 60_000, |pending| {
        registry.observe_accounts(&config, pending, NOW + 60_000)
    });
    assert_eq!(handed.len(), 4);
    assert!(done.is_err());
    // A process that ends loses nothing: a fresh read of the file holds every sighting.
    assert_eq!(kept::load(&dir).pending().len(), 4);

    // The succession completes again; the next pass, with nothing new gathered, registers
    // them all.
    file_seams::complete(&paths);
    let later = NOW + 3_600_000;
    let (handed, done) = kept::through(&dir, &[], later, |pending| {
        registry.observe_accounts(&config, pending, later)
    });
    let done = done.unwrap();
    assert_eq!(handed.len(), 4);
    assert_eq!((done.quiet, done.born, done.marked), (None, 3, 4));
    assert!(kept::load(&dir).pending().is_empty(), "the pass took them");

    let view = registry.view().unwrap();
    let uuid = registry.own_uuid().unwrap();
    let accounts = view.project(&ACCOUNT);
    for pk in &unborn {
        let row = accounts.rows.get(pk).expect("born");
        assert!(row.live());
        assert!(row.get("login").and_then(Value::as_str).is_some());
    }
    let obs = view.owned.get(ACCOUNT_OBS.name).unwrap();
    let how_of = |pk: &str| {
        let row = &obs.rows[&format!("{pk}.{uuid}")].value;
        how(row["state"].as_str().unwrap(), row["src"].as_str().unwrap())
    };
    assert_eq!(how_of(&account_pk(SIGNED)), "signed in");
    assert_eq!(how_of(&codex_pk(USED)), "used by a tab");
    assert_eq!(how_of(&account_pk(BROWSED)), "browser only");
    assert_eq!(how_of(&bronto), "used by a tab");
    // No assignment changed: the owner's one link still names m65 and bronto.
    let links = view.project(&PREFERENCE);
    assert_eq!(links.rows.len(), 1);
    assert_eq!(
        links.rows[&link].get("machine").and_then(Value::as_str),
        Some(m65.as_str())
    );
    // And `check` finds nothing this Mac saw that the registry does not hold.
    let found = checks::observed(
        &kept::load(&dir).observed(later),
        Some(&uuid),
        &view.cells,
        obs,
        later,
    );
    assert_eq!(found, Vec::new());
}

/// The kept file: a newer sighting replaces an older one of its login; a browser's does not
/// replace a CLI's within [`kept::HOLD_MS`], and does after it; a pass settles only what it
/// was handed, never a newer sighting gathered since; and the file reads back what it wrote.
/// NEGATIVE CONTROL: with `hold` taking every newer sighting, the browser's replaces the
/// profile's: `` left: "browser" right: "profile" ``.
#[test]
fn the_kept_file_holds_the_strongest_newest_sighting_until_a_pass_takes_it() {
    let mut kept = kept::Kept::default();
    let cli = seen_by("claude", "claude", BRONTO, "idle", "profile", NOW);
    kept.hold(std::slice::from_ref(&cli));
    kept.hold(&[seen_by(
        "claude",
        "claude",
        BRONTO,
        "idle",
        BROWSER,
        NOW + DAY,
    )]);
    assert_eq!(kept.pending()[0].src, "profile");
    kept.hold(&[seen_by(
        "claude",
        "claude",
        BRONTO,
        "idle",
        BROWSER,
        NOW + 3 * DAY,
    )]);
    assert_eq!(kept.pending()[0].src, BROWSER);
    let passed = kept.pending();
    kept.hold(&[seen_by(
        "claude",
        "claude",
        BRONTO,
        "running",
        "profile",
        NOW + 4 * DAY,
    )]);
    kept.settle(&passed);
    assert_eq!(kept.pending().len(), 1, "a newer sighting still waits");
    let read = kept::Kept::parse(kept.render().as_bytes());
    assert!(read == kept);
    kept.settle(&kept.pending());
    assert!(kept.pending().is_empty());
    kept.prune(NOW + 4 * DAY + kept::KEEP_MS + 1);
    assert!(kept.observed(NOW + 4 * DAY).is_empty());
}

/// Of one account's sightings a CLI's speaks before a browser's, whatever their order.
/// NEGATIVE CONTROL: without the planner's ordering, the row says the browser's:
/// `` left: String("browser") right: String("profile") ``.
#[test]
fn a_browser_sighting_speaks_last_for_its_account() {
    let plan = run(
        ME,
        &[
            seen_by("claude", "claude", BRONTO, "idle", BROWSER, NOW),
            seen_by("claude", "claude", BRONTO, "idle", "profile", NOW),
        ],
        &cells(bronto_registry()),
        &OwnedRead::default(),
        NOW,
    );
    let row = plan.changes[0].value.as_ref().unwrap();
    assert_eq!(row["src"], json!("profile"));
}

/// A sighting whose address does not read (a domain with no dot, which a CLI's file may say) is
/// skipped, and the pass goes on with the rest: kept on disk, it would otherwise refuse every
/// later pass on this Mac, in every process.
/// NEGATIVE CONTROL: with `plan` refusing the pass on `matched`'s error, `run`'s unwrap panics
/// on the whole pass's `Err`.
#[test]
fn a_sighting_whose_address_does_not_read_is_skipped_and_the_pass_goes_on() {
    const DOTLESS: &str = "someone@localhost";
    assert!(keys::address(DOTLESS).is_err(), "the fixture does not read");
    let plan = run(
        ME,
        &[
            sighting(DOTLESS, "running", 1, NOW),
            sighting(BRONTO, "running", 1, NOW),
        ],
        &cells(bronto_registry()),
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!((plan.born, plan.marked), (0, 1));
    assert_eq!(plan.changes[0].key, format!("{}.{ME}", account_pk(BRONTO)));
    assert!(plan.waiting.is_empty());
}

/// A sighting whose birth or first row the quota refused stays pending in the kept file, for a
/// pass with room (after a roll) to register; one the registry already holds as this Mac's row
/// says is taken.
/// NEGATIVE CONTROLS, each run: with `through` settling every sighting handed, `` left: 0
/// right: 1 ``; with `plan` naming no sighting in `waiting`, `` left: [] right: [0] ``.
#[test]
fn a_sighting_the_quota_refused_waits_for_a_pass_with_room() {
    use crate::{alab::Registry, table::tests::Lab};
    use aterm_secrets::commons::roster::Role;
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry.write(vec![bronto_registry()]).unwrap();
    let config = lab.paths[0].home.join("config.toml");
    aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
    let state = aterm_tempfile::tempdir().unwrap();
    let dir = kept::under(state.path());
    let mut spent = 0;
    for n in 0..OBSERVE_RESERVE as u64 {
        let now = NOW + n;
        let seen = [sighting(&format!("u{n}@proton.example"), "running", 1, now)];
        let (_, done) = kept::through(&dir, &seen, now, |pending| {
            registry.observe_accounts(&config, pending, now)
        });
        spent += done.unwrap().records;
    }
    assert_eq!(spent, OBSERVE_RESERVE);
    assert!(kept::load(&dir).pending().is_empty());
    let later = NOW + DAY;
    let seen = [
        sighting("late@proton.example", "running", 1, later),
        sighting("u0@proton.example", "running", 1, later),
    ];
    let (handed, done) = kept::through(&dir, &seen, later, |pending| {
        registry.observe_accounts(&config, pending, later)
    });
    let done = done.unwrap();
    assert_eq!((done.quiet, done.records), (None, 0));
    assert!(done.findings.iter().any(|f| f.kind == "quota-spent"));
    let late = handed
        .iter()
        .position(|s| s.email == "late@proton.example")
        .unwrap();
    assert_eq!(done.waiting, vec![late]);
    let pending = kept::load(&dir).pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].email, "late@proton.example");
}

/// A codex account in the registry, its login bronto's address.
fn codex_registry() -> (String, Vec<Change>) {
    let pk = keys::account_pk("codex", &keys::claims_email(None, Some(BRONTO)).unwrap()).unwrap();
    let mut out = bronto_registry();
    out.extend([
        put(&ACCOUNT, &pk, "provider", json!(keys::OPENAI)),
        put(&ACCOUNT, &pk, "realm", json!("codex")),
        put(&ACCOUNT, &pk, "login", json!(addr_pk(BRONTO))),
        put(&ACCOUNT, &pk, "status", json!("active")),
    ]);
    (pk, out)
}

/// A kept sighting older than this Mac's row of its account says nothing newer of it, and
/// leaves the account to a newer sighting through another login in the same pass.
/// NEGATIVE CONTROL: with the age test after the account is taken as seen, the codex-alab tab
/// writes nothing: `` panicked … running ``.
#[test]
fn a_stale_sighting_leaves_its_account_to_a_newer_one() {
    let (codex, registry) = codex_registry();
    let held = cells(registry);
    let idle = seen_by("codex", "codex", BRONTO, "idle", "auth-file", NOW);
    let first = run(
        ME,
        std::slice::from_ref(&idle),
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(first.marked, 1);
    let obs = obs_after(OwnedRead::default(), &first.changes);
    let later = NOW + 60_000;
    let plan = run(
        ME,
        &[
            seen_by("codex", "codex", BRONTO, "idle", "auth-file", NOW - DAY),
            seen_by("codex-alab", "codex", BRONTO, "running", "auth-file", later),
            seen_by("claude", "claude", UNICORN, "idle", "profile", later),
        ],
        &held,
        &obs,
        later,
    );
    let row = plan
        .changes
        .iter()
        .find(|c| c.key == format!("{codex}.{ME}"))
        .and_then(|c| c.value.as_ref());
    assert!(
        row.is_some_and(|row| row["state"] == json!("running") && row["cli"] == json!("codex-alab")),
        "running"
    );
}

/// The kept file: a CLI's sighting a little older than a kept browser one of its login (another
/// process gathered it just before this one's browser look) replaces it, as a newer one does.
/// NEGATIVE CONTROL: with `hold` dropping every older sighting, `` left: "browser" right:
/// "profile" ``.
#[test]
fn a_cli_sighting_a_little_older_than_a_kept_browser_one_replaces_it() {
    let mut kept = kept::Kept::default();
    kept.hold(&[seen_by("claude", "claude", BRONTO, "idle", BROWSER, NOW)]);
    kept.hold(&[seen_by(
        "claude",
        "claude",
        BRONTO,
        "idle",
        "profile",
        NOW - 60_000,
    )]);
    assert_eq!(kept.pending()[0].src, "profile");
    // One older than the hold gives way: the browser's is the news.
    let mut kept = kept::Kept::default();
    kept.hold(&[seen_by("claude", "claude", BRONTO, "idle", BROWSER, NOW)]);
    kept.hold(&[seen_by(
        "claude",
        "claude",
        BRONTO,
        "idle",
        "profile",
        NOW - kept::HOLD_MS - 1,
    )]);
    assert_eq!(kept.pending()[0].src, BROWSER);
}

/// `check` keeps a sighting that still waits for a pass however old it is (a Mac fenced for
/// longer than [`kept::KEEP_MS`]); a taken one it compares for [`kept::KEEP_MS`].
/// NEGATIVE CONTROL: with `observed` filtering by age alone, `` left: 0 right: 1 ``.
#[test]
fn check_keeps_a_pending_sighting_however_old() {
    let mut kept = kept::Kept::default();
    let old = seen_by("claude", "claude", BRONTO, "idle", "profile", NOW);
    kept.hold(std::slice::from_ref(&old));
    let late = NOW + kept::KEEP_MS + DAY;
    kept.prune(late);
    assert_eq!(kept.observed(late).len(), 1);
    assert!(kept.observed(late)[0].1, "pending");
    kept.settle(&[old]);
    assert!(kept.observed(late).is_empty());
}

/// Each finding of what this Mac saw says which login it means, never its address: the CLI, how
/// it was seen ([`how`]), how long ago, and whether it waits for a pass; and the GUI's Check
/// view reads that line as the finding's detail. An account a row names that this Mac's rows do
/// not list names the account's own row, which the view can show.
/// NEGATIVE CONTROLS, each run: with `lab_json` leaving out the line, `` left: "" right:
/// "claude · used by a tab · seen today" ``; with the unmarked finding naming `account-obs`,
/// `` left: ("account-obs", …) right: ("account", …) ``.
#[test]
fn check_says_which_login_each_finding_means_never_its_address() {
    use crate::alab::{checks, command::lab_json};
    const BROWSED: &str = "browsed.m22@proton.example";
    let seen = vec![
        (
            seen_by("claude", "claude", BROWSED, "idle", BROWSER, NOW - 2 * DAY),
            true,
        ),
        (
            seen_by("claude", "claude", BRONTO, "running", "profile", NOW),
            false,
        ),
    ];
    let found = checks::observed(
        &seen,
        Some(ME),
        &cells(bronto_registry()),
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(found.len(), 2);
    let of = |kind: &str| found.iter().find(|f| f.finding.kind == kind).unwrap();
    let unborn = of("account-seen-unregistered");
    assert_eq!(unborn.finding.pks, [account_pk(BROWSED)]);
    assert!(unborn.line.starts_with("claude"));
    assert!(unborn.line.contains(how("idle", BROWSER)));
    assert!(unborn.line.contains("2 days"));
    let unmarked = of("account-seen-unmarked");
    assert_eq!(
        (unmarked.finding.collection, unmarked.finding.pks.clone()),
        (ACCOUNT.name, vec![account_pk(BRONTO)])
    );
    assert!(unmarked.line.contains(how("running", "profile")));
    assert_ne!(unborn.line, unmarked.line, "waiting is said");
    let json = lab_json(&[], &found, &[]);
    for (finding, seen) in json.iter().zip(&found) {
        assert!(!finding.to_string().contains('@'), "{finding}");
        assert_eq!(crate::att::command::line(finding).detail, seen.line);
    }
}

/// The source word `browser` moves `account-obs` to a new version (design §3.8): an older
/// build's version-1 row, which lists no `browser`, gives way to this build's when it publishes,
/// so the stored schema admits what this build writes, and two builds publishing into a fresh
/// generation never split one version.
/// NEGATIVE CONTROL: with `ACCOUNT_OBS` at version 1, the older row stands: `` left: One {
/// version: 1, … } ``.
#[test]
fn the_browser_word_moves_account_obs_to_a_new_version() {
    use crate::{
        alab::Registry,
        table::tests::Lab,
        typed::{SCHEMA, owned::SchemaState},
    };
    use aterm_secrets::commons::roster::Role;
    let mut older = ACCOUNT_OBS.schema_row();
    let object = older.as_object_mut().unwrap();
    object.insert("r".into(), json!(1));
    let schema = object.get_mut("x").unwrap().as_object_mut().unwrap();
    schema.insert("version".into(), json!(1));
    for column in schema
        .get_mut("columns")
        .and_then(Value::as_array_mut)
        .unwrap()
    {
        if let Some(words) = column
            .as_object_mut()
            .and_then(|column| column.get_mut("type"))
            .and_then(Value::as_object_mut)
            .and_then(|ty| ty.get_mut("enum"))
            .and_then(Value::as_array_mut)
        {
            words.retain(|word| word.as_str() != Some(BROWSER));
        }
    }
    assert_ne!(older, ACCOUNT_OBS.schema_row());
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry
        .write(vec![vec![Change {
            collection: SCHEMA.into(),
            key: ACCOUNT_OBS.name.into(),
            value: Some(older),
        }]])
        .unwrap();
    registry.publish_schema().unwrap();
    assert_eq!(
        registry.view().unwrap().schema(ACCOUNT_OBS.name),
        SchemaState::One {
            version: ACCOUNT_OBS.version,
            row: ACCOUNT_OBS.schema_row()
        }
    );
}

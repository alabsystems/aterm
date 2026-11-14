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
/// `` left: 7 right: 0 ``.
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
/// account (provider, realm, login, active, its default label, the service, live), every cell
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
    assert_eq!(account.get("status"), Some(&json!("active")));
    assert_eq!(account.get("realm"), Some(&json!("claude")));
    assert!(account.live());
    // Bronto's row is untouched.
    assert_eq!(
        accounts.rows[&account_pk(BRONTO)].get("login"),
        Some(&json!(addr_pk(BRONTO)))
    );
}

/// A Muse Code login no row names is born as a Meta account whose service is the registry's
/// word for Muse Code, and its `account-obs` row says the CLI.
/// NEGATIVE CONTROL: with `service` answering `muse` for Meta (no word of
/// [`crate::alab::registry::SERVICES`]), the plan is refused whole: `` called `Result::unwrap()`
/// on an `Err` value ``.
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
    assert!(account.fields["services"].has_member("muse-code"));
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
/// NEGATIVE CONTROL: with the account's label made from the uuid, the label conflicts:
/// `` assertion failed: !field.conflicted() ``.
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

/// An owner's concurrent `status` beats the birth's `active` by the column's rule, and a pass
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
/// appended after the retire, so its automatic `row=live` and `status=active` replaced the
/// owner's cells and the account came back in use.
///
/// NEGATIVE CONTROL: `matched` without its not-live arm plans the birth at the retired row's own
/// key: the first `assert_eq!` reads its account cells (`left: 7 right: 0`).
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

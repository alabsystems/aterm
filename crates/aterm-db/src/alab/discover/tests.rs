// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Address-list discovery: the plan over fixtures, then real labs through
//! `Registry::observe_mailbox` on scratch Commons state. Every address is synthetic
//! (`example.test`, `proton.example`); no real mailbox is read.

use super::*;
use crate::{
    alab::{
        Registry, mail,
        registry::{self, ADDRESS, MAILBOX, MAILBOX_OBS, MODEL},
    },
    table::tests::Lab,
    typed::{
        cell::{self, CellKey, CellValue, Cells},
        grid::tests::{Book, put},
        keys,
        owned::{OwnedRead, OwnedRow},
    },
};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;

const DAY: u64 = 86_400_000;
const NOW: u64 = 400 * DAY;
const ME: &str = "11111111111111111111111111111111";
const MAILBOX_PK: &str = "mb-0011223344556677";
const OTHER_PK: &str = "mb-ffeeddccbbaa9988";
const LOGIN: &str = "owner@proton.example";

fn pk(addr: &str) -> String {
    keys::address_pk(addr).unwrap()
}

fn listed(addr: &str) -> Listed {
    Listed {
        addr: addr.into(),
        own: false,
        receives: true,
    }
}

/// The owner's mailbox list as Proton states it: the login (the account's own), its twin, two
/// receiving aliases, one disabled alias.
fn listing(target: Option<&str>) -> Listing {
    Listing {
        login: LOGIN.into(),
        target: target.map(str::to_owned),
        listed: vec![
            Listed {
                own: true,
                ..listed(LOGIN)
            },
            Listed {
                own: true,
                ..listed("owner@pm.example")
            },
            listed("Ann@Example.test"),
            listed("bob@example.test"),
            listed("new@example.test"),
            Listed {
                receives: false,
                ..listed("off@example.test")
            },
        ],
        complete: true,
    }
}

/// A registry holding the mailbox row, `ann` linked to it by the owner, and rows for `bob`, `off`
/// and, as AI logins too, the mailbox's own login and its short-domain twin.
fn registry_cells() -> Vec<Change> {
    vec![
        put(&MAILBOX, MAILBOX_PK, "label", json!("proton-1")),
        put(&MAILBOX, MAILBOX_PK, "service", json!("proton")),
        put(
            &ADDRESS,
            &pk("ann@example.test"),
            "addr",
            json!("ann@example.test"),
        ),
        put(
            &ADDRESS,
            &pk("ann@example.test"),
            "mailbox",
            json!(MAILBOX_PK),
        ),
        put(
            &ADDRESS,
            &pk("bob@example.test"),
            "addr",
            json!("bob@example.test"),
        ),
        put(
            &ADDRESS,
            &pk("off@example.test"),
            "addr",
            json!("off@example.test"),
        ),
        put(&ADDRESS, &pk(LOGIN), "addr", json!(LOGIN)),
        put(
            &ADDRESS,
            &pk("owner@pm.example"),
            "addr",
            json!("owner@pm.example"),
        ),
    ]
}

fn cells(changes: Vec<Change>) -> Cells {
    let mut book = Book::new();
    book.write(1, &[], changes);
    book.cells()
}

fn obs(rows: &[(String, Value)]) -> OwnedRead {
    OwnedRead {
        rows: rows
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    OwnedRow {
                        machine: [0; 16],
                        actor: [0; 48],
                        value: value.clone(),
                        rank: (0, 1, 1),
                        operation: [0; 32],
                    },
                )
            })
            .collect(),
        inert: 0,
    }
}

fn run(listing: &Listing, keyed: Option<&str>, cells: &Cells, read: &OwnedRead, now: u64) -> Plan {
    plan(
        &Input {
            uuid: ME,
            listing,
            keyed,
            now_ms: now,
        },
        cells,
        read,
    )
    .unwrap()
}

fn written(plan: &Plan) -> &Value {
    plan.changes
        .iter()
        .find(|c| c.collection == MAILBOX_OBS.name)
        .and_then(|c| c.value.as_ref())
        .unwrap()
}

fn obs_key() -> String {
    format!("{MAILBOX_PK}.{ME}")
}

/// The owner's link on a listed address names the open mailbox; the observation lists the keys of
/// the receiving addresses the registry holds, never the login or the account's own addresses
/// (the login's row included) or a disabled one, and the address being signed in is born, in the
/// same record, under its key, with no address text anywhere but its `addr` cell.
/// NEGATIVE CONTROL: with `receiving` keeping the account's own addresses (the `!listed.own`
/// filter removed), the twin's row is listed: `` left: Observed { .., listed: 4, .. } right:
/// Observed { .., listed: 3, .. } ``.
#[test]
fn an_open_mailbox_lists_held_rows_and_births_the_target() {
    let held = cells(registry_cells());
    let plan = run(
        &listing(Some("new@example.test")),
        None,
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(
        plan.outcome,
        Outcome::Observed {
            mailbox: MAILBOX_PK.into(),
            listed: 3,
            born: 1
        }
    );
    let mut want = vec![
        pk("ann@example.test"),
        pk("bob@example.test"),
        pk("new@example.test"),
    ];
    want.sort();
    let row = written(&plan);
    assert_eq!(row["addresses"], json!(want));
    assert_eq!(row["mailbox"], json!(MAILBOX_PK));
    assert_eq!(row["method"], json!("proton-api"));
    assert_eq!(row["complete"], json!(true));
    assert_eq!(
        (row["n"].clone(), row["at"].clone()),
        (json!(1), json!(NOW))
    );
    let key = &plan.changes.last().unwrap().key;
    assert_eq!(key, &obs_key());
    // The birth: four automatic cells, the address only in `addr`.
    let born: Vec<&Change> = plan
        .changes
        .iter()
        .filter(|c| c.collection == ADDRESS.name)
        .collect();
    assert_eq!(born.len(), 4);
    let text = aterm_json::to_string(&json!(
        plan.changes
            .iter()
            .map(|c| (c.key.clone(), c.value.clone()))
            .collect::<Vec<_>>()
    ))
    .unwrap();
    assert_eq!(text.matches("new@example.test").count(), 1, "{text}");
    assert!(!text.contains("owner@"), "the login is nowhere: {text}");
    assert!(
        !row["addresses"]
            .to_string()
            .contains(&pk("owner@pm.example"))
    );
    for change in &born {
        assert!(!change.key.contains('@'));
        let value = CellValue::decode(change.value.as_ref().unwrap()).unwrap();
        assert!(value.a, "a birth cell is automatic");
    }
    let applied = cells([registry_cells(), plan.changes.clone()].concat());
    let row = &cell::project(&ADDRESS, &applied).rows[&pk("new@example.test")];
    assert_eq!(row.get("origin"), Some(&json!("discovered")));
    assert_eq!(row.get("domain"), Some(&json!("example.test")));
    assert!(row.live());
}

/// The mailbox comes from this Mac's mailbox key where the importer left one, else from the
/// owner's links: none is unknown and writes nothing, two hold the pass with a finding by key, a
/// key whose row is not here yet falls back to the links, and a target the list does not receive,
/// or that a retired row holds, is not born.
/// NEGATIVE CONTROL: with `identify` taking the first of several linked mailboxes (the `_` arm
/// answering `Ok(linked.first())`), the ambiguous pass observes: `` left: Observed { .. } right:
/// Held ``.
#[test]
fn the_mailbox_is_keyed_or_linked_and_never_guessed() {
    let held = cells(registry_cells());
    let none = OwnedRead::default();
    // No link and no key: nothing names the mailbox.
    let unlinked = cells(
        registry_cells()
            .into_iter()
            .filter(|c| !c.key.contains("mailbox"))
            .collect(),
    );
    let plan = run(&listing(None), None, &unlinked, &none, NOW);
    assert_eq!((plan.outcome, plan.changes.len()), (Outcome::Unknown, 0));
    // The key names it with no link.
    let plan = run(&listing(None), Some(MAILBOX_PK), &unlinked, &none, NOW);
    assert!(matches!(plan.outcome, Outcome::Observed { born: 0, .. }));
    // A key with no row here falls back to the links.
    let plan = run(&listing(None), Some(OTHER_PK), &held, &none, NOW);
    assert_eq!(written(&plan)["mailbox"], json!(MAILBOX_PK));
    // Two links name two mailboxes.
    let two = cells(
        [
            registry_cells(),
            vec![
                put(&MAILBOX, OTHER_PK, "label", json!("proton-2")),
                put(
                    &ADDRESS,
                    &pk("bob@example.test"),
                    "mailbox",
                    json!(OTHER_PK),
                ),
            ],
        ]
        .concat(),
    );
    let plan = run(&listing(None), None, &two, &none, NOW);
    assert_eq!(plan.outcome, Outcome::Held);
    assert!(plan.changes.is_empty());
    let mut both = vec![MAILBOX_PK.to_owned(), OTHER_PK.to_owned()];
    both.sort();
    assert_eq!(
        plan.findings,
        vec![Finding {
            kind: "mailbox-ambiguous",
            collection: MAILBOX.name,
            pks: both
        }]
    );
    // An unlisted target, a disabled one and a retired row are never born.
    for target in ["elsewhere@example.test", "off@example.test"] {
        let plan = run(&listing(Some(target)), None, &held, &none, NOW);
        assert!(matches!(plan.outcome, Outcome::Observed { born: 0, .. }));
    }
    let retired = cells(
        [
            registry_cells(),
            vec![put(
                &ADDRESS,
                &pk("new@example.test"),
                "row",
                json!("retired"),
            )],
        ]
        .concat(),
    );
    let plan = run(
        &listing(Some("new@example.test")),
        None,
        &retired,
        &none,
        NOW,
    );
    assert!(
        plan.changes
            .iter()
            .all(|c| c.collection == MAILBOX_OBS.name),
        "a retired row is not reborn"
    );
}

/// A row is rewritten only when what it says changes: an unchanged list writes nothing; a change
/// that births nothing rests until the row's last rewrite is `REWRITE_DAYS` old, then lands with
/// the counter moved; a birth lands at once, even the day the row last changed, so the second
/// address signed in on a day is born too; a first list that lists nothing the registry holds
/// writes nothing.
/// NEGATIVE CONTROL: with the rest taking births too (the `born == 0 &&` guard removed), the
/// same-day birth rests: `` panicked at …: Rested { mailbox: "mb-…" } ``; with the rest
/// removed, the same-day change writes: `` left: Observed { .. } right: Rested { .. } ``.
#[test]
fn a_row_changes_monthly_and_a_birth_at_once() {
    let held = cells(registry_cells());
    let first = run(&listing(None), None, &held, &OwnedRead::default(), NOW);
    let row = written(&first).clone();
    let mine = obs(&[(obs_key(), row.clone())]);
    let again = run(&listing(None), None, &held, &mine, NOW + DAY * 5);
    assert_eq!(
        (again.outcome, again.changes.len()),
        (Outcome::Unchanged, 0)
    );
    let more = Listing {
        listed: listing(None)
            .listed
            .into_iter()
            .filter(|l| !l.addr.starts_with("bob"))
            .collect(),
        ..listing(None)
    };
    let rested = Outcome::Rested {
        mailbox: MAILBOX_PK.into(),
    };
    for later in [1000, DAY, (REWRITE_DAYS - 1) * DAY] {
        let plan = run(&more, None, &held, &mine, NOW + later);
        assert_eq!(plan.outcome, rested, "{later}");
        assert!(plan.changes.is_empty());
    }
    let month = run(&more, None, &held, &mine, NOW + REWRITE_DAYS * DAY);
    assert!(matches!(month.outcome, Outcome::Observed { listed: 1, .. }));
    assert_eq!(written(&month)["n"], json!(2));
    // A birth is written the same day as the row's last change.
    let born = run(
        &listing(Some("new@example.test")),
        None,
        &held,
        &mine,
        NOW + 1000,
    );
    assert!(
        matches!(born.outcome, Outcome::Observed { born: 1, .. }),
        "{:?}",
        born.outcome
    );
    assert_eq!(written(&born)["n"], json!(2));
    // A partial list is a change too, and says so.
    let partial = Listing {
        complete: false,
        ..listing(None)
    };
    let plan = run(&partial, None, &held, &mine, NOW + REWRITE_DAYS * DAY);
    assert_eq!(written(&plan)["complete"], json!(false));
    // A first list naming nothing held writes nothing.
    let bare = cells(vec![put(&MAILBOX, MAILBOX_PK, "label", json!("proton-1"))]);
    let plan = run(
        &listing(None),
        Some(MAILBOX_PK),
        &bare,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!((plan.outcome, plan.changes.len()), (Outcome::Empty, 0));
}

fn lab_config(lab: &Lab) -> std::path::PathBuf {
    lab.paths[0].home.join("config.toml")
}

/// On a real lab, `Registry::observe_mailbox` declares nothing, honours `[commons]
/// exclude_tables`, writes one record that the registry reads back as this Mac's observation (so
/// the address reads that mailbox as its effective one, and the target's birth is a live row), and
/// a second pass writes nothing. Discovery spends only its share of this Mac's quota: births go on
/// until `REGISTRATION_RESERVE` records are left, and then a pass writes nothing, leaving those
/// to registration.
/// NEGATIVE CONTROL: with the policy check removed from `observe_mailbox` (its `admitted` call),
/// the excluded pass writes: `` left: Observed { .. } right: Excluded ``; with discovery's
/// reserve 0, the births spend registration's share: `` assertion failed: spent <= SELF_QUOTA -
/// REGISTRATION_RESERVE ``.
#[test]
fn a_pass_writes_one_record_within_policy_and_quota() {
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    let target = listing(Some("new@example.test"));
    let undeclared = registry.observe_mailbox(&lab_config(&lab), &target, NOW);
    assert!(
        undeclared.is_err() || undeclared.unwrap().outcome == Outcome::NotHeld,
        "a pass declares nothing"
    );
    assert!(registry.view().is_err(), "a pass declares nothing");
    registry.open().unwrap();
    registry.write(vec![registry_cells()]).unwrap();
    let table = registry.table().unwrap().unwrap();
    std::fs::write(
        lab_config(&lab),
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&table)
        ),
    )
    .unwrap();
    let before = registry.view().unwrap().ops.len();
    let excluded = registry
        .observe_mailbox(&lab_config(&lab), &target, NOW)
        .unwrap();
    assert_eq!((excluded.outcome, excluded.records), (Outcome::Excluded, 0));
    assert_eq!(registry.view().unwrap().ops.len(), before);
    std::fs::write(lab_config(&lab), "").unwrap();
    let done = registry
        .observe_mailbox(&lab_config(&lab), &target, NOW)
        .unwrap();
    assert_eq!(done.records, 1);
    assert!(matches!(done.outcome, Outcome::Observed { born: 1, .. }));
    let view = registry.view().unwrap();
    let uuid = keys::uuid32(&crate::alab::tests::machine_of(&lab, 0));
    let row = &view.owned[MAILBOX_OBS.name].rows[&format!("{MAILBOX_PK}.{uuid}")];
    assert_eq!(row.value["mailbox"], json!(MAILBOX_PK));
    let seen = mail::observed(
        view.owned[MAILBOX_OBS.name]
            .rows
            .iter()
            .map(|(key, row)| (key.as_str(), &row.value)),
    );
    let addresses = view.project(&ADDRESS);
    let bob = pk("bob@example.test");
    let reached = mail::effective(&addresses, &seen, &|_| None, &bob).unwrap();
    assert_eq!(
        (reached.mailbox.as_str(), reached.how),
        (MAILBOX_PK, mail::How::Observed)
    );
    assert!(addresses.rows[&pk("new@example.test")].live());
    assert!(
        view.quota.spent.values().any(|spent| *spent >= 1),
        "one automatic record"
    );
    let again = registry
        .observe_mailbox(&lab_config(&lab), &target, NOW + DAY)
        .unwrap();
    assert_eq!((again.outcome, again.records), (Outcome::Unchanged, 0));
    // Spend most of discovery's share on other automatic records, then birth an address per
    // sign-in until the share is gone: the pass that finds it gone writes nothing.
    let spend: Vec<Vec<Change>> = (1..=10)
        .map(|n| {
            vec![
                cell::change(
                    &MODEL,
                    CellKey {
                        pk: &format!("vm-{n}"),
                        col: "row",
                        elem: None,
                    },
                    &CellValue::automatic(1, json!("live")),
                )
                .unwrap(),
            ]
        })
        .collect();
    registry.write(spend).unwrap();
    let fresh: Vec<String> = (2..=24).map(|n| format!("new{n}@example.test")).collect();
    let wide = Listing {
        listed: target
            .listed
            .iter()
            .cloned()
            .chain(fresh.iter().map(|addr| listed(addr)))
            .collect(),
        ..target.clone()
    };
    let mut births = 0;
    let mut refused = false;
    for addr in &fresh {
        let before = registry.view().unwrap().ops.len();
        let pass = registry
            .observe_mailbox(
                &lab_config(&lab),
                &Listing {
                    target: Some(addr.clone()),
                    ..wide.clone()
                },
                NOW + 2 * DAY,
            )
            .unwrap();
        if pass.outcome == Outcome::QuotaSpent {
            assert_eq!(pass.records, 0);
            assert_eq!(registry.view().unwrap().ops.len(), before);
            refused = true;
            break;
        }
        assert!(
            matches!(pass.outcome, Outcome::Observed { born: 1, .. }),
            "{:?} after {births} births",
            pass.outcome
        );
        births += 1;
    }
    assert!(births >= 1 && refused, "births {births}, refused {refused}");
    let view = registry.view().unwrap();
    let spent = view.quota.spent.values().copied().max().unwrap();
    assert!(
        spent <= registry::SELF_QUOTA - REGISTRATION_RESERVE,
        "spent {spent}"
    );
}

/// `aterm db alab check` inspects `mailbox-obs` rows alongside cells: it reports a row whose key
/// prefix differs from its `mailbox` cell, an unknown or retired mailbox, two mailboxes listing
/// one live address, and an owner's `address.mailbox` (or `+tag` base) that disagrees with
/// discovery, while never flagging an unlisted own-login or matching `+tag`.
/// NEGATIVE CONTROL: without `checks::mailbox_obs` wired into `command::run`'s `"check"` arm,
/// `report["lab"]` misses `"mailbox-discovery-differs"` and `"mailbox-obs-ambiguous"`.
#[test]
fn check_reports_mailbox_obs_ambiguity_and_owner_disagreement() {
    let ann = pk("ann@example.test");
    let tag_ok = pk("ann+ok@example.test");
    let tag_diff = pk("ann+diff@example.test");
    let shared = pk("bob@example.test");
    let own = pk(LOGIN);
    let retired_mb = "mb-9999888877776666";
    let held = cells(vec![
        put(&MAILBOX, MAILBOX_PK, "label", json!("proton-1")),
        put(&MAILBOX, OTHER_PK, "label", json!("proton-2")),
        put(&MAILBOX, retired_mb, "label", json!("proton-old")),
        put(&MAILBOX, retired_mb, "row", json!("retired")),
        put(&ADDRESS, &ann, "addr", json!("ann@example.test")),
        put(&ADDRESS, &ann, "mailbox", json!(OTHER_PK)),
        put(&ADDRESS, &tag_ok, "addr", json!("ann+ok@example.test")),
        put(&ADDRESS, &tag_ok, "mailbox", json!(MAILBOX_PK)),
        put(&ADDRESS, &tag_diff, "addr", json!("ann+diff@example.test")),
        put(&ADDRESS, &tag_diff, "mailbox", json!(OTHER_PK)),
        put(&ADDRESS, &shared, "addr", json!("bob@example.test")),
        put(&ADDRESS, &own, "addr", json!(LOGIN)),
        put(&ADDRESS, &own, "mailbox", json!(MAILBOX_PK)),
    ]);
    let read = obs(&[
        (
            format!("{MAILBOX_PK}.{ME}"),
            json!({"mailbox": MAILBOX_PK, "complete": true, "at": 10, "addresses": [ann, shared]}),
        ),
        (
            format!("{OTHER_PK}.{ME}"),
            json!({"mailbox": OTHER_PK, "complete": true, "at": 10, "addresses": [shared]}),
        ),
        (
            format!("{retired_mb}.{ME}"),
            json!({"mailbox": retired_mb, "complete": false, "at": 10, "addresses": []}),
        ),
        (
            format!("mb-0000000000000000.{ME}"),
            json!({"mailbox": MAILBOX_PK, "complete": true, "at": 99, "addresses": []}),
        ),
        (
            format!("mb-1234567890abcdef.{ME}"),
            json!({"mailbox": "mb-1234567890abcdef", "complete": false, "at": 1, "addresses": []}),
        ),
    ]);
    let found = crate::alab::checks::mailbox_obs(
        &read,
        &cell::project(&ADDRESS, &held),
        &cell::project(&MAILBOX, &held),
    );
    let kinds: Vec<&str> = found.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        vec![
            "mailbox-discovery-differs",
            "mailbox-discovery-differs",
            "mailbox-obs-ambiguous",
            "mailbox-obs-key-mismatch",
            "mailbox-obs-mailbox-unknown",
            "mailbox-obs-on-retired-mailbox",
        ],
        "{found:?}"
    );
    assert!(
        !found
            .iter()
            .any(|f| f.pks.contains(&own) || f.pks.contains(&tag_ok)),
        "unlisted own-login and matching +tag never false-positive: {found:?}"
    );

    // End-to-end through `aterm db alab check` on a real lab.
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry
        .write(vec![vec![
            put(&MAILBOX, MAILBOX_PK, "label", json!("proton-1")),
            put(&MAILBOX, OTHER_PK, "label", json!("proton-2")),
            put(&ADDRESS, &ann, "addr", json!("ann@example.test")),
            put(&ADDRESS, &ann, "mailbox", json!(MAILBOX_PK)),
        ]])
        .unwrap();
    let seen = registry
        .observe_mailbox(&lab_config(&lab), &listing(None), NOW)
        .unwrap();
    assert!(matches!(seen.outcome, Outcome::Observed { .. }));
    registry
        .write(vec![vec![put(&ADDRESS, &ann, "mailbox", json!(OTHER_PK))]])
        .unwrap();
    let mut review = |_: &std::path::Path| Ok(());
    let report =
        crate::alab::command::run(&lab.paths[0], &["check".to_owned()], None, &mut review, NOW)
            .unwrap();
    let lab_findings = report["lab"].as_array().unwrap();
    assert!(
        lab_findings
            .iter()
            .any(|f| f["finding"] == json!("mailbox-discovery-differs")),
        "{lab_findings:?}"
    );
}

/// When a mailbox already receives `MAX_LISTED` held addresses that sort before a newly born
/// target key, the birth record still lists the newly born target (marking `complete` false);
/// and when `observe_mailbox` signs in through a short-domain twin or bare-username address,
/// `alab-mailbox-keys.json` still identifies the mailbox from the account's `own` original
/// address.
/// NEGATIVE CONTROL: with `listed.into_iter().take(MAX_LISTED)` dropping the tail unconditionally,
/// the newly born `zzz` address key is absent from `row["addresses"]`.
#[test]
fn a_born_target_past_the_list_cap_stays_listed_and_own_twins_key_the_mailbox() {
    let mut target_addr = String::new();
    let mut target_pk = String::new();
    for i in 0..20_000 {
        let cand = format!("born-{i}@example.test");
        let cand_pk = pk(&cand);
        if cand_pk.as_str() > "ad-fe" {
            target_addr = cand;
            target_pk = cand_pk;
            break;
        }
    }
    assert!(!target_pk.is_empty());
    let mut setup = vec![
        put(&MAILBOX, MAILBOX_PK, "label", json!("proton-1")),
        put(&MAILBOX, MAILBOX_PK, "service", json!("proton")),
    ];
    let mut listed_items = vec![
        Listed {
            own: true,
            ..listed(LOGIN)
        },
        listed(&target_addr),
    ];
    let mut earlier = 0usize;
    let mut seq = 0usize;
    while earlier < MAX_LISTED {
        let addr = format!("cap-{seq}@example.test");
        seq += 1;
        let addr_pk = pk(&addr);
        if addr_pk < target_pk {
            setup.push(put(&ADDRESS, &addr_pk, "addr", json!(addr)));
            if earlier == 0 {
                setup.push(put(&ADDRESS, &addr_pk, "mailbox", json!(MAILBOX_PK)));
            }
            listed_items.push(listed(&addr));
            earlier += 1;
        }
    }
    let held = cells(setup);
    let plan = run(
        &Listing {
            login: LOGIN.into(),
            target: Some(target_addr),
            listed: listed_items,
            complete: true,
        },
        None,
        &held,
        &OwnedRead::default(),
        NOW,
    );
    assert_eq!(
        plan.outcome,
        Outcome::Observed {
            mailbox: MAILBOX_PK.into(),
            listed: MAX_LISTED,
            born: 1,
        }
    );
    let row = written(&plan);
    assert_eq!(row["complete"], json!(false));
    let addresses = row["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), MAX_LISTED);
    assert!(addresses.contains(&json!(target_pk)));

    // Own-twin mailbox key resolution in `Registry::observe_mailbox`:
    let lab = Lab::new(&[Role::Writer]);
    std::fs::write(lab_config(&lab), "").unwrap();
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    registry
        .write(vec![vec![
            put(&MAILBOX, MAILBOX_PK, "label", json!("proton-1")),
            put(&MAILBOX, MAILBOX_PK, "service", json!("proton")),
            put(
                &ADDRESS,
                &pk("bob@example.test"),
                "addr",
                json!("bob@example.test"),
            ),
        ]])
        .unwrap();
    let mut keys = crate::alab::import::MailboxKeys::default();
    keys.record(LOGIN, MAILBOX_PK, "proton-1");
    aterm_uds::private_file::ensure_private_dir(&lab.paths[0].dir).unwrap();
    aterm_uds::private_file::write_private(
        &lab.paths[0].dir,
        crate::alab::command::MAILBOX_KEYS,
        &aterm_json::to_vec(&keys.to_json()).unwrap(),
    )
    .unwrap();
    let twin_listing = Listing {
        login: "owner@pm.example".into(),
        ..listing(None)
    };
    let done = registry
        .observe_mailbox(&lab_config(&lab), &twin_listing, NOW)
        .unwrap();
    assert_eq!(
        done.outcome,
        Outcome::Observed {
            mailbox: MAILBOX_PK.into(),
            listed: 1,
            born: 0,
        }
    );
}

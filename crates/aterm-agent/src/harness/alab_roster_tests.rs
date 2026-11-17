// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The lab's roster on scratch homes and stores; every address is synthetic (`example.test`),
//! and nothing here reads or writes this Mac's real accounts, vault or registry.

use super::*;
use crate::ai::connector::Region;
use crate::harness::alab_roster::test_machine;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[cfg(unix)]
#[test]
fn a_private_name_read_never_consumes_the_process_registry_source() {
    use crate::machine_name::{
        Origin, ensure,
        tests::{NOW, state},
    };
    let root = state();
    let lab = Lab {
        machine_name: Some(Err("the live registry has a conflicted name".into())),
        ..Lab::default()
    };
    with_lab(lab, || {
        let (name, origin) = ensure(root.path(), || Ok(Some("m7".into())), |_| false, NOW)
            .expect("a private read uses its own sources");
        assert_eq!(name.name(), "m7");
        assert_eq!(origin, Origin::Evidence);
    });
}

fn email(local: &str) -> String {
    format!("{local}@example.test")
}

fn account(local: &str, cli: Cli, label: &str, preferred: &[(&str, Option<u64>)]) -> LabAccount {
    LabAccount {
        pk: format!("ac-{:0>16}", local.len()),
        cli,
        vendor: Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            flow: None,
        },
        label: label.to_owned(),
        legacy_label: None,
        email: email(local),
        identity: None,
        browser: None,
        joinable: true,
        preferred: preferred
            .iter()
            .map(|(machine, rank)| (test_machine(machine), *rank))
            .collect(),
    }
}

#[test]
fn registry_keys_survive_label_collisions_and_missing_labels_across_roster_operations() {
    let me = MachineRef::from_registry("pk-m3", Some("m3".into()));
    let other = MachineRef::from_registry("PK_m4", Some("m3".into()));
    let mut account = account("one", Cli::Claude, "claude-one", &[]);
    account.preferred.push((other.clone(), Some(9)));
    let mut lab = Lab {
        me: Some(me.clone()),
        accounts: vec![account],
        ..Lab::default()
    };
    let existing = Roster {
        rows: vec![Row {
            label: "claude-one".into(),
            email: Some(email("one")),
            class: Class::Preferred,
            ..Row::default()
        }],
    };
    assert_eq!(lab.class_of(&lab.accounts[0]), Class::Pool);
    assert!(
        fleet_for(&lab, Cli::Claude, None)
            .unwrap()
            .accounts
            .is_empty()
    );
    let kept = retain(
        Roster::default(),
        &existing,
        Some(&lab),
        Cli::Claude,
        &me.key,
    );
    assert_eq!(kept.rows[0].class, Class::Pool);
    assert_eq!(
        holdings(
            &lab,
            Cli::Claude,
            &existing,
            &observe::sweep::places_of(
                Cli::Claude,
                &existing,
                None,
                &observe::sweep::OnToken::none()
            )
        ),
        [(lab.accounts[0].pk.clone(), "borrow")]
    );
    let digest = with_lab(lab.clone(), || fleet_digest(Cli::Claude).unwrap());
    let mut rebound = lab.clone();
    rebound.me = Some(other);
    assert_ne!(
        with_lab(rebound, || fleet_digest(Cli::Claude).unwrap()),
        digest
    );

    lab.accounts[0].preferred[0].0 = me.clone();
    assert_ne!(
        with_lab(lab.clone(), || fleet_digest(Cli::Claude).unwrap()),
        digest
    );
    for label in [None, Some("m3".to_owned()), Some("m99".to_owned())] {
        lab.me.as_mut().unwrap().label = label;
        assert_eq!(lab.class_of(&lab.accounts[0]), Class::Preferred);
        assert_eq!(lab.accounts[0].preferred_on(&me.key), Some(Some(9)));
        let fleet = fleet_for(&lab, Cli::Claude, None).unwrap();
        assert_eq!(fleet.accounts_for_machine(me.key.as_str()).len(), 1);
        assert!(fleet.accounts_for_machine("m3").is_empty());
        assert!(
            holdings(
                &lab,
                Cli::Claude,
                &existing,
                &observe::sweep::places_of(
                    Cli::Claude,
                    &existing,
                    None,
                    &observe::sweep::OnToken::none()
                )
            )
            .is_empty()
        );
        let kept = retain(
            Roster::default(),
            &existing,
            Some(&lab),
            Cli::Claude,
            &me.key,
        );
        assert_eq!(kept.rows[0].class, Class::Preferred);
        with_lab(lab.clone(), || {
            assert!(names_this_mac());
            assert!(chosen(Cli::Claude, None, None, None).unwrap().is_some());
        });
    }
}

#[test]
fn native_selector_and_composition_preserve_preference_rank_with_opaque_keys() {
    for key in ["pk-m3", "PK_m4"] {
        let me = MachineRef::from_registry(key, Some("m3".into()));
        let ranked = |local: &str, rank: Option<u64>| {
            let mut row = account(local, Cli::Claude, local, &[]);
            row.preferred.push((me.clone(), rank));
            row
        };
        let lab = Lab {
            me: Some(me.clone()),
            accounts: vec![
                ranked("alpha", Some(9)),
                ranked("zeta", Some(1)),
                ranked("middle", None),
            ],
            ..Lab::default()
        };
        let fleet = fleet_for(&lab, Cli::Claude, None).unwrap();
        let labels: Vec<_> = fleet
            .accounts_for_machine(key)
            .iter()
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(labels, ["zeta", "alpha", "middle"]);
        assert!(fleet.accounts_for_machine("m3").is_empty());
        let root = aterm_tempfile::TempDir::new().unwrap();
        let store = Store::at(root.path());
        let built = accounts_sync::build_machine_roster(&fleet, key, &store, None, None);
        assert_eq!(
            built
                .rows
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            labels
        );
        let mut reversed = built.clone();
        reversed.rows.reverse();
        let kept = retain(
            reversed,
            &Roster::default(),
            Some(&lab),
            Cli::Claude,
            &me.key,
        );
        assert_eq!(
            kept.rows
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            labels
        );
    }
}

fn legacy(account: LabAccount, label: &str) -> LabAccount {
    LabAccount {
        legacy_label: Some(label.to_owned()),
        ..account
    }
}

fn row(label: &str, dir: Option<&Path>, local: &str, cli: Cli) -> Row {
    Row {
        label: label.to_owned(),
        dir: dir.map(|dir| dir.to_str().unwrap().to_owned()),
        email: Some(email(local)),
        vendor: Vendor {
            region: (cli == Cli::Kimi).then_some(Region::Global),
            flow: None,
        },
        ..Row::default()
    }
}

#[cfg(unix)]
#[test]
fn a_bound_registry_name_snapshot_keeps_its_exact_value_over_pending_state() {
    use crate::machine_name::{RegistryName, RegistryNameRead};
    use aterm_net::fleet_member::name_record::NickClass;
    use aterm_types::machine_name::{Nickname, Number};

    let name = RegistryName {
        num: Number::new(86).expect("a number"),
        nick: Some((
            Nickname::parse("MissBunny").expect("a nickname"),
            NickClass::Chosen,
        )),
    };
    let lab = Lab {
        me: Some(test_machine("m86")),
        machine_name: Some(Ok(name.clone())),
        ..Lab::default()
    };
    with_lab(lab, || {
        assert!(names_this_mac());
        assert_eq!(machine_name(), RegistryNameRead::Bound(Ok(name.clone())));
        while_pending(|| {
            assert_eq!(
                machine_name(),
                RegistryNameRead::Bound(Ok(name.clone())),
                "a retained verified projection remains usable while a later read is pending"
            );
        });
        while_turned_off(|| assert_eq!(machine_name(), RegistryNameRead::Unbound));
    });
}

#[cfg(unix)]
#[test]
fn a_bound_name_conflict_stays_bound_without_a_display_label() {
    use crate::machine_name::RegistryNameRead;

    let why = "synthetic conflicting registry number".to_owned();
    let lab = Lab {
        me: None,
        machine_name: Some(Err(why.clone())),
        ..Lab::default()
    };
    let mut keyed = lab.clone();
    keyed.me = Some(test_machine("m86"));
    with_lab(lab, || {
        assert!(
            names_this_mac(),
            "a missing label does not erase its facts binding"
        );
        assert_eq!(machine_name(), RegistryNameRead::Bound(Err(why.clone())));
        assert!(
            chosen(Cli::Claude, None, None, None).is_err(),
            "a binding without a usable key cannot authorize legacy discovery"
        );
        while_pending(|| {
            assert_eq!(
                machine_name(),
                RegistryNameRead::Bound(Err(why.clone())),
                "a bound conflict must not become an apparently unbound machine"
            );
        });
    });
    with_lab(keyed, || {
        assert!(
            matches!(chosen(Cli::Claude, None, None, None), Ok(Some(_))),
            "a conflicted display name must not disable a usable stable machine key"
        );
    });
}

#[cfg(unix)]
#[test]
fn registry_name_fallback_distinguishes_unbound_pending_and_explicitly_disabled() {
    use crate::machine_name::RegistryNameRead;

    with_lab(Lab::default(), || {
        assert!(!names_this_mac());
        assert_eq!(machine_name(), RegistryNameRead::Unbound);
    });
    while_pending(|| {
        assert_eq!(machine_name(), RegistryNameRead::Pending);
        while_turned_off(|| assert_eq!(machine_name(), RegistryNameRead::Unbound));
        assert_eq!(
            machine_name(),
            RegistryNameRead::Pending,
            "disabling a source within a scope does not clear pending history"
        );
    });
    assert_eq!(machine_name(), RegistryNameRead::Unbound);
}

/// A Mac's preferred accounts join in rank order with an unranked one last; an account this Mac
/// does not prefer, or one that may not join (paused), is not added; an account the roster already
/// has keeps the label the roster gives it, since no sync renames a row; and a lab that names no
/// machine for this Mac yields no fleet, so the sync reads what it always read.
/// NEGATIVE CONTROL: with `fleet_for` taking the lab's label over the roster's, the kept row is
/// renamed by the sync: `` left: ["m22-2", …] right: ["claude-bbbbbb", …] ``; with the
/// `joinable` filter removed, the paused account joins.
#[test]
fn the_lab_fleet_adds_this_macs_preferred_accounts_in_rank_order() {
    let mut paused = account("dee", Cli::Claude, "claude-dddddd", &[("m22", Some(1))]);
    paused.joinable = false;
    let lab = Lab {
        me: Some(test_machine("m22")),
        bound: Default::default(),
        accounts: vec![
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", None)]),
            account("bob", Cli::Claude, "claude-bbbbbb", &[("m22", Some(2))]),
            account(
                "cy",
                Cli::Claude,
                "claude-cccccc",
                &[("m22", Some(1)), ("m3", Some(1))],
            ),
            account("eve", Cli::Claude, "claude-eeeeee", &[("m3", Some(1))]),
            account("fay", Cli::Kimi, "kimi-ffffff", &[("m22", Some(1))]),
            paused,
        ],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let existing = Roster {
        rows: vec![row(
            "m22-2",
            Some(Path::new("/scratch/m22-2")),
            "bob",
            Cli::Claude,
        )],
    };
    let fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let labels: Vec<&str> = fleet.accounts.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(labels, ["claude-cccccc", "m22-2", "claude-aaaaaa"]);
    assert!(
        fleet
            .accounts
            .iter()
            .all(|a| a.machines == [test_machine("m22").key.as_str()])
    );
    assert_eq!(fleet.alab.as_ref(), Some(&test_machine("m22")));
    let kimi = fleet_for(&lab, Cli::Kimi, None).unwrap();
    assert_eq!(kimi.accounts.len(), 1);
    assert_eq!(kimi.accounts[0].vendor.region, Some(Region::Global));
    let unbound = Lab {
        me: None,
        ..lab.clone()
    };
    assert_eq!(fleet_for(&unbound, Cli::Claude, None), None);
}

/// A sync from the lab keeps every row this Mac has, the ones no preference names included, and
/// derives each known row's class from the preferences: preferred here first, every other row
/// after. A row the lab does not know keeps its class.
/// NEGATIVE CONTROL: with `retain`'s put-back loop removed, the unpreferred row and the unknown
/// row are dropped: `` left: ["claude-aaaaaa"] right: ["claude-aaaaaa", "m20-m22", "kept"] ``.
#[test]
fn a_preference_never_drops_a_row() {
    let lab = Lab {
        me: Some(test_machine("m22")),
        bound: Default::default(),
        accounts: vec![
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", Some(1))]),
            account("cy", Cli::Claude, "claude-cccccc", &[("m20", Some(1))]),
        ],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let mut kept = row("kept", Some(Path::new("/scratch/kept")), "dee", Cli::Claude);
    kept.class = Class::Pool;
    let existing = Roster {
        rows: vec![
            row("claude-aaaaaa", None, "ann", Cli::Claude),
            row(
                "m20-m22",
                Some(Path::new("/scratch/m20-m22")),
                "cy",
                Cli::Claude,
            ),
            kept,
        ],
    };
    let rebuilt = Roster {
        rows: vec![row("claude-aaaaaa", None, "ann", Cli::Claude)],
    };
    let roster = retain(
        rebuilt,
        &existing,
        Some(&lab),
        Cli::Claude,
        &test_machine("m22").key,
    );
    let got: Vec<(&str, Class, Option<&str>)> = roster
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.class, r.dir.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("claude-aaaaaa", Class::Preferred, None),
            ("kept", Class::Pool, Some("/scratch/kept")),
            ("m20-m22", Class::Pool, Some("/scratch/m20-m22")),
        ]
    );
}

/// A label an existing row of one account holds stays that row's: the lab's account of another
/// address under the same label is not added, a higher-ranked one included, and a rebuild that
/// put another address under the row's label gets the row back as it was, its login directory
/// with it. Under the lab no old pool hold (`held_by`) survives a sync.
/// NEGATIVE CONTROL: with `fleet_for`'s `held_by_another` check removed, `work` is listed with
/// `gil`'s address; with `retain`'s `Some(at)` arm keeping the rebuilt row, `work` names `gil`.
#[test]
fn a_label_another_account_holds_stays_with_its_row() {
    let lab = Lab {
        me: Some(test_machine("m22")),
        bound: Default::default(),
        accounts: vec![
            account("gil", Cli::Claude, "work", &[("m22", Some(1))]),
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", Some(2))]),
        ],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let mut work = row("work", Some(Path::new("/scratch/work")), "ann", Cli::Claude);
    work.held_by = Some("m3".into());
    let existing = Roster { rows: vec![work] };
    let fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let listed: Vec<(&str, &str)> = fleet
        .accounts
        .iter()
        .map(|a| (a.label.as_str(), a.email.as_str()))
        .collect();
    assert_eq!(listed, [("work", email("ann").as_str())]);

    let rebuilt = Roster {
        rows: vec![row(
            "work",
            Some(Path::new("/scratch/work")),
            "gil",
            Cli::Claude,
        )],
    };
    let roster = retain(
        rebuilt,
        &existing,
        Some(&lab),
        Cli::Claude,
        &test_machine("m22").key,
    );
    assert_eq!(roster.rows.len(), 1);
    let kept = roster.row("work").unwrap();
    assert_eq!(kept.email, Some(email("ann")));
    assert_eq!(kept.dir.as_deref(), Some("/scratch/work"));
    assert_eq!(kept.held_by, None, "no old pool hold limits it");
}

/// The `[fleet] accounts_source` the owner set in `aterm.toml` wins over the lab for Claude
/// Code, whose fleet it names; the other CLIs, which that key does not name, still read the lab.
/// NEGATIVE CONTROL: with `chosen`'s `source_key` check removed, the keyed config reads the lab
/// and the first assertion fails.
#[test]
fn a_source_the_owner_set_wins_over_the_lab() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let keyed = dir.path().join("keyed.toml");
    fs::write(
        &keyed,
        "[fleet]\naccounts_source = \"https://fleet.example.test/accounts.csv\"\n",
    )
    .unwrap();
    let plain = dir.path().join("plain.toml");
    fs::write(&plain, "").unwrap();
    let lab = Lab {
        me: Some(test_machine("m22")),
        bound: Default::default(),
        accounts: vec![
            account("ann", Cli::Claude, "claude-aaaaaa", &[("m22", Some(1))]),
            account("fay", Cli::Kimi, "kimi-ffffff", &[("m22", Some(1))]),
        ],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    with_lab(lab, || {
        assert!(matches!(
            chosen(Cli::Claude, None, None, Some(&keyed)),
            Ok(None)
        ));
        assert!(matches!(
            chosen(Cli::Claude, None, None, Some(&plain)),
            Ok(Some(_))
        ));
        assert!(matches!(
            chosen(Cli::Kimi, None, None, Some(&keyed)),
            Ok(Some(_))
        ));
        assert!(
            matches!(
                chosen(Cli::Claude, None, Some(SOURCE), Some(&keyed)),
                Ok(Some(_))
            ),
            "naming `alab` still reads the lab"
        );
    });
}

/// A lab that may name this Mac but could not be read yet ([`pending`]) refuses a sync with no
/// source named, so neither the sheet nor legacy discovery labels a row after a machine meanwhile;
/// a source the owner named elsewhere is still read. With nothing pending and no lab, the sync
/// reads what it always read.
/// NEGATIVE CONTROL: with `chosen`'s `pending` refusal removed, the pending sync falls through to
/// legacy discovery (`refused while pending`).
#[test]
fn a_pending_lab_refuses_a_sync_with_no_source_named() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let plain = dir.path().join("plain.toml");
    fs::write(&plain, "").unwrap();
    let keyed = dir.path().join("keyed.toml");
    fs::write(
        &keyed,
        "[fleet]\naccounts_source = \"https://fleet.example.test/accounts.csv\"\n",
    )
    .unwrap();
    assert!(matches!(
        chosen(Cli::Claude, None, None, Some(&plain)),
        Ok(None)
    ));
    while_pending(|| {
        assert!(
            chosen(Cli::Claude, None, None, Some(&plain)).is_err(),
            "refused while pending"
        );
        assert!(chosen(Cli::Kimi, None, None, Some(&plain)).is_err());
        assert!(matches!(
            chosen(
                Cli::Claude,
                None,
                Some("/fleet/accounts.toml"),
                Some(&plain)
            ),
            Ok(None)
        ));
        assert!(matches!(
            chosen(Cli::Claude, None, None, Some(&keyed)),
            Ok(None)
        ));
    });
}

/// An alias's codes are looked for where the lab says its auth login is received: the mail route
/// that names its linked address wins over a `*@domain` route, while a route naming the alias
/// itself still wins over the link; with no lab held, the alias reads the domain route.
/// NEGATIVE CONTROL: with `resolve_index` not reading `mail_targets`, the alias resolves to the
/// domain route under the lab too: `` left: Some(1) right: Some(0) ``.
#[test]
fn an_alias_reads_the_mail_route_of_the_address_it_is_linked_with() {
    use super::super::mailbox_routes::{MailboxRoute, resolve_index};
    let route = |receives: &str| MailboxRoute {
        receives: vec![receives.to_owned()],
        ..MailboxRoute::default()
    };
    let routes = [
        route(&email("hub")),
        route("*@example.test"),
        route(&email("own")),
    ];
    assert!(mail_targets(&email("ann")).is_empty());
    assert_eq!(resolve_index(&routes, Some(&email("ann"))), Some(1));
    let lab = Lab {
        aliases: BTreeMap::from([
            (email("ann"), vec![email("hub")]),
            (email("own"), vec![email("hub")]),
        ]),
        ..Lab::default()
    };
    with_lab(lab, || {
        assert_eq!(mail_targets(&email("ANN")), [email("hub")]);
        assert_eq!(resolve_index(&routes, Some(&email("ann"))), Some(0));
        assert_eq!(resolve_index(&routes, Some(&email("own"))), Some(2));
    });
}

/// What a sync of this Mac (`m1`) makes of one account in `AlabCells`'s preference area, given
/// the lab `lab` (`pref`, `joinable`) and the roster `existing` (`row`, `held`), through the sync's own composition: [`fleet_for`], then
/// [`accounts_sync::build_machine_roster`] on a scratch store, then [`retain`].
fn preference_sync(lab: &Lab, existing: &Roster) -> Roster {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(&root.path().join("state"));
    let fleet = fleet_for(lab, Cli::Claude, Some(existing)).expect("the lab names m1");
    let built = accounts_sync::build_machine_roster(
        &fleet,
        test_machine("m1").key.as_str(),
        &store,
        Some(existing),
        None,
    );
    retain(
        built,
        existing,
        Some(lab),
        Cli::Claude,
        &test_machine("m1").key,
    )
}

/// The lab and roster an area-5 `state` stands for: account `x` (`example.test`), preferred on
/// `m1` where `pref`, else on `m2`; joinable where `joinable`; a row `m1-2` at `/scratch/x` where
/// `row`, held by `m9` where `held`.
fn preference_bed(state: &aterm_spec::interp::State) -> (Lab, Roster) {
    let on = if state["pref"] == 1 { "m1" } else { "m2" };
    let mut x = account("x", Cli::Claude, "claude-xxxxxx", &[(on, Some(1))]);
    x.joinable = state["joinable"] == 1;
    let lab = Lab {
        me: Some(test_machine("m1")),
        bound: Default::default(),
        accounts: vec![x],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let mut rows = Vec::new();
    if state["row"] == 1 {
        let mut r = row("m1-2", Some(Path::new("/scratch/x")), "x", Cli::Claude);
        r.held_by = (state["held"] == 1).then(|| "m9".to_owned());
        rows.push(r);
    }
    (lab, Roster { rows })
}

/// `state` after a sync that left `synced`: its `row` and `held` read back, and the ghosts
/// (`dropped`, `unasked`, `missing`, `limited`) set where the roster breaks the law each names.
fn preference_project(
    state: &aterm_spec::interp::State,
    synced: &Roster,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    let rows: Vec<&Row> = synced
        .rows
        .iter()
        .filter(|r| r.matches_email(&email("x")))
        .collect();
    let row = i64::from(!rows.is_empty());
    let held = i64::from(rows.iter().any(|r| r.held_by.is_some()));
    let was_row = state["row"] == 1;
    let invited = state["pref"] == 1 && state["joinable"] == 1;
    if was_row && row == 0 {
        next.insert("dropped", 1);
    }
    if !was_row && row == 1 && state["pref"] == 0 {
        next.insert("unasked", 1);
    }
    if row == 0 && invited {
        next.insert("missing", 1);
    }
    if held == 1 {
        next.insert("limited", 1);
    }
    next.insert("row", row);
    next.insert("held", held);
    next
}

/// The `AlabCells` projection of `action` from an area-5 `state`: a `Sync` is the sync's
/// composition on the bed `state` stands for; every other action is the owner's verb or a
/// person's act, which sets the bed it names.
pub(crate) fn preference_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Sync" => {
            let (lab, existing) = preference_bed(state);
            return preference_project(state, &preference_sync(&lab, &existing));
        }
        "Prefer" => next.insert("pref", 1),
        "Unprefer" => next.insert("pref", 0),
        "Pause" => next.insert("joinable", 0),
        "Ask" => next.insert("row", 1),
        "Hold" => next.insert("held", 1),
        other => panic!("{other} is no preference action"),
    };
    next
}

/// Every step a sync from the lab takes is an `AlabCells` successor from every state the model
/// reaches in its preference area (§0: a preference never limits who may run an account): the
/// sync keeps a row this Mac does not prefer or that is paused, adds an account only where this
/// Mac prefers it and it may join, and clears every old hold.
/// NEGATIVE CONTROLS: a sync that skips [`retain`] keeps the hold an old pool file set on a row
/// this Mac does not prefer (the rebuild returns the roster as it was), breaking `NoHoldUnderLab`,
/// and is no `Sync` successor; a lab that ignores a pause (the account still
/// `joinable`) adds the paused account, no `Sync` successor either.
#[test]
fn a_sync_takes_the_preference_models_steps() {
    let model = aterm_models_db::alab_cells_model();
    let states: Vec<aterm_spec::interp::State> = aterm_spec::interp::reachable_states(&model)
        .into_iter()
        .filter(|state| state["area"] == 5)
        .collect();
    assert!(states.len() > 8, "{} preference states", states.len());
    for state in &states {
        for action in &model.actions {
            if !model.action_enabled(action.name, state) {
                continue;
            }
            let next = preference_transition(state, action.name);
            assert!(
                model.successors(action.name, state).contains(&next),
                "{} from {state:?} steps to {next:?}",
                action.name
            );
        }
    }

    // Without `retain`, the old hold stays.
    let held = states
        .iter()
        .find(|s| s["row"] == 1 && s["held"] == 1 && s["pref"] == 0)
        .expect("a held row this Mac does not prefer");
    let (lab, existing) = preference_bed(held);
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(&root.path().join("state"));
    let fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let unretained = accounts_sync::build_machine_roster(
        &fleet,
        test_machine("m1").key.as_str(),
        &store,
        Some(&existing),
        None,
    );
    let next = preference_project(held, &unretained);
    assert_eq!(next["limited"], 1);
    assert!(!model.successors("Sync", held).contains(&next));

    // A pause the lab does not carry: the paused account joins.
    let paused = states
        .iter()
        .find(|s| s["row"] == 0 && s["pref"] == 1 && s["joinable"] == 0)
        .expect("a paused account this Mac prefers");
    let (mut lab, existing) = preference_bed(paused);
    lab.accounts[0].joinable = true;
    let next = preference_project(paused, &preference_sync(&lab, &existing));
    assert_eq!(next["row"], 1);
    assert!(!model.successors("Sync", paused).contains(&next));
}

/// An account asked for by name (§0, §7) is the lab's row for it, found by its label, its legacy
/// label or its account key, under its lab label with its address and derived order (an account
/// another Mac prefers is a pool row here), and a sync then keeps it as it keeps every row: the
/// request writes no preference, so the lab is as it was. A name the lab does not know is no
/// lab row (a new label, as it always was), and so is any name where the lab names no machine for
/// this Mac; a paused account or a name two accounts share is refused.
/// NEGATIVE CONTROL: with `asked_row`'s `joinable` check removed, the paused account is a row
/// (`a paused account is refused`); with its `legacy_label` match removed, the legacy name
/// finds nothing (`by its legacy label`).
#[test]
fn an_account_asked_for_by_name_joins_with_no_preference() {
    let mut paused = account("carol", Cli::Claude, "claude-cccccc", &[("m20", Some(1))]);
    paused.joinable = false;
    let lab = Lab {
        me: Some(test_machine("m1")),
        bound: Default::default(),
        accounts: vec![
            legacy(
                account("ann", Cli::Claude, "claude-aaaaaa", &[("m20", Some(1))]),
                "m20-m1",
            ),
            account("gilbert", Cli::Claude, "claude-gggggg", &[]),
            paused,
            account("evan", Cli::Claude, "claude-dupdup", &[]),
            account("eve", Cli::Kimi, "claude-dupdup", &[]),
            account("eveline", Cli::Claude, "claude-dupdup", &[]),
        ],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let before = lab.clone();
    with_lab(lab.clone(), || {
        let key = format!("ac-{:0>16}", "ann".len());
        for (name, why) in [
            ("claude-aaaaaa", "by its label"),
            ("m20-m1", "by its legacy label"),
            (key.as_str(), "by its account key"),
        ] {
            let row = asked_row(Cli::Claude, name)
                .unwrap_or_else(|| panic!("{why}"))
                .unwrap();
            assert_eq!(row.label, "claude-aaaaaa", "{why}");
            assert_eq!(row.email.as_deref(), Some(email("ann").as_str()));
            assert_eq!(row.class, Class::Pool, "another Mac prefers it");
            assert_eq!(row.dir, None);
        }
        assert!(asked_row(Cli::Claude, "nobody").is_none());
        assert!(
            asked_row(Cli::Kimi, "claude-aaaaaa").is_none(),
            "another CLI's"
        );
        assert!(
            matches!(asked_row(Cli::Claude, "claude-cccccc"), Some(Err(_))),
            "a paused account is refused"
        );
        assert!(
            matches!(asked_row(Cli::Claude, "claude-dupdup"), Some(Err(_))),
            "a name two accounts share is refused"
        );
        assert!(matches!(asked_row(Cli::Kimi, "claude-dupdup"), Some(Ok(_))));
        let asked = asked_row(Cli::Claude, "claude-gggggg").unwrap().unwrap();
        let synced = preference_sync(&lab, &Roster { rows: vec![asked] });
        let kept = synced
            .row("claude-gggggg")
            .expect("the sync keeps the asked row");
        assert_eq!(kept.class, Class::Pool);
    });
    assert_eq!(lab, before, "the request writes no preference");
    with_lab(
        Lab {
            me: None,
            ..lab.clone()
        },
        || assert!(asked_row(Cli::Claude, "claude-aaaaaa").is_none()),
    );
    assert!(asked_row(Cli::Claude, "claude-aaaaaa").is_none(), "no lab");
}

/// On a Mac the lab names, the lab is the source even where it prefers nothing here: [`chosen`]
/// gives an empty fleet rather than falling back to the sheet or a discovery that labels rows
/// after machines, and a sync of that fleet keeps every row the roster has and adds none. A Mac
/// the lab does not name reads what it always read.
/// NEGATIVE CONTROL: with `chosen`'s old `filter(|fleet| !fleet.accounts.is_empty())` restored,
/// the empty lab roster is `Ok(None)` (`the lab is the source`).
#[test]
fn an_empty_lab_roster_stays_empty_and_keeps_every_row() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let plain = dir.path().join("plain.toml");
    fs::write(&plain, "").unwrap();
    let lab = Lab {
        me: Some(test_machine("m1")),
        bound: Default::default(),
        accounts: vec![account(
            "ann",
            Cli::Claude,
            "claude-aaaaaa",
            &[("m20", Some(1))],
        )],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    with_lab(lab.clone(), || {
        let fleet = chosen(Cli::Claude, None, None, Some(&plain))
            .unwrap()
            .expect("the lab is the source");
        assert!(fleet.accounts.is_empty());
        assert_eq!(fleet.alab.as_ref(), Some(&test_machine("m1")));
    });
    let existing = Roster {
        rows: vec![
            row("m1", None, "zed", Cli::Claude),
            row("m1-2", Some(Path::new("/scratch/m1-2")), "ann", Cli::Claude),
        ],
    };
    let synced = preference_sync(&lab, &existing);
    let labels: Vec<&str> = synced.rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["m1", "m1-2"], "every row stays, none is added");
    with_lab(Lab { me: None, ..lab }, || {
        assert!(matches!(
            chosen(Cli::Claude, None, None, Some(&plain)),
            Ok(None)
        ))
    });
}

/// The upkeep's trigger under the lab reads the lab and the roster alone: a roster that already
/// holds every account this Mac prefers, in its derived order, is not due, nor is one whose row
/// has its account's legacy label, since no sync renames a row; one the lab
/// prefers a new account for, one whose row stands in another order, and one with an old pool's
/// hold are. The lab's digest moves with any account field and is none where the lab names no
/// machine for this Mac.
/// NEGATIVE CONTROL: with `sync_due`'s new-account clause removed, the added preference is not
/// due (`a new preference is due`); with `fleet_digest`'s `preferred` loop removed, the digest
/// does not move (`a preference moves the digest`); with the deleted rename clause back in
/// `sync_due`, the legacy row is due (`a legacy label is no change`).
#[test]
fn a_sync_is_due_only_when_the_lab_would_change_the_roster() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let path = dir.path().join("accounts.toml");
    let write = |rows: &[Row]| {
        fs::write(
            &path,
            accounts::append_rows_for(Cli::Claude, "", rows).unwrap(),
        )
        .unwrap();
    };
    let ann = account("ann", Cli::Claude, "claude-aaaaaa", &[("m1", Some(1))]);
    let lab = Lab {
        me: Some(test_machine("m1")),
        bound: Default::default(),
        accounts: vec![
            ann.clone(),
            legacy(
                account("bobby", Cli::Claude, "claude-bbbbbb", &[("m20", Some(1))]),
                "m20-m1",
            ),
        ],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Default::default(),
    };
    let mut pool = row(
        "claude-bbbbbb",
        Some(Path::new(at!("/scratch/b"))),
        "bobby",
        Cli::Claude,
    );
    pool.class = Class::Pool;
    let synced = vec![row("claude-aaaaaa", None, "ann", Cli::Claude), pool.clone()];
    write(&synced);
    let digest = with_lab(lab.clone(), || {
        assert!(!sync_due(Cli::Claude, &path), "nothing to do");
        fleet_digest(Cli::Claude).unwrap()
    });
    let mut more = lab.clone();
    more.accounts.push(account(
        "gil",
        Cli::Claude,
        "claude-gggggg",
        &[("m1", Some(2))],
    ));
    with_lab(more, || {
        assert!(sync_due(Cli::Claude, &path), "a new preference is due");
    });
    let mut moved = lab.clone();
    moved.accounts[1]
        .preferred
        .push((test_machine("m1"), Some(2)));
    with_lab(moved, || {
        assert!(sync_due(Cli::Claude, &path), "a row's order changed");
        assert_ne!(
            fleet_digest(Cli::Claude).unwrap(),
            digest,
            "a preference moves the digest"
        );
    });
    let mut held = pool.clone();
    held.held_by = Some("m9".into());
    write(&[row("claude-aaaaaa", None, "ann", Cli::Claude), held]);
    with_lab(lab.clone(), || {
        assert!(sync_due(Cli::Claude, &path), "an old hold is due");
    });
    let mut old = pool;
    old.label = "m20-m1".into();
    write(&[row("claude-aaaaaa", None, "ann", Cli::Claude), old]);
    with_lab(lab.clone(), || {
        assert!(!sync_due(Cli::Claude, &path), "a legacy label is no change");
    });
    with_lab(Lab { me: None, ..lab }, || {
        assert!(fleet_digest(Cli::Claude).is_none());
        assert!(!sync_due(Cli::Claude, &path));
    });
}

/// The mailbox labels come from the lab where it names this Mac: every account's lab label and
/// the legacy label a row may still have and its secrets name; none where it does not, so that
/// Mac reads what it always read.
/// NEGATIVE CONTROL: with `mailbox_labels`' legacy push removed, `kimi-m16` maps to nothing.
#[test]
fn the_mailbox_labels_are_the_labs() {
    let lab = Lab {
        me: Some(test_machine("m16")),
        accounts: vec![legacy(
            account("eve", Cli::Kimi, "kimi-eeeeee", &[]),
            "kimi-m16",
        )],
        ..Lab::default()
    };
    with_lab(lab.clone(), || {
        assert_eq!(
            mailbox_labels(),
            [
                (Cli::Kimi, "kimi-eeeeee".to_owned(), email("eve")),
                (Cli::Kimi, "kimi-m16".to_owned(), email("eve")),
            ]
        );
    });
    with_lab(Lab { me: None, ..lab }, || {
        assert!(mailbox_labels().is_empty())
    });
}

/// The lab's mailbox decides a route first: the entry whose login the lab names for the target
/// wins over a route naming the target's own address; with no lab, or where the lab names no
/// mailbox for it, the closest pattern still decides.
/// NEGATIVE CONTROL: with `resolve_index`'s `mailbox_of` arm removed, the target resolves to its
/// own address's route under the lab too: `` left: Some(0) right: Some(1) ``.
#[test]
fn the_labs_mailbox_decides_the_route_first() {
    use super::super::mailbox_routes::{MailboxRoute, resolve_index};
    let route = |login: &str, receives: &str| MailboxRoute {
        login: login.to_owned(),
        receives: vec![receives.to_owned()],
        ..MailboxRoute::default()
    };
    let routes = [
        route("own-box", &email("ann")),
        route("Hub-Box", "*@other.example.test"),
    ];
    assert_eq!(resolve_index(&routes, Some(&email("ann"))), Some(0));
    let lab = Lab {
        mailboxes: BTreeMap::from([(email("ann"), "hub-box".to_owned())]),
        ..Lab::default()
    };
    with_lab(lab, || {
        assert_eq!(mailbox_of(&email("ANN")).as_deref(), Some("hub-box"));
        assert_eq!(resolve_index(&routes, Some(&email("ann"))), Some(1));
        assert_eq!(resolve_index(&routes, Some(&email("bob"))), None);
    });
}

/// A place is held by the account its login file names, row or no row: the default home of a
/// Mac that lists no account is held by the login found there, a default row whose place holds
/// another account reports that one and not its own, and only a place nothing was read of is
/// held by its row.
/// NEGATIVE CONTROL: with `holdings` taking each place's row and never `place.seen` (the
/// loop over `roster.rows` it replaced), the stale default row reports its own account
/// (`left: [("ac-0000000000000bob", "borrow")] right: [("ac-0000000000000uni", "fill")]`) and
/// the Mac with no roster reports nothing (`left: [] right: [("ac-0000000000000uni", "fill")]`).
#[test]
fn holdings_follow_the_login_each_place_holds() {
    use crate::harness::journal::ObservedSrc;
    use crate::harness::observe::sweep::Place;
    use crate::harness::observe::{Observation, Observed};
    let keyed = |local: &str, preferred: &[(&str, Option<u64>)]| LabAccount {
        pk: format!("ac-{local:0>16}"),
        ..account(local, Cli::Claude, &format!("claude-{local}"), preferred)
    };
    let lab = Lab {
        me: Some(test_machine("m22")),
        accounts: vec![
            keyed("ann", &[("m22", Some(1))]),
            keyed("bob", &[("m3", Some(1))]),
            keyed("uni", &[]),
        ],
        ..Lab::default()
    };
    fn default_home<'a>(row: Option<&'a Row>, seen: Option<&str>) -> Place<'a> {
        Place {
            cli: Cli::Claude.into(),
            row,
            seen: seen.map_or(Observation::Unread, |local| {
                Observation::Seen(Observed {
                    cli: crate::harness::journal::ObservedCli::Pool(Cli::Claude),
                    region: None,
                    email: email(local),
                    key: None,
                    src: ObservedSrc::Profile,
                    path: PathBuf::from("/scratch/home/.claude.json"),
                    mtime: None,
                    plan: None,
                })
            }),
            fallback: PathBuf::from("/scratch/home/.claude.json"),
            src: ObservedSrc::Profile,
        }
    }
    let uni = [("ac-0000000000000uni".to_owned(), "fill")];
    let bob = [("ac-0000000000000bob".to_owned(), "borrow")];
    let listed = Roster {
        rows: vec![row("m22-row", None, "bob", Cli::Claude)],
    };
    let held = |roster: &Roster, seen: Option<&str>| {
        holdings(
            &lab,
            Cli::Claude,
            roster,
            &[default_home(roster.rows.first(), seen)],
        )
    };
    assert_eq!(held(&listed, Some("uni")), uni, "the stale default row");
    assert_eq!(held(&listed, None), bob, "unread: the belief");
    assert_eq!(held(&listed, Some("bob")), bob, "the row's own login");
    assert_eq!(held(&Roster::default(), Some("uni")), uni, "no roster");
    assert!(held(&Roster::default(), None).is_empty());
    assert!(
        held(&listed, Some("ann")).is_empty(),
        "an account this Mac prefers is no holding"
    );
    assert!(
        held(&listed, Some("zed")).is_empty(),
        "an address the lab does not know"
    );
    let unaddressed = Roster {
        rows: vec![Row {
            label: "plain".into(),
            ..Row::default()
        }],
    };
    assert_eq!(
        held(&unaddressed, Some("uni")),
        uni,
        "a row that names no address is held by the login found there"
    );
}

/// What this Mac runs that it does not prefer is reported by account key with the place row's
/// reason: `borrow` where another Mac prefers it, `fill` where nobody does and it may be signed
/// in, `stranded` where the lab bars it; a row this Mac prefers, a row the lab does not know and
/// a second row of one account report nothing, and a lab that names no machine for this Mac
/// reports nothing at all.
/// NEGATIVE CONTROL: with `holdings` not skipping an account this Mac prefers, ann is reported
/// (`left: 4 right: 3`); with the `borrow` arm reading `preferred` including this Mac, the
/// account nobody else prefers reads `borrow`.
#[test]
fn holdings_report_each_unpreferred_account_with_its_reason() {
    let keyed = |local: &str, preferred: &[(&str, Option<u64>)]| LabAccount {
        pk: format!("ac-{local:0>16}"),
        ..account(local, Cli::Claude, &format!("claude-{local}"), preferred)
    };
    let barred = LabAccount {
        joinable: false,
        ..keyed("cy", &[])
    };
    let lab = Lab {
        me: Some(test_machine("m22")),
        accounts: vec![
            keyed("ann", &[("m22", Some(1))]),
            keyed("bob", &[("m3", Some(1))]),
            keyed("dee", &[]),
            barred,
        ],
        ..Lab::default()
    };
    let roster = Roster {
        rows: vec![
            row("ann", None, "ann", Cli::Claude),
            row("bob", Some(Path::new("/scratch/bob")), "bob", Cli::Claude),
            row(
                "bob-2",
                Some(Path::new("/scratch/bob2")),
                "bob",
                Cli::Claude,
            ),
            row("dee", Some(Path::new("/scratch/dee")), "dee", Cli::Claude),
            row("cy", Some(Path::new("/scratch/cy")), "cy", Cli::Claude),
            row("eve", Some(Path::new("/scratch/eve")), "eve", Cli::Claude),
        ],
    };
    let held = holdings(
        &lab,
        Cli::Claude,
        &roster,
        &observe::sweep::places_of(Cli::Claude, &roster, None, &observe::sweep::OnToken::none()),
    );
    assert_eq!(
        held,
        [
            ("ac-0000000000000bob".to_owned(), "borrow"),
            ("ac-0000000000000dee".to_owned(), "fill"),
            ("ac-00000000000000cy".to_owned(), "stranded"),
        ]
    );
    let unbound = Lab {
        me: None,
        ..lab.clone()
    };
    assert!(
        holdings(
            &unbound,
            Cli::Claude,
            &roster,
            &observe::sweep::places_of(
                Cli::Claude,
                &roster,
                None,
                &observe::sweep::OnToken::none()
            )
        )
        .is_empty()
    );
    assert_eq!(
        lab.holders(&lab.accounts[1]),
        ["m3"],
        "no place rows: the preferences alone"
    );
    let mut placed = lab.clone();
    placed.places.by_account.insert(
        "ac-0000000000000bob".into(),
        vec![
            ("m22".into(), "borrow".into()),
            ("m3".into(), "fill".into()),
        ],
    );
    assert_eq!(placed.holders(&placed.accounts[1]), ["m3", "m22"]);
}

/// THE `alab` SOURCE NAMED WHERE `[alab] roster = false` TURNS IT OFF SAYS THAT, NOT THAT THE LAB
/// NAMES NO MACHINE, WHICH THE LAB MAY WELL DO. CONTROL: with the source on, and no lab held, the
/// refusal says no machine is named.
#[test]
fn the_lab_source_turned_off_says_it_is_off() {
    let dir = aterm_tempfile::TempDir::new().unwrap();
    let plain = dir.path().join("plain.toml");
    fs::write(&plain, "").unwrap();
    let off = "`[alab] roster = false` in aterm.toml turns the lab source off";
    let named = || match chosen(Cli::Claude, None, Some(SOURCE), Some(&plain)) {
        Err(why) => why,
        Ok(_) => panic!("no lab is held"),
    };
    assert!(while_turned_off(named).contains(off));
    let why = named();
    assert!(!why.contains(off), "CONTROL: {why}");
    assert!(why.contains("names no machine"), "CONTROL: {why}");
}

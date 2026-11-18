// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `FleetMachineAssignment`'s order, label and mail areas (docs/DESIGN-alab-lab-model-2026-11-01.md
//! §0) bound to the harness's lab code on scratch stores. Every address is synthetic
//! (`example.test`); nothing here reads or writes this Mac's real accounts, vault or registry.

use super::super::mailbox_routes::{MailboxRoute, resolve_index};
use super::*;
use crate::harness::accounts::rename_tests::NOW;
use crate::harness::alab_roster::test_machine;
use aterm_spec::derive::Model;
use aterm_spec::interp::State;
use std::collections::BTreeMap;

/// This Mac's machine label in every bed.
const ME: &str = "m1";
/// The account's lab label.
const LAB: &str = "claude-xxxxxx";
/// Its legacy label, which names a machine.
const LEGACY: &str = "m1-2";
/// A label the owner gave its row.
const OWNER: &str = "spare-x";

fn email(local: &str) -> String {
    format!("{local}@example.test")
}

/// Account `x`, preferred by the machines `preferred`.
fn account(preferred: &[&str]) -> LabAccount {
    LabAccount {
        pk: "ac-0000000000000001".to_owned(),
        cli: Cli::Claude,
        vendor: Vendor::default(),
        label: LAB.to_owned(),
        legacy_label: Some(LEGACY.to_owned()),
        email: email("x"),
        identity: None,
        browser: None,
        joinable: true,
        preferred: preferred
            .iter()
            .map(|machine| (test_machine(machine), Some(1)))
            .collect(),
    }
}

fn lab(accounts: Vec<LabAccount>) -> Lab {
    Lab {
        me: Some(test_machine(ME)),
        accounts,
        ..Lab::default()
    }
}

/// The slug the lab names the account labelled `label` by: never its label, as LabView labels a
/// pooled account with its reserved slug, which names nothing.
fn slug_of(label: &str) -> String {
    format!("{label}-s")
}

/// `lab` with each account named as the lab names it, its slug [`slug_of`] its label
/// ([`Lab::names`]): what [`asked_row`] resolves a word among.
fn named(mut lab: Lab) -> Lab {
    lab.names = lab
        .accounts
        .iter()
        .map(|account| {
            let names = aterm_file::AccountNames {
                realm: "claude".to_owned(),
                slug: slug_of(&account.label),
                name: aterm_db::typed::keys::account_name("claude", &account.email),
                email: Some(account.email.clone()),
                login: Some(account.email.clone()),
                live: true,
            };
            (account.pk.clone(), names)
        })
        .collect();
    lab
}

/// Account `x`'s row under `label`, at its own login directory or, as the default login's, at none.
fn row(label: &str, dirless: bool, class: Class) -> Row {
    Row {
        label: label.to_owned(),
        class,
        dir: (!dirless).then(|| crate::harness::host_path::abs(&format!("/scratch/{label}"))),
        email: Some(email("x")),
        ..Row::default()
    }
}

/// The reachable states of `model` in `area`.
fn area_states(model: &Model, area: i64) -> Vec<State> {
    aterm_spec::interp::reachable_states(model)
        .into_iter()
        .filter(|state| state["area"] == area)
        .collect()
}

/// Every action enabled at each of `states` steps, through `transition`, to a model successor.
fn conforms(model: &Model, states: &[State], transition: fn(&State, &str) -> State) {
    for state in states {
        for action in &model.actions {
            if !model.action_enabled(action.name, state) {
                continue;
            }
            let next = transition(state, action.name);
            assert!(
                model.successors(action.name, state).contains(&next),
                "{} from {state:?} steps to {next:?}",
                action.name
            );
        }
    }
}

/// A lab sync for this Mac, as the product runs it: [`fleet_for`], then
/// [`accounts_sync::compose_roster_with_logins`] (the sync's own composition: conflicting rows carried, the
/// build, [`retain`] under `lab`, the retained shared rows) on a scratch store with the default
/// login's address `default`.
fn sync(lab: &Lab, existing: &Roster, default: Option<&str>) -> Roster {
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(&root.path().join("state"));
    let fleet = fleet_for(lab, Cli::Claude, Some(existing)).expect("the lab names m1");
    let logins = store.load_logins(NOW).unwrap().0;
    accounts_sync::compose_roster_with_logins(
        &fleet,
        (test_machine(ME).key.as_str(), default),
        (&store, existing),
        None,
        Some(lab),
        &logins,
    )
    .expect("the sync composes")
}

/// The row of account `x` in `roster`, where it has one.
fn x_row(roster: &Roster) -> Option<&Row> {
    let mut rows = roster.rows.iter().filter(|r| r.matches_email(&email("x")));
    let found = rows.next();
    assert!(rows.next().is_none(), "two rows of x: {roster:?}");
    found
}

// 2. The order a preference gives.

/// The lab and roster an order-area `state` stands for: `x` preferred by this Mac where `here`
/// and by `m2` where `other`; a row `spare-x` of it, in the order `cls`, where `row`.
fn order_bed(state: &State) -> (Lab, Roster) {
    let mut preferred = Vec::new();
    if state["here"] == 1 {
        preferred.push(ME);
    }
    if state["other"] == 1 {
        preferred.push("m2");
    }
    let mut rows = Vec::new();
    if state["row"] == 1 {
        let class = if state["cls"] == 1 {
            Class::Preferred
        } else {
            Class::Pool
        };
        rows.push(row(OWNER, false, class));
    }
    (lab(vec![account(&preferred)]), Roster { rows })
}

/// `state` once `x` has `got` as its row: `row` and `cls` read back, and the ghosts set where
/// the row breaks the law each names.
/// `asked` says the row was asked for by name, which adds it whoever prefers it.
fn order_project(state: &State, got: Option<&Row>, asked: bool) -> State {
    let mut next = state.clone();
    let (was, here) = (state["row"] == 1, state["here"] == 1);
    let cls = got.map_or(0, |row| match row.class {
        Class::Preferred => 1,
        Class::Pool => 2,
    });
    if was && got.is_none() {
        next.insert("dropped", 1);
    }
    if !asked && !was && got.is_some() && !here {
        next.insert("unasked", 1);
    }
    if got.is_some() && (cls == 1) != here {
        next.insert("misordered", 1);
    }
    next.insert("row", i64::from(got.is_some()));
    next.insert("cls", cls);
    next
}

/// The order area's projection of `action`: `Sync` is the sync's composition, `Ask` is
/// [`asked_row`] under the lab the state stands for, and the owner's verbs set the bed.
pub(crate) fn order_transition(state: &State, action: &str) -> State {
    let (lab, existing) = order_bed(state);
    match action {
        "Sync" => order_project(state, x_row(&sync(&lab, &existing, None)), false),
        "Ask" => {
            let lab = named(lab);
            assert!(
                with_lab(lab.clone(), || asked_row(Cli::Claude, LAB)).is_none(),
                "the lab's label names nothing"
            );
            let asked = with_lab(lab, || asked_row(Cli::Claude, &slug_of(LAB)))
                .expect("the lab knows x")
                .expect("x may be asked for");
            order_project(state, Some(&asked), true)
        }
        "Unprefer" => {
            let mut next = state.clone();
            next.insert("here", 0);
            next
        }
        "PreferHere" | "PreferElsewhere" => {
            let mut next = state.clone();
            next.insert(
                if action == "PreferHere" {
                    "here"
                } else {
                    "other"
                },
                1,
            );
            next
        }
        other => panic!("{other} is no order action"),
    }
}

/// Every step a lab sync and an account asked for by name take is a `FleetMachineAssignment`
/// successor from every state its order area reaches (§0: a preference orders and never
/// limits): a row another Mac prefers stays, nothing another Mac prefers is added unasked, and a
/// row's order is this Mac's preference alone, whether or not another Mac prefers it too.
/// NEGATIVE CONTROLS: a roster that is the lab's fleet alone (a preference read as a limit) drops
/// the row another Mac prefers, breaking `KeptWherePreferredElsewhere`; a fleet that lists the
/// accounts it only knows (every joinable account) adds one another Mac prefers, breaking
/// `NothingUnasked`; neither is a `Sync` successor. Run against the code, `retain` ordering a row
/// `Preferred` only where this Mac alone prefers it (the multiplicity rule) makes the `Sync` from
/// an account two Macs prefer step to `misordered = 1`, no successor.
#[test]
fn the_lab_sync_orders_by_this_macs_preference_and_limits_nothing() {
    let model = aterm_models_harness::fleet_machine_assignment_model();
    let states = area_states(&model, 2);
    assert!(states.len() > 8, "{} order states", states.len());
    conforms(&model, &states, order_transition);
    let both = states
        .iter()
        .find(|s| s["here"] == 1 && s["other"] == 1 && s["row"] == 0)
        .expect("an account two Macs prefer");
    assert_eq!(order_transition(both, "Sync")["cls"], 1);

    let elsewhere = states
        .iter()
        .find(|s| s["row"] == 1 && s["here"] == 0 && s["other"] == 1)
        .expect("a row another Mac prefers");
    let (lab, existing) = order_bed(elsewhere);
    let fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    let limited = Roster {
        rows: fleet
            .accounts
            .iter()
            .map(|a| row(&a.label, false, a.class))
            .collect(),
    };
    let next = order_project(elsewhere, x_row(&limited), false);
    assert_eq!(next["dropped"], 1);
    assert!(!model.successors("Sync", elsewhere).contains(&next));

    let unrowed = states
        .iter()
        .find(|s| s["row"] == 0 && s["here"] == 0 && s["other"] == 1)
        .expect("an account only another Mac prefers");
    let (lab, existing) = order_bed(unrowed);
    let mut fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    fleet.accounts.extend(fleet.known.clone());
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(&root.path().join("state"));
    let built = accounts_sync::build_machine_roster(
        &fleet,
        test_machine(ME).key.as_str(),
        &store,
        Some(&existing),
        None,
    );
    let next = order_project(
        unrowed,
        x_row(&retain(
            built,
            &existing,
            Some(&lab),
            Cli::Claude,
            &test_machine(ME).key,
        )),
        false,
    );
    assert_eq!(next["unasked"], 1);
    assert!(!model.successors("Sync", unrowed).contains(&next));
}

// 3. Labels.

/// The label kind the label area counts `label` as: 1 a label shaped like a machine's (`m<N>…`),
/// 2 the lab's, 3 the owner's.
fn label_kind(label: &str) -> i64 {
    let machine = label
        .strip_prefix('m')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    match label {
        _ if machine => 1,
        LAB => 2,
        OWNER => 3,
        other => panic!("a label the bed never gives: {other}"),
    }
}

/// The lab and roster a label-area `state` stands for: `x` preferred by no Mac, its legacy label
/// `LEGACY`; its row under the label `lbl` names, dirless where it is the default login (`dflt`),
/// else at its own directory.
fn labels_bed(state: &State) -> (Lab, Roster) {
    let label = match state["lbl"] {
        0 => None,
        1 => Some(LEGACY),
        2 => Some(LAB),
        _ => Some(OWNER),
    };
    let rows = label
        .map(|label| row(label, state["dflt"] == 1, Class::Pool))
        .into_iter()
        .collect();
    (lab(vec![account(&[])]), Roster { rows })
}

/// `state` once a sync leaves `x`'s row as `got`: `lbl` read back, and the ghosts set where the
/// sync breaks the law each names.
fn labels_project(state: &State, got: Option<&Row>) -> State {
    let mut next = state.clone();
    let (was, lbl) = (state["lbl"], got.map_or(0, |row| label_kind(&row.label)));
    if was == 0 && lbl == 1 {
        next.insert("minted", 1);
    }
    if was == 1 && lbl == 2 {
        next.insert("resynced", 1);
    }
    next.insert("lbl", lbl);
    next
}

/// The label area's projection of `action`: `SyncLabels` is the sync's composition, with `x`'s
/// address as the default login's where `dflt`; the rows a Mac had and its default login set the
/// bed.
pub(crate) fn labels_transition(state: &State, action: &str) -> State {
    let (lab, existing) = labels_bed(state);
    let mut next = state.clone();
    match action {
        "SyncLabels" => {
            let default = (state["dflt"] == 1).then(|| email("x"));
            let synced = sync(&lab, &existing, default.as_deref());
            return labels_project(state, x_row(&synced));
        }
        "RowLegacy" => next.insert("lbl", 1),
        "RowOwner" => next.insert("lbl", 3),
        "DefaultLogin" => next.insert("dflt", 1),
        other => panic!("{other} is no label action"),
    };
    next
}

/// Every step a lab sync takes is a `FleetMachineAssignment` successor from every state its label
/// area reaches (§0: labels name no machine): the default login takes the lab's label, and a sync
/// renames no row, its legacy row and the default login's included.
/// NEGATIVE CONTROLS: the sheet path's fleet (`alab` cleared, so the default login is named by
/// `local_account`) names the default login after the machine, breaking `NoMachineLabelMinted`;
/// a sync that gives the legacy row its account's lab label, as the deleted rename did, breaks
/// `SyncNeverRenames`; neither is a successor.
#[test]
fn labels_come_from_the_lab_and_no_sync_renames_a_row() {
    let model = aterm_models_harness::fleet_machine_assignment_model();
    let states = area_states(&model, 3);
    // No row, a legacy one or the owner's, each with and without the default login, and the
    // default login's row the lab named.
    assert_eq!(states.len(), 7, "{states:?}");
    conforms(&model, &states, labels_transition);

    let unnamed = states
        .iter()
        .find(|s| s["lbl"] == 0 && s["dflt"] == 1)
        .expect("a default login with no row");
    assert_eq!(labels_transition(unnamed, "SyncLabels")["lbl"], 2);
    let (lab, existing) = labels_bed(unnamed);
    let root = aterm_tempfile::TempDir::new().unwrap();
    let store = Store::at(&root.path().join("state"));
    let mut fleet = fleet_for(&lab, Cli::Claude, Some(&existing)).unwrap();
    fleet.alab = None;
    for account in &mut fleet.accounts {
        account.machines = vec![ME.to_owned()];
    }
    // The sheet path reads a fleet that lists some row here; another account's stands in.
    fleet.accounts.push(FleetAccount {
        label: "claude-yyyyyy".to_owned(),
        machines: vec![ME.to_owned()],
        class: Class::Preferred,
        email: email("y"),
        browser: None,
        vendor: Vendor::default(),
    });
    let built =
        accounts_sync::build_machine_roster(&fleet, ME, &store, Some(&existing), Some(&email("x")));
    let next = labels_project(unnamed, x_row(&built));
    assert_eq!(next["minted"], 1, "{built:?}");
    assert!(!model.successors("SyncLabels", unnamed).contains(&next));

    for dflt in [0, 1] {
        let legacy = states
            .iter()
            .find(|s| s["lbl"] == 1 && s["dflt"] == dflt && s["resynced"] == 0)
            .expect("a legacy row");
        assert_eq!(labels_transition(legacy, "SyncLabels")["lbl"], 1);
        let (lab, existing) = labels_bed(legacy);
        let mut renamed = sync(&lab, &existing, (dflt == 1).then(|| email("x")).as_deref());
        for row in &mut renamed.rows {
            if row.label == LEGACY {
                row.label = LAB.to_owned();
            }
        }
        let next = labels_project(legacy, x_row(&renamed));
        assert_eq!(next["resynced"], 1);
        assert!(!model.successors("SyncLabels", legacy).contains(&next));
    }
}

// 4. Mail.

/// The routes and lab a mail-area `state` stands for, for the login `ann`. The lab is what
/// `aterm-db`'s `LabView` hands the harness for the chain the state's registry rows make (its
/// `Follow` is bound there, `alab::view::tests::an_alias_chain_takes_the_harness_models_steps`):
/// `Lab::aliases` lists `hub` where `t1` and then `root` (or `ann` itself round a cycle) where
/// `t2`, and `Lab::mailboxes` places `ann` in `lab-ann` where `eff` is 1 or `lab-root` where it
/// is 2, with a route of that login whose pattern names another domain. Routes name `ann`
/// where `own`, `hub` where `rh`, `root` where `rr`, and `*@example.test` where `dom`.
fn mail_bed(state: &State) -> (Vec<MailboxRoute>, Lab) {
    let route = |login: &str, receives: String| MailboxRoute {
        login: login.to_owned(),
        receives: vec![receives],
        ..MailboxRoute::default()
    };
    let mut routes = Vec::new();
    let mut lab = Lab::default();
    if state["dom"] == 1 {
        routes.push(route("domain-box", "*@example.test".to_owned()));
    }
    if state["rr"] == 1 {
        routes.push(route("root-box", email("root")));
    }
    if state["rh"] == 1 {
        routes.push(route("hub-box", email("hub")));
    }
    if state["own"] == 1 {
        routes.push(route("own-box", email("ann")));
    }
    let mut chain = Vec::new();
    if state["t1"] == 1 {
        chain.push(email("hub"));
    }
    if state["t2"] == 1 {
        chain.push(email(if state["hop"] == 1 { "root" } else { "ann" }));
    }
    if !chain.is_empty() {
        lab.aliases = BTreeMap::from([(email("ann"), chain)]);
    }
    let lab_box = match state["eff"] {
        0 => None,
        1 => Some("lab-ann"),
        _ => Some("lab-root"),
    };
    if let Some(login) = lab_box {
        routes.push(route(login, "*@other.example.test".to_owned()));
        lab.mailboxes = BTreeMap::from([(email("ann"), login.to_owned())]);
    }
    (routes, lab)
}

/// The `res` a resolution to `at` in `routes` reads as.
fn mail_res(routes: &[MailboxRoute], at: Option<usize>) -> i64 {
    match at.map(|at| routes[at].login.as_str()) {
        Some("lab-ann" | "lab-root") => 1,
        Some("own-box") => 2,
        Some("hub-box") => 3,
        Some("root-box") => 4,
        Some("domain-box") => 5,
        Some(other) => panic!("a route the bed never gives: {other}"),
        None => 6,
    }
}

/// The mail area's projection of `action`: `Resolve` is [`resolve_index`] for `ann` under the
/// lab the state stands for. The registry's rows, its read (`Follow`, bound in `aterm-db`) and
/// the routes are the bed this crate is handed, so the walk takes the model's one step for each.
pub(crate) fn mail_transition(state: &State, action: &str) -> State {
    if action == "Resolve" {
        let (routes, lab) = mail_bed(state);
        let at = with_lab(lab, || resolve_index(&routes, Some(&email("ann"))));
        let mut next = state.clone();
        next.insert("res", mail_res(&routes, at));
        return next;
    }
    let model = aterm_models_harness::fleet_machine_assignment_model();
    let [next] = &model.successors(action, state)[..] else {
        panic!("{action} is no deterministic mail input at {state:?}");
    };
    next.clone()
}

/// Every resolution a login's code fetch makes is a `FleetMachineAssignment` successor from every
/// state its mail area reaches (§0: an alias is linked with the address its auth login is
/// received at, and resolves to one mailbox): the lab's mailbox first, then the address's own
/// route, then the routes of the chain the lab hands over, nearest first, then a pattern.
/// NEGATIVE CONTROLS: a resolver that does not read the lab (no lab held) takes the address's
/// own route over the lab's mailbox, breaking `LabMailboxFirst`, and the domain route over the
/// chain's, breaking `LinkBeforePattern`; a lab whose chain is listed farthest first takes
/// root's route over hub's, breaking `NearestLinkFirst`. None is a `Resolve` successor. Run
/// against the code, `resolve_index` without its `mailbox_of` arm fails the walk the same way.
#[test]
fn an_alias_resolves_to_one_effective_mailbox() {
    let model = aterm_models_harness::fleet_machine_assignment_model();
    let states = area_states(&model, 4);
    assert!(states.len() > 1000, "{} mail states", states.len());
    conforms(&model, &states, mail_transition);

    let unresolved = |want: &[(&str, i64)]| {
        states
            .iter()
            .find(|s| s["fol"] == 1 && s["res"] == 0 && want.iter().all(|(k, v)| s[*k] == *v))
            .expect("the unresolved bed")
            .clone()
    };
    let refused = |state: &State, lab: Option<Lab>, law: &str| {
        let (routes, _) = mail_bed(state);
        let resolve = || resolve_index(&routes, Some(&email("ann")));
        let at = match lab {
            Some(lab) => with_lab(lab, resolve),
            None => resolve(),
        };
        let mut next = state.clone();
        next.insert("res", mail_res(&routes, at));
        assert!(!model.check_invariant(law, &next), "{law}: {next:?}");
        assert!(!model.successors("Resolve", state).contains(&next));
    };
    refused(
        &unresolved(&[("eff", 1), ("own", 1)]),
        None,
        "LabMailboxFirst",
    );
    refused(
        &unresolved(&[("eff", 0), ("own", 0), ("t1", 1), ("rh", 1), ("dom", 1)]),
        None,
        "LinkBeforePattern",
    );
    let both = unresolved(&[
        ("eff", 0),
        ("own", 0),
        ("hop", 1),
        ("t1", 1),
        ("t2", 1),
        ("rh", 1),
        ("rr", 1),
    ]);
    let mut reversed = mail_bed(&both).1;
    for chain in reversed.aliases.values_mut() {
        chain.reverse();
    }
    refused(&both, Some(reversed), "NearestLinkFirst");
}

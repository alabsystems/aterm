// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The generated exports over hand-built labs; every address is synthetic (`example.test`), and
//! nothing here reads or writes a vault, `~/.secrets` or a registry.

use super::*;
use crate::harness::alab_roster::Placements;

fn email(local: &str) -> String {
    format!("{local}@example.test")
}

fn account(local: &str, cli: Cli, label: &str, preferred: &[(&str, Option<u64>)]) -> LabAccount {
    LabAccount {
        pk: format!("ac-{local:0>16}"),
        cli,
        vendor: Vendor::default(),
        label: label.to_owned(),
        legacy_label: None,
        email: email(local),
        identity: None,
        browser: None,
        joinable: true,
        preferred: preferred
            .iter()
            .map(|(machine, rank)| ((*machine).to_owned(), *rank))
            .collect(),
    }
}

fn file<'a>(files: &'a [Export], name: &str) -> &'a str {
    &files.iter().find(|f| f.name == name).unwrap().body
}

fn machines_of(fleet: &FleetRoster, local: &str) -> Vec<String> {
    fleet
        .accounts
        .iter()
        .find(|a| a.email == email(local))
        .map(|a| a.machines.clone())
        .unwrap_or_default()
}

/// The lab of the export tests: m8 and m12 are older builds that read the files. ann is preferred
/// by m5 (rank 2) and m3 (rank 1) and held by m7 and m9 by place rows; bob is preferred by nobody
/// and m3 holds it; cy is paused and m5 prefers it.
fn lab() -> Lab {
    let ann = account(
        "ann",
        Cli::Claude,
        "claude-aaaaaa",
        &[("m3", Some(1)), ("m5", Some(2))],
    );
    let bob = account("bob", Cli::Claude, "claude-bbbbbb", &[]);
    let cy = LabAccount {
        joinable: false,
        ..account("cy", Cli::Claude, "claude-cccccc", &[("m5", Some(1))])
    };
    let dee = LabAccount {
        legacy_label: Some("m8-2".into()),
        ..account("dee", Cli::Codex, "codex-dddddd", &[("m3", None)])
    };
    let mut by_account = BTreeMap::new();
    by_account.insert(
        ann.pk.clone(),
        vec![
            ("m7".to_owned(), "borrow".to_owned()),
            ("m9".to_owned(), "probation".to_owned()),
        ],
    );
    by_account.insert(bob.pk.clone(), vec![("m3".to_owned(), "fill".to_owned())]);
    Lab {
        me: Some("m3".into()),
        settled: true,
        accounts: vec![ann, bob, cy, dee],
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        #[cfg(unix)]
        machine_name: None,
        places: Placements { by_account },
    }
}

/// Each exported row lists every Mac that holds its account: the preferring ones by rank, then
/// those whose place rows say they run it, then each Mac the current file gives it (m8, an older
/// build); no Mac the current file lists is dropped, since the lab cannot tell which Macs read it,
/// a row the lab knows no account for keeps its Macs, a paused account gains no Mac from a
/// preference, a row keeps the label the file gives it, `ai-pool.toml`'s warm list stops at three
/// with each slot's reason, and every file reads back through the reader an older build runs.
/// Rendering again over its own output changes nothing.
/// NEGATIVE CONTROL: with `machines` not carrying the listed Macs, ann's row loses m8
/// (`left: ["m3", "m5", "m7", "m9"] right: [..., "m8"]`); with `Lab::holders` reading preferences
/// only, bob is not exported (`left: [] right: ["m3"]`); with `machines` taking a paused
/// account's preferences, cy is exported for m5.
#[test]
fn an_export_lists_every_mac_that_holds_an_account() {
    let lab = lab();
    let current_fleet = FleetRoster {
        source: "fleet".into(),
        accounts: vec![
            FleetAccount {
                label: "m3-1".into(),
                machines: vec!["m8".into(), "m5".into()],
                class: Class::Pool,
                email: email("ann"),
                browser: None,
                vendor: Vendor::default(),
            },
            FleetAccount {
                label: "m5".into(),
                machines: vec!["m5".into()],
                class: Class::Preferred,
                email: email("gone"),
                browser: None,
                vendor: Vendor::default(),
            },
            FleetAccount {
                label: "m8".into(),
                machines: vec!["m8".into(), "m5".into()],
                class: Class::Pool,
                email: email("old"),
                browser: None,
                vendor: Vendor::default(),
            },
        ],
        ..FleetRoster::default()
    };
    let current: BTreeMap<String, String> = [(
        accounts_sync::FLEET_ROSTER_FILE.to_owned(),
        current_fleet.to_toml(),
    )]
    .into();
    let files = render(&lab, &current).unwrap();
    assert_eq!(
        files.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
        names()
    );
    let fleet = FleetRoster::from_toml(file(&files, "fleet-accounts.toml"), "export").unwrap();
    assert_eq!(machines_of(&fleet, "ann"), ["m3", "m5", "m7", "m9", "m8"]);
    assert_eq!(machines_of(&fleet, "bob"), ["m3"], "a place row exports");
    assert!(
        machines_of(&fleet, "cy").is_empty(),
        "paused: no preference adds a Mac"
    );
    assert_eq!(
        machines_of(&fleet, "gone"),
        ["m5"],
        "a listed Mac stays: the lab cannot tell it reads the lab"
    );
    assert_eq!(
        machines_of(&fleet, "old"),
        ["m8", "m5"],
        "an older build keeps its row"
    );
    let ann = fleet
        .accounts
        .iter()
        .find(|a| a.email == email("ann"))
        .unwrap();
    assert_eq!(ann.label, "m3-1", "the label older Macs call the row");
    assert_eq!(ann.class, Class::Pool);
    let bob = fleet
        .accounts
        .iter()
        .find(|a| a.email == email("bob"))
        .unwrap();
    assert_eq!(bob.label, "claude-bbbbbb");

    let codex = FleetRoster::from_toml_for(
        Cli::Codex,
        file(&files, &accounts_sync::cli_fleet_file(Cli::Codex)),
        "export",
    )
    .unwrap();
    assert_eq!(codex.accounts.len(), 1);
    assert_eq!(codex.accounts[0].label, "m8-2", "the legacy label");

    let pool = Pool::from_toml(file(&files, ai_pool::AI_POOL_FILE), "export").unwrap();
    let ann = pool
        .accounts
        .iter()
        .find(|a| a.email == email("ann"))
        .unwrap();
    assert_eq!(ann.warm, ["m3", "m5", "m7"], "three, by rank then place");
    assert_eq!(ann.why, [SlotWhy::Pin, SlotWhy::Pin, SlotWhy::Borrow]);
    let bob = pool
        .accounts
        .iter()
        .find(|a| a.email == email("bob"))
        .unwrap();
    assert_eq!(bob.why, [SlotWhy::Fill]);

    let again: BTreeMap<String, String> = files
        .iter()
        .map(|f| (f.name.clone(), f.body.clone()))
        .collect();
    assert_eq!(render(&lab, &again).unwrap(), files, "a fixed point");
}

/// A current file that is there and does not parse renders nothing over it, since what it gives
/// the Macs the lab cannot describe is unknown; with no current files, an empty lab still renders
/// every file, each one its reader takes.
/// NEGATIVE CONTROL: with `render` treating a file that does not parse as absent, the broken
/// pool renders (`is_err`).
#[test]
fn an_unreadable_current_file_refuses_the_render() {
    let lab = lab();
    let broken: BTreeMap<String, String> =
        [(ai_pool::AI_POOL_FILE.to_owned(), "[[account\n".to_owned())].into();
    assert!(render(&lab, &broken).is_err());
    let files = render(&Lab::default(), &BTreeMap::new()).unwrap();
    assert_eq!(files.len(), names().len());
    for cli in Cli::ALL {
        let name = fleet_file(cli);
        let fleet = FleetRoster::from_toml_for(cli, file(&files, &name), &name).unwrap();
        assert!(fleet.accounts.is_empty());
    }
    assert!(
        Pool::from_toml(file(&files, ai_pool::AI_POOL_FILE), "export")
            .unwrap()
            .accounts
            .is_empty()
    );
}

/// The pool's warm list keeps every Mac the current pool lists before any the lab adds, since an
/// older Mac reads the pool ahead of every other file and a slot it loses signs it out: ann's
/// current warm list names m12 (an older build) and m3, so the cut keeps both and adds m5, not the
/// place-row Macs. A row the lab knows no account for keeps its label and Macs, and a lab account
/// whose legacy label that row holds takes its lab label instead.
/// NEGATIVE CONTROL: with `warm` putting the lab's holders first, m12 is cut
/// (`left: ["m3", "m5", "m7"] right: ["m12", "m3", "m5"]`); with `fleet` not reserving the
/// unknown rows' labels for them, dee takes the older row's `m8-2` (`left: "m8-2" right:
/// "codex-dddddd"`).
#[test]
fn an_export_never_cuts_a_mac_an_older_file_lists() {
    let lab = lab();
    let warm_row = |local: &str, provider: Cli, label: &str, warm: &[&str]| PoolAccount {
        provider,
        vendor: Vendor::default(),
        label: Some(label.to_owned()),
        email: email(local),
        browser: None,
        warm: warm.iter().map(|m| (*m).to_owned()).collect(),
        since: Vec::new(),
        why: warm.iter().map(|_| SlotWhy::Pin).collect(),
        used: Vec::new(),
    };
    let current_pool = Pool {
        source: "pool".into(),
        last_heard: BTreeMap::new(),
        accounts: vec![
            warm_row("ann", Cli::Claude, "claude-aaaaaa", &["m12", "m3"]),
            warm_row("ex", Cli::Codex, "m8-2", &["m12"]),
        ],
    };
    let current_codex = FleetRoster {
        source: "fleet".into(),
        accounts: vec![FleetAccount {
            label: "m8-2".into(),
            machines: vec!["m12".into()],
            class: Class::Preferred,
            email: email("ex"),
            browser: None,
            vendor: Vendor::default(),
        }],
        ..FleetRoster::default()
    };
    let current: BTreeMap<String, String> = [
        (ai_pool::AI_POOL_FILE.to_owned(), current_pool.to_toml()),
        (
            accounts_sync::cli_fleet_file(Cli::Codex),
            current_codex.to_toml(),
        ),
    ]
    .into();
    let files = render(&lab, &current).unwrap();
    let pool = Pool::from_toml(file(&files, ai_pool::AI_POOL_FILE), "export").unwrap();
    let row = |local: &str| {
        pool.accounts
            .iter()
            .find(|a| a.email == email(local))
            .unwrap()
            .clone()
    };
    assert_eq!(row("ann").warm, ["m12", "m3", "m5"]);
    assert_eq!(row("ann").why, [SlotWhy::Pin, SlotWhy::Pin, SlotWhy::Pin]);
    assert_eq!(row("ex").warm, ["m12"]);
    assert_eq!(row("ex").label.as_deref(), Some("m8-2"));
    assert_eq!(row("dee").label.as_deref(), Some("codex-dddddd"));
    let codex = FleetRoster::from_toml_for(
        Cli::Codex,
        file(&files, &accounts_sync::cli_fleet_file(Cli::Codex)),
        "export",
    )
    .unwrap();
    assert_eq!(machines_of(&codex, "ex"), ["m12"], "the unknown row stays");
    let dee = codex
        .accounts
        .iter()
        .find(|a| a.email == email("dee"))
        .unwrap();
    assert_eq!(dee.label, "codex-dddddd", "m8-2 is the older row's");
    let again: BTreeMap<String, String> = files
        .iter()
        .map(|f| (f.name.clone(), f.body.clone()))
        .collect();
    assert_eq!(render(&lab, &again).unwrap(), files, "a fixed point");
}

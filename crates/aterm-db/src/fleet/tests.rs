// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet tables' laws: every registry row is a row, in the owner's order; a value nobody
//! read is never a zero; this Mac and an unregistered answer are rows; no agents row carries an
//! address; and where an account is expected comes from the registry's preferences alone.

use super::command::{print, print_schema, shown, sources_line};
use super::fixture;
use super::fold::{AGENTS_KEEP_MS, FleetTables, Inputs, REACH_KEEP_MS, Source, fold};
use super::read::{
    AccountWord, AgentSeen, Answer, Answered, At, Harness, Latest, MachineSeen, OWN_ID, Read,
    SIGHTED_KEEP_MS, State, Tally, word,
};
use super::rows::{
    self, AccountRef, Cell, EvidenceHow, Reach, Row, UNREGISTERED, Via, WindowState,
};
use super::{
    Folder, HELD_FLEET_UNREAD, HELD_NO_HISTORY, HELD_PARTIAL, HELD_READ_FAILED, HELD_STALE,
    HELD_UNOBSERVED, Reading,
};
use crate::alab::journal::event::SIGNIN_IN;
use crate::alab::journal::place::{Placed, Why};
use crate::alab::journal::seen::{Claimed, Seen, SignedIn};
use crate::alab::view::{Account, LabView, Machine, MachineNickname, Preference};
use crate::typed::{cell, grid, keys};
use aterm_json::Value;
use aterm_types::network_state::{
    HistoryState, NetworkObservation, NetworkStateSnapshot, Visibility,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The instant every test folds at, Unix milliseconds.
const NOW: u64 = 1_800_000_000_000;
/// A Commons uuid of a test's own.
const UUID: &str = "000102030405060708090a0b0c0d0e0f";

/// The machines columns that hold what a machine was observed to do: unread unless read.
const OBSERVED: &[&str] = &[
    "sessions",
    "agents",
    "busy",
    "idle",
    "waiting",
    "walled",
    "stuck",
    "unknown",
    "agents_listed",
    "agents_at",
    "disk_free_mb",
];
/// The machines columns of the measures nothing carries yet. Memory's capacity is not among
/// them: the registry carries it ([`MEM_CAPACITY`]).
const MEASURES: &[&str] = &[
    "cpu_used_x100",
    "cpu_cores",
    "cpu_at",
    "mem_used_mb",
    "gpu_used_x100",
    "gpu_cores",
    "gpu_at",
    "disk_used_mb",
    "disk_total_mb",
    "disk_at",
    "net_used_kbps",
    "net_link_kbps",
    "net_at",
];

/// The machines columns of the memory pair's capacity: read where the registry has `ram_gb`.
const MEM_CAPACITY: &[&str] = &["mem_total_mb", "mem_at"];

fn machine(label: &str) -> Machine {
    Machine {
        pk: keys::machine_pk(label),
        label: Some(label.to_owned()),
        nickname: MachineNickname::Unset,
        aliases: Vec::new(),
        status: Some("active".to_owned()),
        os: None,
        chip: None,
        ram_gb: None,
        live: true,
    }
}

fn lab_of(labels: &[&str]) -> LabView {
    let mut lab = LabView::default();
    for label in labels {
        let machine = machine(label);
        lab.machines.insert(machine.pk.clone(), machine);
    }
    lab
}

fn account(realm: &str, address: &str, label: &str) -> Account {
    Account {
        pk: keys::account_pk(realm, address).expect("a test address is an address"),
        provider: keys::provider_of_realm(realm).map(str::to_owned),
        realm: Some(realm.to_owned()),
        label: Some(label.to_owned()),
        legacy_label: None,
        login: Some(address.to_owned()),
        identity: None,
        status: Some("active".to_owned()),
        plan: None,
        region: None,
        flow: None,
        browser: None,
        live: true,
    }
}

fn live<T>(value: T, at_ms: u64) -> Source<Arc<T>> {
    Source::Live {
        value: Arc::new(value),
        at_ms,
    }
}

fn agent(sid: &str, harness: Harness, state: State, account: Option<&str>) -> AgentSeen {
    AgentSeen {
        sid: sid.to_owned(),
        harness,
        state,
        state_why: None,
        state_age_ms: None,
        task: None,
        ctx_left_pct: None,
        usage_5h_pct: None,
        usage_7d_pct: None,
        account: account.and_then(AccountWord::new),
        model: None,
    }
}

/// An answer of `sessions` sessions listing `agents`, counted.
fn answered(sessions: u32, agents: Vec<AgentSeen>) -> Answer {
    let mut tally = Tally::default();
    for agent in &agents {
        tally.add(agent.state);
    }
    Answer::Answered(Answered {
        build: Some("0.141.0".to_owned()),
        home_free_mb: Some(50_000),
        sessions: Some(sessions),
        tally: Some(tally),
        agents,
        agents_more: 0,
    })
}

fn peer(label: &str, answer: Answer) -> MachineSeen {
    MachineSeen::peer(
        &format!("{label}-test.local"),
        &format!("{label}-test"),
        Some(label),
        answer,
    )
}

/// One read made a second before [`NOW`].
fn read_of(machines: Vec<MachineSeen>) -> Source<Arc<Latest>> {
    let at = NOW - 1_000;
    live(Latest::default().with(whole(at, machines)), at)
}

/// A read at `at` that asked the fleet and named `machines`.
fn whole(at: u64, machines: Vec<MachineSeen>) -> Read {
    Read {
        read_at_ms: at,
        machines,
        fleet: true,
    }
}

/// A read at `at` whose question of the fleet was refused: this Mac's `own` answer alone.
fn own_alone(at: u64, own: Answer) -> Read {
    Read {
        read_at_ms: at,
        machines: vec![MachineSeen::own(None, own)],
        fleet: false,
    }
}

fn tables(inputs: &Inputs) -> FleetTables {
    fold(inputs, NOW).tables
}

fn fixture_tables() -> (Inputs, FleetTables) {
    let inputs = fixture::inputs(NOW);
    let tables = tables(&inputs);
    (inputs, tables)
}

fn cell_of<R: Row>(row: &R, column: &str) -> Cell {
    let at = R::SCHEMA
        .position(column)
        .unwrap_or_else(|| panic!("{}: no column {column}", R::SCHEMA.name));
    row.cells().swap_remove(at)
}

fn shown_of<R: Row>(row: &R, column: &str) -> String {
    let at = R::SCHEMA.position(column).expect("a column of the schema");
    shown(&R::SCHEMA.columns[at].column.ty, &row.cells()[at])
}

/// A printed TSV table as its lines' cells, the header first.
fn grid_of(text: &str) -> Vec<Vec<&str>> {
    text.lines()
        .map(|line| line.split('\t').collect())
        .collect()
}

/// A printed JSON table's rows.
fn json_rows(text: &str) -> Vec<Value> {
    let value: Value = aterm_json::from_str(text).expect("the table is JSON");
    value
        .get("rows")
        .and_then(Value::as_array)
        .expect("a table has rows")
        .clone()
}

/// How many rows of `relation` the filters `specs` keep, as the TSV prints them.
fn kept(tables: &FleetTables, relation: &str, specs: &[&str]) -> usize {
    let filters: Vec<grid::Where> = specs
        .iter()
        .map(|spec| grid::Where::parse(spec).expect("a filter"))
        .collect();
    let printed = print(tables, relation, &filters, false).expect("the relation prints");
    printed.text.lines().count() - 1
}

fn labels(tables: &FleetTables) -> Vec<String> {
    tables
        .machines
        .iter()
        .map(|row| row.label.clone().unwrap_or_else(|| "-".to_owned()))
        .collect()
}

#[test]
fn every_registry_machine_and_account_is_a_row_in_order_and_nothing_unread_is_zero() {
    let (inputs, tables) = fixture_tables();
    let lab = inputs.lab.value().expect("the fixture holds a registry");

    // Every registry machine is one row, in the order of its number, and the one answer the
    // registry does not name comes after them all.
    assert_eq!(lab.machines.len(), fixture::MACHINES);
    assert_eq!(tables.machines.len(), fixture::MACHINES + 1);
    let (registry, strangers) = tables.machines.split_at(fixture::MACHINES);
    let keys: BTreeSet<&str> = registry.iter().map(|row| row.machine.as_str()).collect();
    assert_eq!(
        keys,
        lab.machines.keys().map(String::as_str).collect(),
        "each registry machine is exactly one row"
    );
    let numbers: Vec<u32> = registry
        .iter()
        .map(|row| row.number.expect("a labelled machine has a number"))
        .collect();
    assert!(
        numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "machines come in numeric order: {numbers:?}"
    );
    assert_eq!((numbers[0], numbers[fixture::MACHINES - 1]), (10, 295));
    assert_eq!(strangers[0].status, Some(UNREGISTERED));
    assert_eq!(
        strangers[0].label.as_deref(),
        Some(fixture::UNREGISTERED_LABEL)
    );
    let status = |word: &str| {
        registry
            .iter()
            .filter(|row| row.status == Some(word))
            .count()
    };
    assert_eq!(
        (status("active"), status("planned")),
        (
            fixture::MACHINES_ACTIVE,
            fixture::MACHINES - fixture::MACHINES_ACTIVE
        )
    );

    // Every registry account is one row, by provider then label.
    assert_eq!(tables.accounts.len(), fixture::ACCOUNTS);
    let keys: BTreeSet<&str> = tables
        .accounts
        .iter()
        .map(|row| row.account.as_str())
        .collect();
    assert_eq!(keys, lab.accounts.keys().map(String::as_str).collect());
    assert!(
        tables
            .accounts
            .windows(2)
            .all(|pair| (&pair[0].provider, &pair[0].slug) < (&pair[1].provider, &pair[1].slug)),
        "accounts come by provider, then label"
    );

    // Every sighted agent is one row, keyed by its machine and session.
    assert_eq!(tables.agents.len(), fixture::AGENTS);
    let keys: BTreeSet<(&str, &str)> = tables
        .agents
        .iter()
        .map(|row| (row.machine.as_str(), row.sid.as_str()))
        .collect();
    assert_eq!(keys.len(), fixture::AGENTS);

    // Nothing unread is zero: a machine no read heard has no figure in any observed column, in
    // the row, in the TSV and in the JSON; and no machine has a measure nothing carries.
    let tsv = print(&tables, "machines", &[], false).expect("machines print");
    let grid = grid_of(&tsv.text);
    let json = json_rows(
        &print(&tables, "machines", &[], true)
            .expect("machines print")
            .text,
    );
    assert_eq!(grid.len(), tables.machines.len() + 1);
    assert_eq!(json.len(), tables.machines.len());
    let mut unheard = 0;
    for (n, row) in tables.machines.iter().enumerate() {
        let heard = row.sessions.is_some();
        unheard += usize::from(!heard);
        let unread = OBSERVED.iter().filter(|_| !heard).chain(MEASURES).copied();
        for column in unread {
            let at = rows::MACHINES.position(column).expect("a machines column");
            assert_eq!(row.cells()[at], Cell::Unread, "{column} of row {n}");
            assert_eq!(grid[n + 1][at], "?", "{column} of row {n} prints unread");
            assert_eq!(
                json[n].get(column),
                Some(&Value::Null),
                "{column} of row {n} is null"
            );
        }
        // Capacity the registry carries is read, whether or not the machine was ever heard, in
        // the measure's own unit; where the registry has none it is unread, not zero.
        let total = rows::MACHINES.position("mem_total_mb").expect("a column");
        match row.ram_gb {
            Some(gb) => assert_eq!(grid[n + 1][total], (gb * 1_024).to_string()),
            None => assert_eq!(grid[n + 1][total], "?"),
        }
    }
    assert!(
        tables
            .machines
            .iter()
            .any(|row| row.ram_gb.is_some() && row.sessions.is_none()),
        "the fixture has capacity on machines nothing heard"
    );
    assert_eq!(
        unheard,
        fixture::MACHINES - fixture::MACHINES_ANSWERING,
        "most of the lab was never heard, and none of it reads as idle"
    );
    let unread: Vec<_> = tables
        .accounts
        .iter()
        .filter(|row| row.w5h.state == WindowState::Unread)
        .collect();
    assert_eq!(unread.len(), fixture::ACCOUNTS - fixture::ACCOUNTS_READ);
    for row in unread {
        for column in ["w5h_used_x10", "w5h_reset", "w5h_at", "walled", "last_use"] {
            assert_eq!(cell_of(row, column), Cell::Unread, "{column}");
            assert_eq!(shown_of(row, column), "?");
        }
    }

    // Control: the row count follows the registry, so it is the registry the rows come from.
    let gone = registry
        .iter()
        .find(|row| row.status == Some("planned"))
        .expect("the fixture plans machines")
        .machine
        .clone();
    let mut fewer = LabView::clone(lab);
    fewer.machines.remove(&gone);
    let mut without = inputs.clone();
    without.lab = live(fewer, NOW);
    let without = self::tables(&without);
    assert_eq!(without.machines.len(), fixture::MACHINES);
    assert!(without.machines.iter().all(|row| row.machine != gone));
}

#[test]
fn a_value_nobody_read_prints_unread_and_a_counted_zero_prints_zero() {
    // A registry and nothing else: its machine was never heard.
    let mut inputs = Inputs {
        lab: live(lab_of(&["m3"]), NOW),
        ..Inputs::default()
    };
    let unheard = tables(&inputs);
    let row = &unheard.machines[0];
    for column in
        OBSERVED
            .iter()
            .chain(MEASURES)
            .chain(MEM_CAPACITY)
            .chain(&["build", "last_seen", "proven"])
    {
        assert_eq!(cell_of(row, column), Cell::Unread, "{column}");
        assert_eq!(shown_of(row, column), "?", "{column}");
    }
    assert_eq!(shown_of(row, "reach"), "unread");
    // With no evidence source read, how many accounts run there is unread too.
    assert_eq!(cell_of(row, "accounts_active"), Cell::Unread);
    let json = print(&unheard, "machines", &[], true)
        .expect("machines print")
        .text;
    assert!(json.contains("\"sessions\":null"), "{json}");
    assert!(json.contains("\"cpu_cores\":null"), "{json}");
    // The registry read its preferences and found none: that zero was counted.
    assert_eq!(shown_of(row, "accounts_expected"), "0");

    // Control: the same machine answering that it runs nothing prints its zeros.
    inputs.read = read_of(vec![peer("m3", answered(0, Vec::new()))]);
    let heard = tables(&inputs);
    let row = &heard.machines[0];
    for column in ["sessions", "agents", "busy", "idle", "agents_listed"] {
        assert_eq!(shown_of(row, column), "0", "{column}");
    }
    assert_eq!(shown_of(row, "disk_free_mb"), "50000");
    for column in MEASURES.iter().chain(MEM_CAPACITY) {
        assert_eq!(shown_of(row, column), "?", "{column} is still unread");
    }

    // Capacity is read apart from use: the registry's memory is the pair's capacity, in MiB and
    // dated by the registry's read, while the memory in use stays unread.
    let mut sized = machine("m3");
    sized.ram_gb = Some(128);
    let mut lab = LabView::default();
    lab.machines.insert(sized.pk.clone(), sized);
    let sized = tables(&Inputs {
        lab: live(lab, NOW - 7),
        ..Inputs::default()
    });
    let row = &sized.machines[0];
    assert_eq!(shown_of(row, "ram_gb"), "128");
    assert_eq!(shown_of(row, "mem_total_mb"), "131072");
    assert_eq!(cell_of(row, "mem_at"), Cell::Value(Value::from(NOW - 7)));
    assert_eq!(shown_of(row, "mem_used_mb"), "?");
    let json = print(&heard, "machines", &[], true)
        .expect("machines print")
        .text;
    assert!(json.contains("\"sessions\":0"), "{json}");
}

#[test]
fn a_reading_has_no_default() {
    // `At<T>` implements `Default` for no `T`: were it to, both impls below would apply and
    // this would not compile. A reading is made only with a value and a time in hand.
    trait NoDefault<Marker> {
        fn holds() {}
    }
    impl<T: ?Sized> NoDefault<()> for T {}
    struct WouldBeAmbiguous;
    impl<T: Default> NoDefault<WouldBeAmbiguous> for T {}
    <At<u64> as NoDefault<_>>::holds();
    <At<String> as NoDefault<_>>::holds();
    <At<Tally> as NoDefault<_>>::holds();
    assert_eq!(At::new(7_u64, NOW).map(|v| v + 1), At::new(8, NOW));
}

#[test]
fn machines_come_in_numeric_order_whatever_order_they_were_offered_in() {
    let mut lab = lab_of(&["m104", "m22", "m3"]);
    let mut unlabelled = machine("m9");
    unlabelled.label = None;
    lab.machines.insert(unlabelled.pk.clone(), unlabelled);
    let sightings = || {
        vec![
            peer("m104", answered(1, Vec::new())),
            peer("m3", answered(2, Vec::new())),
            peer("m22", Answer::Silent { why: None }),
            peer("m7", answered(3, Vec::new())),
            peer("m5", answered(4, Vec::new())),
        ]
    };
    let forward = tables(&Inputs {
        lab: live(lab.clone(), NOW),
        read: read_of(sightings()),
        ..Inputs::default()
    });
    assert_eq!(
        labels(&forward),
        ["m3", "m22", "m104", "-", "m5", "m7"],
        "by number, an unnumbered row after, unregistered answers last and in order too"
    );
    // Control: the text order of the same labels is another order.
    let mut by_text = labels(&forward)[..3].to_vec();
    by_text.sort();
    assert_eq!(by_text, ["m104", "m22", "m3"]);

    let mut reversed = sightings();
    reversed.reverse();
    let backward = tables(&Inputs {
        lab: live(lab, NOW),
        read: read_of(reversed),
        ..Inputs::default()
    });
    assert_eq!(
        forward, backward,
        "the order a read names machines in changes nothing"
    );
}

#[test]
fn this_mac_is_a_row() {
    let own = || {
        MachineSeen::own(
            None,
            answered(4, vec![agent("s1", Harness::Claude, State::Busy, None)]),
        )
    };

    // With a registry that binds this Mac, its own answer is that machine's row.
    let mut lab = lab_of(&["m3", "m22"]);
    lab.me = Some(keys::machine_pk("m22"));
    let named = tables(&Inputs {
        lab: live(lab.clone(), NOW),
        read: read_of(vec![own()]),
        ..Inputs::default()
    });
    assert_eq!(named.machines.len(), 2);
    let me = &named.machines[1];
    assert_eq!(me.label.as_deref(), Some("m22"));
    assert_eq!((me.how, me.reach), (Via::Own, Reach::Up));
    assert_eq!(me.sessions, Some(At::new(4, NOW - 1_000)));
    assert_eq!(shown_of(me, "how"), "self");
    assert_eq!(named.agents.len(), 1);
    assert_eq!(named.agents[0].machine, me.machine);
    // Control: the other machine, which nothing heard, is not up.
    assert_eq!(
        (named.machines[0].how, named.machines[0].reach),
        (Via::None, Reach::Unread)
    );

    // With a registry that does not name it, it is still a row, and says so.
    lab.me = None;
    let stranger = tables(&Inputs {
        lab: live(lab, NOW),
        read: read_of(vec![own()]),
        ..Inputs::default()
    });
    assert_eq!(stranger.machines.len(), 3);
    let me = &stranger.machines[2];
    assert_eq!(
        (me.machine.as_str(), me.status),
        (OWN_ID, Some(UNREGISTERED))
    );

    // With no registry read at all, it is a row whose status nobody read.
    let alone = tables(&Inputs {
        read: read_of(vec![own()]),
        ..Inputs::default()
    });
    assert_eq!(alone.machines.len(), 1);
    assert_eq!(alone.machines[0].status, None);
    assert_eq!(shown_of(&alone.machines[0], "status"), "?");
    assert_eq!(alone.agents.len(), 1);
}

#[test]
fn an_answer_no_registry_row_names_is_kept_and_comes_last() {
    let inputs = Inputs {
        lab: live(lab_of(&["m3", "m22"]), NOW),
        read: read_of(vec![
            peer(
                "m1",
                answered(2, vec![agent("s9", Harness::Codex, State::Idle, None)]),
            ),
            peer("m22", answered(1, Vec::new())),
            MachineSeen::peer("10.0.1.20", "10", None, answered(1, Vec::new())),
        ]),
        ..Inputs::default()
    };
    let tables = tables(&inputs);
    assert_eq!(labels(&tables), ["m3", "m22", "m1", "-"]);
    let (m1, by_address) = (&tables.machines[2], &tables.machines[3]);
    assert_eq!(m1.status, Some(UNREGISTERED));
    assert!(m1.machine.starts_with("un-"), "{}", m1.machine);
    assert_eq!(m1.sessions.map(|at| at.v), Some(2));
    assert_eq!(m1.host.as_deref(), Some("m1-test"));
    // Its agents are rows too, under its key.
    assert_eq!(tables.agents.len(), 1);
    assert_eq!(tables.agents[0].machine, m1.machine);
    // A machine dialled by address is kept under a digest: the address is in no cell.
    assert_eq!(by_address.status, Some(UNREGISTERED));
    assert_eq!(by_address.host, None);
    let printed = print(&tables, "machines", &[], false)
        .expect("machines print")
        .text;
    assert!(!printed.contains("10.0.1"), "{printed}");
    // The same dial is the same row from one read to the next.
    let again = MachineSeen::peer("10.0.1.20", "10", None, Answer::Silent { why: None });
    assert_eq!(again.id, by_address.machine);

    // Control: once the registry names it, it is that row and no stranger.
    let mut inputs = inputs;
    inputs.lab = live(lab_of(&["m1", "m3", "m22"]), NOW);
    let named = self::tables(&inputs);
    assert_eq!(labels(&named), ["m1", "m3", "m22", "-"]);
    assert_eq!(named.machines[0].status, Some("active"));
    assert_eq!(named.machines[0].sessions.map(|at| at.v), Some(2));
}

#[test]
fn no_address_appears_in_an_agents_row() {
    let (inputs, tables) = fixture_tables();
    // Control: the read the rows were folded from does name accounts by address.
    let latest = inputs.read.value().expect("the fixture holds a read");
    let by_address = latest
        .machines
        .values()
        .filter_map(|sighted| sighted.answered.as_ref())
        .flat_map(|answered| &answered.v.agents)
        .filter_map(|agent| agent.account.as_ref())
        .filter(|word| word.as_str().contains('@'))
        .count();
    assert!(by_address > fixture::AGENTS / 2, "{by_address}");

    for json in [false, true] {
        let printed = print(&tables, "agents", &[], json)
            .expect("agents print")
            .text;
        assert!(!printed.contains('@'), "an agents row prints no address");
        assert!(!printed.contains("fixture.example"));
    }
    let debug = format!("{:?}", tables.agents);
    assert!(!debug.contains('@') && !debug.contains("fixture.example"));
    // Nor does a debug print of the read itself: an account word prints no part of itself.
    let debug = format!("{latest:?}");
    assert!(!debug.contains('@') && !debug.contains("fixture.example"));
    assert!(debug.contains("AccountWord(..)"));

    // A resolved word is a registry key, an unresolved one nothing, and both are counted.
    let resolved = tables
        .agents
        .iter()
        .filter(|row| matches!(&row.account, AccountRef::Resolved(pk) if pk.starts_with("ac-")))
        .count();
    let unresolved: Vec<_> = tables
        .agents
        .iter()
        .filter(|row| row.account == AccountRef::Unresolved)
        .collect();
    let unnamed = tables
        .agents
        .iter()
        .filter(|row| row.account == AccountRef::Unnamed)
        .count();
    assert_eq!(unresolved.len(), fixture::AGENTS_UNRESOLVED);
    assert_eq!(
        tables.counts.unresolved_accounts as usize,
        fixture::AGENTS_UNRESOLVED
    );
    assert_eq!(unnamed, fixture::AGENTS / 10);
    assert_eq!(resolved + unresolved.len() + unnamed, fixture::AGENTS);
    assert_eq!(cell_of(unresolved[0], "account"), Cell::Unread);
    assert_eq!(shown_of(unresolved[0], "account"), "?");

    // The accounts relation holds each login and prints it masked.
    assert!(tables.accounts.iter().all(|row| {
        row.login
            .as_deref()
            .is_some_and(|login| login.ends_with("@fixture.example"))
    }));
    for json in [false, true] {
        let printed = print(&tables, "accounts", &[], json)
            .expect("accounts print")
            .text;
        assert!(
            !printed.contains("fixture.example"),
            "a login prints masked"
        );
        assert!(
            printed.contains("po…@…"),
            "{}",
            &printed[..400.min(printed.len())]
        );
    }
}

#[test]
fn where_an_account_is_expected_comes_only_from_preference() {
    let row = account("claude", "one@lab.example", "claude-one");
    let pk = row.pk.clone();
    let mut lab = lab_of(&["m3", "m22"]);
    let (m3, m22) = (keys::machine_pk("m3"), keys::machine_pk("m22"));
    lab.accounts.insert(pk.clone(), row);
    lab.bound.insert(UUID.to_owned(), m3.clone());
    lab.me = Some(m3.clone());

    // Every kind of evidence says the account runs on m3: an agent, a claim and a sign-in.
    let now_s = NOW / 1_000;
    let mut seen = Seen::of_events(&[], now_s);
    seen.claims.insert(
        pk.clone(),
        vec![Claimed {
            uuid: UUID.to_owned(),
            kind: "home".to_owned(),
            since: now_s - 60,
            until: now_s + 60,
        }],
    );
    seen.signins.entry(pk.clone()).or_default().insert(
        UUID.to_owned(),
        SignedIn {
            state: SIGNIN_IN,
            t: now_s - 120,
            cli: "claude".to_owned(),
        },
    );
    let mut inputs = Inputs {
        lab: live(lab.clone(), NOW),
        journal: live(seen, NOW),
        read: read_of(vec![MachineSeen::own(
            Some("m3"),
            answered(
                1,
                vec![agent(
                    "s1",
                    Harness::Claude,
                    State::Busy,
                    Some("one@lab.example"),
                )],
            ),
        )]),
        ..Inputs::default()
    };

    // No preference names it, so nothing expects it anywhere, whatever runs.
    let before = tables(&inputs);
    let account = &before.accounts[0];
    assert!(account.expected.is_empty());
    assert_eq!(shown_of(account, "expected"), "");
    let active = account.active.as_ref().expect("evidence was read");
    assert_eq!(
        active
            .iter()
            .map(|e| (e.on.name(), e.how))
            .collect::<Vec<_>>(),
        [
            ("m3", EvidenceHow::Agent),
            ("m3", EvidenceHow::Claim),
            ("m3", EvidenceHow::SignedIn),
        ]
    );
    assert_eq!(account.agents, Some(1));
    assert_eq!(before.agents[0].account, AccountRef::Resolved(pk.clone()));
    let on = |tables: &FleetTables, n: usize| {
        (
            tables.machines[n].accounts_expected,
            tables.machines[n].accounts_active,
        )
    };
    assert_eq!(
        (on(&before, 0), on(&before, 1)),
        ((Some(0), Some(1)), (Some(0), Some(0)))
    );

    // A preference of m22 for it: it is expected there, and still runs where it ran.
    lab.preferences.push(Preference {
        machine: m22,
        account: pk,
        rank: Some(1),
    });
    inputs.lab = live(lab, NOW);
    let after = tables(&inputs);
    let account = &after.accounts[0];
    assert_eq!(
        account
            .expected
            .iter()
            .map(rows::MachineRef::name)
            .collect::<Vec<_>>(),
        ["m22"]
    );
    assert_eq!(account.active, before.accounts[0].active);
    assert_eq!(
        (on(&after, 0), on(&after, 1)),
        ((Some(0), Some(1)), (Some(1), Some(0)))
    );
    assert_eq!(kept(&after, "accounts", &["expected=m22"]), 1);
    assert_eq!(kept(&before, "accounts", &["expected=m22"]), 0);
    assert_eq!(kept(&after, "accounts", &["active=signed-in"]), 1);
    assert_eq!(kept(&after, "accounts", &["active=place"]), 0);
}

#[test]
fn an_account_word_is_resolved_by_its_harness_realm_or_its_label() {
    let mut lab = lab_of(&["m3"]);
    lab.me = Some(keys::machine_pk("m3"));
    let claude = account("claude", "both@lab.example", "claude-both");
    let codex = account("codex", "both@lab.example", "codex-both");
    let (claude_pk, codex_pk) = (claude.pk.clone(), codex.pk.clone());
    lab.accounts.insert(claude.pk.clone(), claude);
    lab.accounts.insert(codex.pk.clone(), codex);
    let agents = vec![
        agent("s1", Harness::Claude, State::Busy, Some("Both@Lab.Example")),
        agent(
            "s2",
            Harness::CodexAlab,
            State::Busy,
            Some("both@lab.example"),
        ),
        agent("s3", Harness::Other, State::Busy, Some("both@lab.example")),
        agent("s4", Harness::Other, State::Busy, Some("codex-both")),
        agent(
            "s5",
            Harness::Claude,
            State::Busy,
            Some("nobody@lab.example"),
        ),
        agent("s6", Harness::Claude, State::Busy, Some("-")),
    ];
    let tables = tables(&Inputs {
        lab: live(lab, NOW),
        read: read_of(vec![MachineSeen::own(Some("m3"), answered(6, agents))]),
        ..Inputs::default()
    });
    let accounts: Vec<&AccountRef> = tables.agents.iter().map(|row| &row.account).collect();
    assert_eq!(
        accounts,
        [
            &AccountRef::Resolved(claude_pk),
            &AccountRef::Resolved(codex_pk.clone()),
            // An address two realms hold names neither where the harness names no realm.
            &AccountRef::Unresolved,
            &AccountRef::Resolved(codex_pk),
            &AccountRef::Unresolved,
            &AccountRef::Unnamed,
        ]
    );
    assert_eq!(tables.counts.unresolved_accounts, 2);
    assert_eq!(cell_of(&tables.agents[5], "account"), Cell::Empty);
    assert_eq!(shown_of(&tables.agents[5], "account"), "");
}

#[test]
fn every_schema_is_well_formed_and_every_fixture_cell_is_of_its_columns_type() {
    for schema in rows::SCHEMAS {
        let mut names = BTreeSet::new();
        for col in schema.columns {
            let name = col.column.name;
            assert!(
                !name.is_empty()
                    && name.len() <= 20
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "{}.{name}",
                schema.name
            );
            assert!(names.insert(name), "{}.{name} is named twice", schema.name);
            assert!(
                !col.doc.is_empty(),
                "{}.{name} says what it holds",
                schema.name
            );
        }
        assert!(!schema.key.is_empty());
        for key in schema.key {
            let at = schema.position(key).expect("a key is a column");
            assert!(!schema.columns[at].column.null, "{}.{key}", schema.name);
        }
    }

    fn check<R: Row>(rows: &[R]) -> usize {
        let schema = R::SCHEMA;
        let mut values = 0;
        for row in rows {
            let cells = row.cells();
            assert_eq!(cells.len(), schema.columns.len(), "{}", schema.name);
            for (col, cell) in schema.columns.iter().zip(&cells) {
                match cell {
                    Cell::Value(value) => {
                        values += 1;
                        assert!(
                            cell::admissible(&col.column.ty, value),
                            "{}.{} holds {value}",
                            schema.name,
                            col.column.name
                        );
                    }
                    Cell::Unread | Cell::Empty => assert!(
                        col.column.null,
                        "{}.{} always holds a value",
                        schema.name, col.column.name
                    ),
                }
            }
        }
        values
    }
    let (_, tables) = fixture_tables();
    assert!(check(&tables.machines) > fixture::MACHINES * 5);
    assert!(check(&tables.accounts) > fixture::ACCOUNTS * 5);
    assert!(check(&tables.agents) > fixture::AGENTS * 5);

    // The schema verb prints one line a column, and each relation alone.
    let all = print_schema(None, false).expect("schemas print");
    let columns: usize = rows::SCHEMAS
        .iter()
        .map(|schema| schema.columns.len())
        .sum();
    assert_eq!(all.lines().count(), columns + 1);
    let agents = grid_of(&all)
        .into_iter()
        .filter(|line| line[0] == "agents" && line[4] == "key")
        .map(|line| line[1].to_owned())
        .collect::<Vec<_>>();
    assert_eq!(agents, ["machine", "sid"]);
    let one = print_schema(Some("accounts"), true).expect("a schema prints");
    let value: Value = aterm_json::from_str(&one).expect("the schema is JSON");
    let relations = value
        .get("relations")
        .and_then(Value::as_array)
        .expect("relations");
    assert_eq!(relations.len(), 1);
    assert!(
        one.contains("\"pii\":true"),
        "the login column says it is masked"
    );
    assert!(print_schema(Some("nothing"), false).is_err());
}

#[test]
fn the_fixture_is_the_labs_scale_and_the_same_every_time() {
    let (inputs, tables) = fixture_tables();
    assert_eq!(tables, self::tables(&fixture::inputs(NOW)));
    let lab = inputs.lab.value().expect("a registry");
    assert_eq!(lab.bound.len(), fixture::MACHINES_BOUND);
    assert_eq!(lab.preferences.len(), fixture::PREFERENCES);
    let status = |word: &str| {
        tables
            .accounts
            .iter()
            .filter(|row| row.status == Some(word))
            .count()
    };
    assert_eq!(
        (status("active"), status("pending"), status("paused")),
        (fixture::ACCOUNTS_ACTIVE, 109, 2)
    );
    let up = tables
        .machines
        .iter()
        .filter(|row| row.reach == Reach::Up)
        .count();
    let silent = tables
        .machines
        .iter()
        .filter(|row| row.reach == Reach::Silent)
        .count();
    assert_eq!((up, silent), (fixture::MACHINES_ANSWERING + 1, 4));
    assert_eq!(
        tables
            .machines
            .iter()
            .filter(|row| row.how == Via::Own)
            .count(),
        1
    );
    let expected: u32 = tables
        .machines
        .iter()
        .filter_map(|row| row.accounts_expected)
        .sum();
    assert_eq!(expected as usize, fixture::PREFERENCES);
    // A peer whose header names no label is joined by the number its host name starts with.
    assert_eq!(tables.counts.shadowed, 0);
    assert!(
        tables
            .machines
            .iter()
            .all(|row| row.status != Some(UNREGISTERED)
                || row.label.as_deref() == Some(fixture::UNREGISTERED_LABEL))
    );
    // Some windows were read before their reset passed: they have no figure, and say why.
    let passed: Vec<_> = tables
        .accounts
        .iter()
        .filter(|row| row.w5h.state == WindowState::ResetPassed)
        .collect();
    assert!(!passed.is_empty());
    for row in passed {
        assert_eq!(shown_of(row, "w5h_used_x10"), "?");
        assert_eq!(shown_of(row, "w5h_state"), "reset-passed");
    }
    assert!(tables.accounts.iter().any(|row| row.walled == Some(true)));
    assert!(tables.accounts.iter().any(|row| row.read_by.is_some()));
}

#[test]
fn the_rows_change_by_themselves_only_at_the_instant_the_fold_names() {
    let inputs = fixture::inputs(NOW);
    let now = fold(&inputs, NOW);
    let next = now.next_change_ms.expect("a window resets ahead");
    assert!(next > NOW);
    assert_eq!(fold(&inputs, next - 1).tables, now.tables);
    assert_ne!(fold(&inputs, next).tables, now.tables);

    // Two hours after a machine's last answer, it no longer speaks for its agents: they are
    // unknown, not gone and not idle.
    let later = fold(&inputs, NOW + AGENTS_KEEP_MS).tables;
    assert!(later.agents.is_empty());
    let me = now
        .tables
        .machines
        .iter()
        .find(|row| row.how == Via::Own)
        .expect("this Mac is a row")
        .machine
        .clone();
    let own = later
        .machines
        .iter()
        .find(|row| row.machine == me)
        .expect("this Mac is still a row");
    for column in ["sessions", "agents", "busy", "agents_listed"] {
        assert_eq!(shown_of(own, column), "?", "{column}");
    }
    assert!(own.last_seen_ms.is_some() && own.build.is_some());
    // Control: one millisecond inside the two hours they are all still rows.
    let inside = fold(&inputs, NOW - 20_000 + AGENTS_KEEP_MS).tables;
    assert_eq!(inside.agents.len(), fixture::AGENTS);
}

#[test]
fn a_silent_machine_keeps_its_last_answer_and_an_unnamed_one_is_dropped_in_time() {
    let first = Latest::default().with(whole(
        NOW,
        vec![
            peer("m3", answered(2, Vec::new())),
            peer("m5", answered(1, Vec::new())),
        ],
    ));
    let second = first.with(whole(
        NOW + 60_000,
        vec![peer(
            "m3",
            Answer::Silent {
                why: Some("nothing-listens".to_owned()),
            },
        )],
    ));
    let m3 = second
        .machines
        .values()
        .find(|sighted| sighted.label.as_deref() == Some("m3"))
        .expect("m3 is held");
    assert_eq!(
        m3.answered.as_ref().map(|at| (at.v.sessions, at.at_ms)),
        Some((Some(2), NOW))
    );
    assert_eq!(
        m3.silent,
        Some(At::new(Some("nothing-listens".to_owned()), NOW + 60_000))
    );
    let tables = fold(
        &Inputs {
            read: live(second.clone(), NOW + 60_000),
            ..Inputs::default()
        },
        NOW + 61_000,
    )
    .tables;
    let row = tables
        .machines
        .iter()
        .find(|row| row.label.as_deref() == Some("m3"))
        .expect("m3 is a row");
    assert_eq!(row.reach, Reach::Silent);
    assert_eq!(row.reach_why.as_deref(), Some("nothing-listens"));
    assert_eq!(
        row.sessions.map(|at| at.v),
        Some(2),
        "what it last said stands, dated"
    );
    // m5, which the second read did not name, says nothing of now.
    let m5 = tables
        .machines
        .iter()
        .find(|row| row.label.as_deref() == Some("m5"))
        .expect("m5 is a row");
    assert_eq!((m5.reach, m5.how), (Reach::Unread, Via::None));
    assert_eq!(m5.last_seen_ms, Some(NOW));

    let third = second.with(whole(
        NOW + SIGHTED_KEEP_MS + 1,
        vec![peer("m3", answered(0, Vec::new()))],
    ));
    assert_eq!(
        third.machines.len(),
        1,
        "a machine no read names for two hours leaves"
    );
    assert_eq!(second.machines.len(), 2);
}

#[test]
fn a_word_a_row_carries_holds_no_address_and_no_sentence() {
    assert_eq!(
        word(" fable-5.2 [1m] ", 48).as_deref(),
        Some("fable-5.2 [1m]")
    );
    assert_eq!(
        word("0.141.0+g1a2b3c4", 48).as_deref(),
        Some("0.141.0+g1a2b3c4")
    );
    for refused in ["one@lab.example", "-", "", "a\tb", "\"quoted\"", "x; rm"] {
        assert_eq!(word(refused, 48), None, "{refused:?}");
    }
    assert_eq!(word("too long", 4), None);
    assert!(AccountWord::new("-").is_none() && AccountWord::new("  ").is_none());
    assert_eq!(Harness::of_program(Some("codex-alab")), Harness::CodexAlab);
    assert_eq!(Harness::of_program(Some("vim")), Harness::Other);
    assert_eq!(Harness::of_program(None), Harness::Other);
}

#[test]
fn a_folder_publishes_only_what_changed_and_a_fixture_holds_against_later_offers() {
    let folder = Folder::new();
    let first = folder.tables().revision;
    folder.fold_now(NOW);
    assert_eq!(
        folder.tables().revision,
        first,
        "nothing was offered: nothing changed"
    );

    folder.offer_read(whole(NOW, vec![peer("m3", answered(1, Vec::new()))]));
    folder.fold_now(NOW);
    let read = folder.try_tables().expect("nothing holds the tables");
    assert_eq!(read.revision, first + 1);
    assert_eq!(read.value.machines.len(), 1);
    folder.fold_now(NOW + 1);
    assert_eq!(
        folder.tables().revision,
        first + 1,
        "the same rows are the same revision"
    );

    // A failed read keeps the rows and says the input is held.
    folder.hold_read(HELD_READ_FAILED);
    folder.fold_now(NOW + 2);
    let held = folder.tables();
    assert_eq!(held.revision, first + 2);
    assert_eq!(held.value.machines, read.value.machines);
    assert_eq!(held.value.sources.read.why(), Some(HELD_READ_FAILED));
    assert_eq!(
        sources_line(&held.value.sources),
        "registry=never journal=never network=never fleet_read=held(read-failed)"
    );

    // A fixture stands: a reader's later offer does not replace it.
    folder.offer_fixture(fixture::inputs(NOW));
    folder.fold_now(NOW);
    assert_eq!(folder.tables().value.machines.len(), fixture::MACHINES + 1);
    folder.offer_read(whole(NOW + 5, Vec::new()));
    folder.offer_lab(Reading::Off, Reading::Off, NOW + 5);
    folder.fold_now(NOW);
    assert_eq!(folder.tables().value.machines.len(), fixture::MACHINES + 1);
}

#[test]
fn a_reader_that_could_not_read_holds_what_it_read_before_and_one_turned_off_drops_it() {
    let folder = Folder::new();
    let lab = Arc::new(lab_of(&["m3", "m22"]));
    let seen = Arc::new(Seen::of_events(&[], NOW / 1_000));
    folder.offer_lab(Reading::Read(lab), Reading::Read(seen), NOW);
    folder.fold_now(NOW);
    let read = folder.tables();
    assert_eq!(read.value.machines.len(), 2);
    assert_eq!(
        (
            read.value.sources.lab.word(),
            read.value.sources.journal.word()
        ),
        ("live", "live")
    );

    folder.offer_lab(
        Reading::Held(HELD_PARTIAL),
        Reading::Held(HELD_PARTIAL),
        NOW + 300_000,
    );
    folder.fold_now(NOW + 300_000);
    let held = folder.tables();
    assert_eq!(held.revision, read.revision + 1);
    assert_eq!(
        held.value.machines, read.value.machines,
        "the rows it read stay"
    );
    assert_eq!(held.value.sources.lab.why(), Some(HELD_PARTIAL));
    assert_eq!(
        held.value.sources.lab.at_ms(),
        Some(NOW),
        "and are dated by that read"
    );

    folder.offer_lab(Reading::Off, Reading::Off, NOW + 600_000);
    folder.fold_now(NOW + 600_000);
    let off = folder.tables();
    assert!(off.value.machines.is_empty());
    assert_eq!(off.value.sources.lab.word(), "off");

    // The network observer's snapshot is offered at every look: only new network facts fold.
    let snapshot = Arc::new(NetworkStateSnapshot {
        network: NetworkObservation {
            observed_at_ms: NOW,
            ..NetworkObservation::default()
        },
        ..NetworkStateSnapshot::default()
    });
    folder.offer_network(&snapshot);
    folder.fold_now(NOW);
    let seen = folder.tables();
    assert_eq!(seen.revision, off.revision + 1);
    assert_eq!(seen.value.sources.network.at_ms(), Some(NOW));
    folder.offer_network(&snapshot);
    let services_moved = Arc::new(NetworkStateSnapshot {
        revision: 9,
        ..NetworkStateSnapshot::clone(&snapshot)
    });
    folder.offer_network(&services_moved);
    folder.fold_now(NOW);
    assert_eq!(folder.tables().revision, seen.revision);
    let moved = Arc::new(NetworkStateSnapshot {
        network: NetworkObservation {
            observed_at_ms: NOW + 15_000,
            ..NetworkObservation::default()
        },
        ..NetworkStateSnapshot::default()
    });
    folder.offer_network(&moved);
    folder.fold_now(NOW);
    assert_eq!(folder.tables().revision, seen.revision + 1);
}

#[test]
fn the_registry_reader_starts_on_the_first_ask_for_the_tables() {
    let started = Arc::new(AtomicUsize::new(0));
    let starter = |started: &Arc<AtomicUsize>| -> Box<dyn Fn() + Send + Sync> {
        let started = Arc::clone(started);
        Box::new(move || {
            started.fetch_add(1, Ordering::SeqCst);
        })
    };
    let folder = Folder::new();
    assert!(folder.install_lab_reader(starter(&started)));
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "nobody asked: nothing reads the registry"
    );
    let _ = folder.tables();
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "a waiting read is no ask"
    );
    let _ = folder.try_tables();
    let _ = folder.try_tables();
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert!(
        !folder.install_lab_reader(starter(&started)),
        "the first install stands"
    );
    assert_eq!(started.load(Ordering::SeqCst), 1);

    // Asked before a reader was installed: it starts when it is.
    let late = Folder::new();
    let _ = late.try_tables();
    assert!(late.install_lab_reader(starter(&started)));
    assert_eq!(started.load(Ordering::SeqCst), 2);

    // The fold thread is started by the first ask too, and by nothing before it: a process
    // that only offers (a command, a window with no surface of the tables) folds nothing.
    let folds = Arc::new(AtomicUsize::new(0));
    let folder = Folder::new();
    assert!(folder.fold_thread.set(starter(&folds)).is_ok());
    let first = folder.tables().revision;
    folder.offer_read(whole(NOW, vec![peer("m3", answered(1, Vec::new()))]));
    folder.offer_lab(Reading::Off, Reading::Off, NOW);
    folder.hold_read(HELD_READ_FAILED);
    assert_eq!(
        folds.load(Ordering::SeqCst),
        0,
        "an offer starts no fold thread"
    );
    assert_eq!(
        folder.tables().revision,
        first,
        "and nothing was folded: nobody reads the tables"
    );
    assert!(folder.tables().value.machines.is_empty());
    let _ = folder.try_tables();
    let _ = folder.try_tables();
    assert_eq!(
        folds.load(Ordering::SeqCst),
        1,
        "the first ask starts it, once"
    );
    // This process's own folder is made without one running: making it folds nothing either.
    assert_eq!(super::folder().tables().revision, 0);
}

/// One turn of the fold thread folds when an offer changed an input or when the instant the
/// last fold named has come, and waits otherwise.
#[test]
fn the_fold_thread_folds_on_an_offer_or_at_the_instant_the_last_fold_named() {
    let folder: &'static Folder = Box::leak(Box::new(Folder::new()));
    let read_at = NOW - REACH_KEEP_MS + 50;
    folder.offer_read(whole(read_at, vec![peer("m3", answered(1, Vec::new()))]));

    // An offer was made: the turn folds at once and names when the rows next change.
    let due = folder.turn(None, &|| NOW);
    assert_eq!(due, Some(NOW + 51), "the read stops saying who answers");
    let folded = folder.tables();
    assert_eq!(folded.revision, 1);
    assert_eq!(folded.value.machines[0].reach, Reach::Up);

    // Nothing was offered, and the clock comes to that instant while the turn waits: it folds
    // then, and the row has changed by itself.
    let ticks = AtomicUsize::new(0);
    let clock = || NOW + 20 * ticks.fetch_add(1, Ordering::SeqCst) as u64;
    let after = folder.turn(due, &clock);
    assert!(
        ticks.load(Ordering::SeqCst) >= 4,
        "it waited for the instant, reading the clock again each time"
    );
    let lapsed = folder.tables();
    assert_eq!(lapsed.revision, 2);
    assert_eq!(lapsed.value.machines[0].reach, Reach::Unread);
    assert_eq!(lapsed.value.sources.read.why(), Some(HELD_STALE));
    assert!(after.is_some_and(|next| next > NOW + 51));

    // With nothing offered and no instant ahead, a turn waits for an offer, however long.
    let (done, turned) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(folder.turn(None, &|| NOW + 1_000));
    });
    assert_eq!(
        turned.recv_timeout(std::time::Duration::from_millis(200)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout),
        "no offer and no instant: no fold"
    );
    assert_eq!(folder.tables().revision, 2);
    folder.offer_lab(
        Reading::Read(Arc::new(lab_of(&["m3", "m22"]))),
        Reading::Off,
        NOW + 1_000,
    );
    turned
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the offer wakes the turn");
    let offered = folder.tables();
    assert_eq!(offered.revision, 3);
    assert_eq!(offered.value.machines.len(), 2);
}

/// A read says which machines answer for [`REACH_KEEP_MS`] and no longer: reads that stopped
/// (the glance unwatched, this Mac off the LAN or asleep) leave no machine `up`.
#[test]
fn a_read_says_which_machines_answer_for_five_minutes_and_no_longer() {
    let read_at = NOW - 1_000;
    let inputs = Inputs {
        lab: live(lab_of(&["m3", "m7"]), NOW),
        read: read_of(vec![
            peer(
                "m3",
                Answer::Silent {
                    why: Some("nothing-listens".to_owned()),
                },
            ),
            peer("m7", answered(2, Vec::new())),
        ]),
        ..Inputs::default()
    };
    let reach = |tables: &FleetTables| -> Vec<(Reach, Via, Option<u64>)> {
        tables
            .machines
            .iter()
            .map(|row| (row.reach, row.how, row.reach_at_ms))
            .collect()
    };
    let young = fold(&inputs, read_at + REACH_KEEP_MS);
    assert_eq!(
        reach(&young.tables),
        [
            (Reach::Silent, Via::Survey, Some(read_at)),
            (Reach::Up, Via::Survey, Some(read_at)),
        ]
    );
    assert_eq!(young.tables.sources.read.word(), "live");
    assert_eq!(
        fold(&inputs, NOW).next_change_ms,
        Some(read_at + REACH_KEEP_MS + 1),
        "the fold names the instant the read stops speaking"
    );

    // One millisecond later, and six hours later, nothing current says either way.
    for now in [read_at + REACH_KEEP_MS + 1, read_at + 6 * 3_600_000] {
        let old = fold(&inputs, now).tables;
        assert_eq!(
            reach(&old),
            [(Reach::Unread, Via::None, None); 2],
            "at {now}"
        );
        assert_eq!(shown_of(&old.machines[1], "reach"), "unread");
        assert_eq!(
            (old.sources.read.word(), old.sources.read.why()),
            ("held", Some(HELD_STALE))
        );
        assert_eq!(old.sources.read.at_ms(), Some(read_at), "dated by the read");
        assert_eq!(
            old.machines[1].last_seen_ms,
            Some(read_at),
            "when it last answered stands"
        );
    }
    assert_eq!(
        sources_line(&fold(&inputs, read_at + 6 * 3_600_000).tables.sources),
        "registry=live journal=never network=never fleet_read=held(stale)"
    );
    // What it said of its sessions stands for two hours, dated, as before.
    let old = fold(&inputs, read_at + REACH_KEEP_MS + 1).tables;
    assert_eq!(old.machines[1].sessions, Some(At::new(2, read_at)));
}

/// A read whose question of the fleet was refused still heard this Mac's own windows: its row
/// takes that answer, and every peer keeps what the last read of the fleet said of it.
#[test]
fn a_refused_fleet_read_gives_this_mac_its_own_answer_and_holds_the_peers() {
    let mut lab = lab_of(&["m3", "m7"]);
    lab.me = Some(keys::machine_pk("m3"));
    let folder = Folder::new();
    folder.offer_lab(Reading::Read(Arc::new(lab)), Reading::Off, NOW);
    let mut first = vec![MachineSeen::own(None, answered(2, Vec::new()))];
    first.push(peer("m7", answered(5, Vec::new())));
    folder.offer_read(whole(NOW, first));
    folder.fold_now(NOW + 1);
    assert_eq!(folder.tables().value.sources.read.word(), "live");

    // A minute on, this Mac is away: its windows answer and the fleet is not asked.
    let agents = vec![agent("s1", Harness::Claude, State::Busy, None)];
    folder.offer_read(own_alone(NOW + 60_000, answered(3, agents)));
    folder.fold_now(NOW + 61_000);
    let away = folder.tables().value;
    let (me, m7) = (&away.machines[0], &away.machines[1]);
    assert_eq!(me.sessions, Some(At::new(3, NOW + 60_000)));
    assert_eq!(
        (me.reach, me.how, me.reach_at_ms),
        (Reach::Up, Via::Own, Some(NOW + 60_000))
    );
    assert_eq!(
        away.agents.len(),
        1,
        "its agents are the ones it just listed"
    );
    assert_eq!(
        m7.sessions,
        Some(At::new(5, NOW)),
        "a peer keeps the answer the last read of the fleet heard"
    );
    assert_eq!((m7.reach, m7.reach_at_ms), (Reach::Up, Some(NOW)));
    assert_eq!(
        (away.sources.read.why(), away.sources.read.at_ms()),
        (Some(HELD_FLEET_UNREAD), Some(NOW)),
        "held, and dated by the last read that asked the fleet"
    );

    // The refused reads go on: this Mac's row stays its own, a peer's word of now runs out,
    // and no peer is dropped for going unasked.
    for minute in 2..=(SIGHTED_KEEP_MS / 60_000 + 2) {
        folder.offer_read(own_alone(NOW + minute * 60_000, answered(3, Vec::new())));
    }
    let end = NOW + (SIGHTED_KEEP_MS / 60_000 + 2) * 60_000;
    folder.fold_now(end + 1);
    let long = folder.tables().value;
    let (me, m7) = (&long.machines[0], &long.machines[1]);
    assert_eq!((me.reach, me.sessions), (Reach::Up, Some(At::new(3, end))));
    assert_eq!((m7.reach, m7.how), (Reach::Unread, Via::None));
    assert_eq!(m7.last_seen_ms, Some(NOW), "it is still held");
    assert_eq!(m7.sessions, None, "its two hours have passed");

    // Control: a read that did ask the fleet and no longer names it drops it.
    folder.offer_read(whole(end + 60_000, Vec::new()));
    folder.fold_now(end + 60_001);
    let asked = folder.tables().value;
    assert_eq!(asked.machines[1].last_seen_ms, None);
    assert_eq!(asked.sources.read.word(), "live");

    // With no read of the fleet before it, a refused read's row is this Mac's alone.
    let alone = Folder::new();
    alone.offer_read(own_alone(NOW, answered(1, Vec::new())));
    alone.fold_now(NOW);
    let alone = alone.tables().value;
    assert_eq!(alone.machines.len(), 1);
    assert_eq!(
        (alone.sources.read.why(), alone.sources.read.at_ms()),
        (Some(HELD_FLEET_UNREAD), Some(NOW))
    );
}

/// A network observation no observer is making is held, never live: with no history as
/// `no-history`, with retained history as `unobserved`, dated by that history.
#[test]
fn a_network_observation_nobody_is_making_is_held() {
    let snapshot = |visibility, history, observed_at_ms| {
        Arc::new(NetworkStateSnapshot {
            network: NetworkObservation {
                observed_at_ms,
                visibility,
                history,
                ..NetworkObservation::default()
            },
            ..NetworkStateSnapshot::default()
        })
    };
    let network = |folder: &Folder| {
        folder.fold_now(NOW);
        folder.tables().value.sources.network
    };

    // A window with no observer and no retained history: nothing was observed.
    let folder = Folder::new();
    let fallback = snapshot(Visibility::Unavailable, HistoryState::Unavailable, 0);
    folder.offer_network(&fallback);
    let none = network(&folder);
    assert_eq!(
        (none.word(), none.why(), none.at_ms()),
        ("held", Some(HELD_NO_HISTORY), None),
        "never `live`, and never read at the epoch"
    );
    let revision = folder.tables().revision;
    folder.offer_network(&fallback);
    folder.offer_network(&snapshot(
        Visibility::Unavailable,
        HistoryState::Unavailable,
        0,
    ));
    folder.fold_now(NOW);
    assert_eq!(
        folder.tables().revision,
        revision,
        "the same again changes nothing"
    );

    // An observer starts: its observation is live.
    let watching = snapshot(Visibility::Active, HistoryState::Ready, NOW - 2_000);
    folder.offer_network(&watching);
    let live = network(&folder);
    assert_eq!((live.word(), live.at_ms()), ("live", Some(NOW - 2_000)));

    // Its publication expires: what it observed is retained history, held and dated.
    let expired = snapshot(Visibility::Unavailable, HistoryState::Ready, NOW - 2_000);
    folder.offer_network(&expired);
    let held = network(&folder);
    assert_eq!(
        (held.word(), held.why(), held.at_ms()),
        ("held", Some(HELD_UNOBSERVED), Some(NOW - 2_000))
    );
    let revision = folder.tables().revision;
    folder.offer_network(&snapshot(
        Visibility::Unavailable,
        HistoryState::Ready,
        NOW - 2_000,
    ));
    folder.fold_now(NOW);
    assert_eq!(folder.tables().revision, revision);

    // Its history becomes unreadable too: what was observed before stays, dated.
    folder.offer_network(&fallback);
    let lost = network(&folder);
    assert_eq!(
        (lost.why(), lost.at_ms()),
        (Some(HELD_NO_HISTORY), Some(NOW - 2_000))
    );
    assert_eq!(
        sources_line(&folder.tables().value.sources),
        "registry=never journal=never network=held(no-history) fleet_read=never"
    );
}

/// A machine's build is the newer of its last answer's and its journal row's, and a journal row
/// its writer dated ahead of the journal's read is held to that read.
#[test]
fn a_machines_build_is_its_newest_reading_and_no_journal_row_is_from_the_future() {
    let mut lab = lab_of(&["m40"]);
    lab.bound.insert(UUID.to_owned(), keys::machine_pk("m40"));
    let now_s = NOW / 1_000;
    let monday = NOW - 2 * 86_400_000;
    // It answered on Monday with 0.139.0 and has been asked, and silent, since.
    let mut old = answered(1, Vec::new());
    if let Answer::Answered(answer) = &mut old {
        answer.build = Some("0.139.0".to_owned());
    }
    let latest = Latest::default()
        .with(whole(monday, vec![peer("m40", old)]))
        .with(whole(
            NOW - 1_000,
            vec![peer("m40", Answer::Silent { why: None })],
        ));
    let with_sum = |at: u64| {
        let mut seen = Seen::of_events(&[], now_s);
        seen.sums.insert(
            UUID.to_owned(),
            aterm_json::json!({"at": at, "aterm": "0.141.0"}),
        );
        tables(&Inputs {
            lab: live(lab.clone(), NOW),
            journal: live(seen, NOW),
            read: live(latest.clone(), NOW - 1_000),
            ..Inputs::default()
        })
    };

    // Its journal row of an hour ago says 0.141.0: that is the newer reading.
    let newer = with_sum(now_s - 3_600);
    let row = &newer.machines[0];
    assert_eq!(
        row.build,
        Some(At::new("0.141.0".to_owned(), NOW - 3_600_000))
    );
    assert_eq!(row.last_seen_ms, Some(NOW - 3_600_000));
    // Control: a journal row older than the answer leaves the answer's build standing.
    let older = with_sum(monday / 1_000 - 3_600);
    assert_eq!(
        older.machines[0].build,
        Some(At::new("0.139.0".to_owned(), monday))
    );
    assert_eq!(older.machines[0].last_seen_ms, Some(monday));

    // A writer whose clock runs a day ahead: its row is held to the journal's read.
    let ahead = with_sum(now_s + 86_400);
    let row = &ahead.machines[0];
    assert_eq!(
        row.last_seen_ms,
        Some(NOW),
        "nothing was seen in the future"
    );
    assert_eq!(row.build, Some(At::new("0.141.0".to_owned(), NOW)));
}

/// THE FOLD NAMES EVERY INSTANT ITS ROWS CHANGE BY THEMSELVES: a claim's and a place's end
/// among them, and nothing changes between two instants it names.
#[test]
fn a_claim_and_a_place_end_at_an_instant_the_fold_names_and_nothing_changes_between() {
    let row = account("claude", "one@lab.example", "claude-one");
    let pk = row.pk.clone();
    let mut lab = lab_of(&["m3"]);
    lab.accounts.insert(pk.clone(), row);
    lab.bound.insert(UUID.to_owned(), keys::machine_pk("m3"));
    let now_s = NOW / 1_000;
    let inputs_of = |claim_until: Option<u64>, place_until: Option<u64>| {
        let mut seen = Seen::of_events(&[], now_s);
        if let Some(until) = claim_until {
            seen.claims.insert(
                pk.clone(),
                vec![Claimed {
                    uuid: UUID.to_owned(),
                    kind: "home".to_owned(),
                    since: now_s - 60,
                    until,
                }],
            );
        }
        if let Some(until) = place_until {
            seen.places.rows.push(Placed {
                uuid: UUID.to_owned(),
                account: pk.clone(),
                why: Why::Fill,
                since: now_s - 60,
                until,
            });
        }
        Inputs {
            lab: live(lab.clone(), NOW),
            journal: live(seen, NOW),
            ..Inputs::default()
        }
    };
    let on = |tables: &FleetTables| {
        let how: Vec<EvidenceHow> = tables.accounts[0]
            .active
            .iter()
            .flatten()
            .map(|e| e.how)
            .collect();
        (how, tables.machines[0].accounts_active)
    };
    for (inputs, how) in [
        (inputs_of(Some(now_s + 60), None), EvidenceHow::Claim),
        (inputs_of(None, Some(now_s + 60)), EvidenceHow::Place),
    ] {
        let now = fold(&inputs, NOW);
        let ends = (now_s + 60) * 1_000;
        assert_eq!(on(&now.tables), (vec![how], Some(1)));
        assert_eq!(now.next_change_ms, Some(ends), "{how:?}");
        assert_eq!(fold(&inputs, ends - 1).tables, now.tables);
        let after = fold(&inputs, ends);
        assert_eq!(on(&after.tables), (Vec::new(), Some(0)), "{how:?}");
        assert_eq!(after.next_change_ms, None);
    }

    // Over the whole fixture, from each instant the fold names to the next: the rows are the
    // same a millisecond before it, and the walk ends, with nothing left to change.
    let inputs = fixture::inputs(NOW);
    let mut held = fold(&inputs, NOW);
    let mut instants = 0;
    while let Some(next) = held.next_change_ms {
        assert_eq!(
            fold(&inputs, next - 1).tables,
            held.tables,
            "something changed before {next}, the instant named"
        );
        let then = fold(&inputs, next);
        assert_ne!(then.tables, held.tables, "nothing changed at {next}");
        held = then;
        instants += 1;
    }
    assert!(instants > 50, "{instants}");
    // The fixture holds claims, and the walk passed their end.
    let claims = |tables: &FleetTables| {
        tables
            .accounts
            .iter()
            .flat_map(|row| row.active.iter().flatten())
            .filter(|e| e.how == EvidenceHow::Claim)
            .count()
    };
    assert!(claims(&fold(&inputs, NOW).tables) > 0);
    assert_eq!(claims(&held.tables), 0);
}

/// Where no machine's answer is young enough to speak for its agents, how many sessions run on
/// an account is unread, not zero.
#[test]
fn an_accounts_sessions_are_unread_once_no_answer_speaks_for_agents() {
    let row = account("claude", "one@lab.example", "claude-one");
    let mut lab = lab_of(&["m3"]);
    lab.accounts.insert(row.pk.clone(), row);
    lab.me = Some(keys::machine_pk("m3"));
    let mut inputs = Inputs {
        lab: live(lab, NOW),
        read: read_of(vec![MachineSeen::own(
            None,
            answered(
                1,
                vec![agent(
                    "s1",
                    Harness::Claude,
                    State::Busy,
                    Some("one@lab.example"),
                )],
            ),
        )]),
        ..Inputs::default()
    };
    let young = tables(&inputs);
    assert_eq!(young.accounts[0].agents, Some(1));
    assert_eq!(shown_of(&young.accounts[0], "agents"), "1");
    assert_eq!(young.machines[0].accounts_active, Some(1));
    // Control: an account no agent names, while answers do speak, has a counted zero.
    let other = account("claude", "two@lab.example", "claude-two");
    let mut more = LabView::clone(inputs.lab.value().expect("a registry"));
    more.accounts.insert(other.pk.clone(), other);
    let counted = tables(&Inputs {
        lab: live(more, NOW),
        ..inputs.clone()
    });
    let zero = counted
        .accounts
        .iter()
        .find(|row| row.slug.as_deref() == Some("claude-two"))
        .expect("the other account");
    assert_eq!(
        (zero.agents, shown_of(zero, "agents")),
        (Some(0), "0".into())
    );

    // Two hours on with no newer read: the agents are unknown, and so is the account's count.
    let late = NOW + AGENTS_KEEP_MS;
    let old = fold(&inputs, late).tables;
    assert!(old.agents.is_empty());
    let row = &old.accounts[0];
    assert_eq!(row.agents, None);
    assert_eq!(shown_of(row, "agents"), "?");
    assert_eq!(row.active, None, "no evidence source says anything current");
    assert_eq!(shown_of(row, "active"), "?");
    assert_eq!(shown_of(&old.machines[0], "accounts_active"), "?");

    // With the journal read, its evidence is read (and empty); the sessions are still unread.
    inputs.journal = live(Seen::of_events(&[], NOW / 1_000), NOW);
    let journal = fold(&inputs, late).tables;
    assert_eq!(journal.accounts[0].active, Some(Vec::new()));
    assert_eq!(journal.accounts[0].agents, None);
    assert_eq!(journal.machines[0].accounts_active, Some(0));
}

/// This Mac's own answer is the row of the machine its facts row binds, whatever label its
/// header carries: a stale local label takes no other machine's row.
#[test]
fn this_mac_is_the_machine_its_facts_row_binds_whatever_its_header_says() {
    let mut lab = lab_of(&["m5", "m7"]);
    lab.me = Some(keys::machine_pk("m7"));
    let read = || {
        read_of(vec![
            // Its header still says m5, after a renumbering.
            MachineSeen::own(Some("m5"), answered(4, Vec::new())),
            peer("m5", answered(1, Vec::new())),
        ])
    };
    let tables = self::tables(&Inputs {
        lab: live(lab.clone(), NOW),
        read: read(),
        ..Inputs::default()
    });
    let (m5, m7) = (&tables.machines[0], &tables.machines[1]);
    assert_eq!((m7.how, m7.sessions.map(|at| at.v)), (Via::Own, Some(4)));
    assert_eq!(
        (m5.how, m5.sessions.map(|at| at.v)),
        (Via::Survey, Some(1)),
        "the real m5 keeps its own answer"
    );
    assert_eq!(tables.counts.shadowed, 0);

    // Control: with no facts row binding this Mac, its header's label is what names it, and
    // its own answer stands over the peer's of the same name.
    lab.me = None;
    let unbound = self::tables(&Inputs {
        lab: live(lab, NOW),
        read: read(),
        ..Inputs::default()
    });
    assert_eq!(
        (
            unbound.machines[0].how,
            unbound.machines[0].sessions.map(|at| at.v)
        ),
        (Via::Own, Some(4))
    );
    assert_eq!(unbound.machines[1].how, Via::None);
    assert_eq!(unbound.counts.shadowed, 1);
}

/// What an unproven answer says is kept and marked, and is no evidence of where an account
/// runs.
#[test]
fn an_unproven_answer_is_marked_and_is_no_evidence_of_where_an_account_runs() {
    let row = account("claude", "one@lab.example", "claude-one");
    let pk = row.pk.clone();
    let mut lab = lab_of(&["m9"]);
    lab.accounts.insert(pk.clone(), row);
    let claims = || {
        MachineSeen::peer(
            "m9.local",
            "m9",
            Some("m3"),
            answered(
                1,
                vec![agent(
                    "s1",
                    Harness::Claude,
                    State::Busy,
                    Some("one@lab.example"),
                )],
            ),
        )
    };
    let unproven = claims().unproven();
    assert_eq!(unproven.label, None, "the label it claims is not taken");
    let tables_of = |seen: MachineSeen| {
        tables(&Inputs {
            lab: live(lab.clone(), NOW),
            read: read_of(vec![seen]),
            ..Inputs::default()
        })
    };
    let marked = tables_of(unproven);
    let m9 = &marked.machines[0];
    assert_eq!(
        m9.label.as_deref(),
        Some("m9"),
        "the machine the book dials"
    );
    assert_eq!((m9.reach, m9.proven), (Reach::Up, Some(false)));
    assert_eq!(shown_of(m9, "proven"), "false");
    assert_eq!(marked.agents.len(), 1, "its rows are kept");
    assert!(!marked.agents[0].proven);
    assert_eq!(shown_of(&marked.agents[0], "proven"), "false");
    assert_eq!(marked.agents[0].account, AccountRef::Resolved(pk.clone()));
    let account = &marked.accounts[0];
    assert_eq!(account.active, Some(Vec::new()), "a claim is no evidence");
    assert_eq!(account.agents, Some(0));
    assert_eq!(m9.accounts_active, Some(0));
    assert_eq!(kept(&marked, "agents", &["proven=false"]), 1);

    // Control: the same answer, proven, is evidence. (Its own label then names its row, so the
    // registry is given the machine it claims.)
    let mut named = lab.clone();
    let m3 = machine("m3");
    named.machines.insert(m3.pk.clone(), m3);
    let proven = tables(&Inputs {
        lab: live(named, NOW),
        read: read_of(vec![claims()]),
        ..Inputs::default()
    });
    let m3 = &proven.machines[0];
    assert_eq!((m3.label.as_deref(), m3.proven), (Some("m3"), Some(true)));
    assert!(proven.agents[0].proven);
    let account = &proven.accounts[0];
    assert_eq!(
        account
            .active
            .iter()
            .flatten()
            .map(|e| (e.on.name(), e.how))
            .collect::<Vec<_>>(),
        [("m3", EvidenceHow::Agent)]
    );
    assert_eq!(account.agents, Some(1));
    assert_eq!(m3.accounts_active, Some(1));
}

#[test]
fn a_table_prints_by_its_schema_and_a_relation_nobody_read_says_so() {
    let (_, tables) = fixture_tables();
    let printed = print(&tables, "machines", &[], false).expect("machines print");
    assert!(printed.read);
    let grid = grid_of(&printed.text);
    let header: Vec<&str> = rows::MACHINES
        .columns
        .iter()
        .map(|col| col.column.name)
        .collect();
    assert_eq!(grid[0], header);
    assert!(grid.iter().all(|line| line.len() == header.len()));

    assert_eq!(
        kept(&tables, "machines", &["status=planned"]),
        fixture::MACHINES - fixture::MACHINES_ACTIVE
    );
    assert_eq!(kept(&tables, "machines", &["reach=up", "how=self"]), 1);
    let unread = [grid::Where::parse("sessions=?").expect("a filter")];
    let json = json_rows(
        &print(&tables, "machines", &unread, true)
            .expect("print")
            .text,
    );
    assert_eq!(json.len(), fixture::MACHINES - fixture::MACHINES_ANSWERING);
    let stuck = tables
        .agents
        .iter()
        .filter(|row| row.state == State::Stuck)
        .count();
    assert!(stuck > 0);
    assert_eq!(kept(&tables, "agents", &["state=stuck"]), stuck);
    assert!(
        print(
            &tables,
            "machines",
            &[grid::Where::parse("nope=1").expect("a filter")],
            false
        )
        .is_err()
    );
    assert!(print(&tables, "sessions", &[], false).is_err());

    // The JSON answer carries each input's state beside the rows, in the schema's column order.
    let json = print(&tables, "agents", &[], true)
        .expect("agents print")
        .text;
    let value: Value = aterm_json::from_str(&json).expect("the table is JSON");
    assert_eq!(
        value.get("relation").and_then(Value::as_str),
        Some("agents")
    );
    assert_eq!(
        value.get("unresolved_accounts").and_then(Value::as_u64),
        Some(fixture::AGENTS_UNRESOLVED as u64)
    );
    let state = |input: &str| {
        value
            .get("sources")
            .and_then(|sources| sources.get(input))
            .and_then(|source| source.get("state"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    assert_eq!(state("registry").as_deref(), Some("live"));
    assert_eq!(state("fleet_read").as_deref(), Some("live"));
    let first = json.lines().nth(1).expect("a first row");
    assert!(first.starts_with("{\"machine\":"), "{first}");
    assert!(first.find("\"sid\":") < first.find("\"harness\":"));

    // With no fleet read held, the agents relation was not read: no rows, and it says so.
    let unread = FleetTables::unread();
    assert_eq!(
        unread,
        fold(&Inputs::default(), NOW).tables,
        "the tables nothing was offered to are what folding nothing gives"
    );
    let agents = print(&unread, "agents", &[], false).expect("agents print");
    assert!(!agents.read);
    assert_eq!(agents.text.lines().count(), 1, "the header alone");
    assert!(!print(&unread, "machines", &[], false).expect("print").read);
    assert!(!print(&unread, "accounts", &[], true).expect("print").read);
    assert_eq!(
        sources_line(&unread.sources),
        "registry=never journal=never network=never fleet_read=never"
    );
}

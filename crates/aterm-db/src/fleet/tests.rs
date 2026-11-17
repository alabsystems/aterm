// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet tables' laws: every registry row is a row, in the owner's order; a value nobody
//! read is never a zero; this Mac and an unregistered answer are rows; no agents row carries an
//! address; and where an account is expected comes from the registry's preferences alone.

use super::command::{print, print_schema, sources_line};
use super::fixture;
use super::fold::{AGENTS_KEEP_MS, FleetTables, Inputs, REACH_KEEP_MS, Source, fold};
use super::read::{
    AccountWord, AgentSeen, Answer, Answered, At, Harness, Latest, Look, MachineSeen,
    OUTPUT_KEEP_MS, OWN_ID, OutSrc, Rate, Read, SIGHTED_KEEP_MS, State, Tally, TurnFacts, Work,
    word,
};
use super::rows::{
    self, AccountRef, EvidenceHow, ObservedState, Reach, UNREGISTERED, Via, WindowState,
};
use super::{
    Folder, HELD_FLEET_UNREAD, HELD_NO_HISTORY, HELD_PARTIAL, HELD_READ_FAILED, HELD_STALE,
    HELD_UNOBSERVED, Reading,
};
use crate::alab::journal::event::{Event, SIGNIN_ENDED, SIGNIN_IN, SIGNIN_OUT};
use crate::alab::journal::place::{Placed, Why};
use crate::alab::journal::seen::{Claimed, Seen, SignedIn};
use crate::alab::view::{Account, LabView, Machine, MachineNickname, Preference};
use crate::relation::{Cell, Row};
use crate::typed::{cell, grid, keys};
use aterm_json::Value;
use aterm_types::network_state::{
    HistoryState, NetworkObservation, NetworkStateSnapshot, Visibility,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The instant every test folds at, Unix milliseconds.
const NOW: u64 = 1_800_000_000_000;
/// A Commons uuid of a test's own.
const UUID: &str = "000102030405060708090a0b0c0d0e0f";
/// A second one, for a second machine.
const UUID_2: &str = "101112131415161718191a1b1c1d1e1f";
/// One no machine row binds.
const UUID_UNBOUND: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff";
/// A third, for a second seat of a machine.
const UUID_3: &str = "202122232425262728292a2b2c2d2e2f";

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

fn account(realm: &str, address: &str, slug: &str) -> Account {
    Account {
        pk: keys::account_pk(realm, address).expect("a test address is an address"),
        provider: keys::provider_of_realm(realm).map(str::to_owned),
        realm: Some(realm.to_owned()),
        slug: slug.to_owned(),
        name: None,
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
    // An older aterm's row, its agent's word `busy` where it is busy.
    let turn = TurnFacts {
        word_turn: state == State::Busy,
        ..TurnFacts::OLDER
    };
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
        pinned: None,
        turn,
        tok_in: None,
        tok_out: None,
        tok_for_ms: None,
        turns: None,
        tools: None,
        work: Work::of(state, &turn),
        answer_at_ms: None,
        rate: None,
    }
}

/// An answer of `sessions` sessions listing `agents`, counted.
fn answered(sessions: u32, agents: Vec<AgentSeen>) -> Answer {
    let mut tally = Tally::default();
    for agent in &agents {
        tally.add(agent.state);
    }
    Answer::Answered(Box::new(Answered {
        build: Some("0.141.0".to_owned()),
        answer_at_ms: None,
        home_free_mb: Some(50_000),
        look: Look::UNREAD,
        access: None,
        drive: None,
        sessions: Some(sessions),
        tally: Some(tally),
        work: None,
        wait: None,
        agents,
        agents_more: 0,
    }))
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
    row.cells()[at].shown(&R::SCHEMA.columns[at].column.ty)
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

/// Every registry machine and account is one row, in order, and each account shows the slug
/// every command names it by.
/// NEGATIVE CONTROL: with `fold` filling `AccountRow.slug` from the stored label (`account.label
/// .clone()`, as before), each row shows the default label: `` left: Some("claude-…") right:
/// Some("claude-pool…-fixture") ``.
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

    // Every registry account is one row, by provider then slug: the slug every command names it
    // by, never its stored label.
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
        "accounts come by provider, then slug"
    );
    for row in &tables.accounts {
        let slug = row.slug.as_deref().expect("every account has a slug");
        assert_eq!(Some(slug), lab.slug(&row.account));
        assert_eq!(lab.accounts[&row.account].slug, slug);
    }

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

    // The accounts relation holds each login and prints it as it is: an address is ordinary
    // data.
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
            printed.contains("pool000@fixture.example"),
            "a login prints as it is"
        );
        assert!(!printed.contains("…@…"), "no login is abbreviated");
    }

    // A login another writer's build stored though it is no address reads the same on both
    // faces: withheld. NEGATIVE CONTROL: with `Cell::json` giving every cell's own JSON, the
    // JSON face prints the stored text and the first assertion fails for `json=true`.
    const STORED: &str = concat!("ghp", "_cccccccccccccccccccccccc");
    let mut odd = tables.clone();
    odd.accounts[0].login = Some(STORED.to_owned());
    for json in [false, true] {
        let printed = print(&odd, "accounts", &[], json)
            .expect("accounts print")
            .text;
        assert!(
            !printed.contains(STORED),
            "json={json}: a refused login is not printed"
        );
        assert!(printed.contains(grid::REFUSED), "json={json}");
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
fn an_account_word_is_resolved_by_its_harness_realm_or_its_slug() {
    let mut lab = lab_of(&["m3"]);
    lab.me = Some(keys::machine_pk("m3"));
    let claude = account("claude", "both@lab.example", "claude-both");
    let codex = account("codex", "both@lab.example", "oai-both");
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
        agent("s4", Harness::Other, State::Busy, Some("oai-both")),
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

/// A session whose footer read no address reports its roster row's label, which names the
/// account whose row it is in every form a build gave rows: the reserved slug a row a sync adds
/// takes now, the default `<cli>-<6 hex>` label an earlier build gave rows (until SF-10 re-keys
/// them), and the legacy label; the slug resolves as before. Ada's slug moved when the owner
/// relabelled her account, and the short handle it was now derives Bea's: neither of Ada's row
/// labels names Bea.
/// NEGATIVE CONTROL: with `accounts_by_name` naming by slug alone (its `rows` and legacy label
/// left out), only Bea's slug names an account: `` left: [Unresolved, Unresolved,
/// Resolved("ac-…"), Unresolved, Unresolved, Unresolved] ``.
#[test]
fn a_row_label_of_any_build_names_the_account_whose_row_it_is() {
    let mut lab = lab_of(&["m3"]);
    lab.me = Some(keys::machine_pk("m3"));
    let ada = account("claude", "ada.fb@alpha.example", "claude-m2-work");
    let bea = account("claude", "bea.fb@alpha.example", "claude-fb-alpha");
    let mut oai = account("codex", "cy.m8@beta.example", "oai-m8-beta");
    oai.legacy_label = Some("m8-2".to_owned());
    let (ada_pk, bea_pk, oai_pk) = (ada.pk.clone(), bea.pk.clone(), oai.pk.clone());
    let words = [
        keys::earlier_row_label("claude", &ada_pk).unwrap(),
        keys::reserved_slug("claude", &ada_pk),
        "claude-fb-alpha".to_owned(),
        keys::earlier_row_label("codex", &oai_pk).unwrap(),
        keys::reserved_slug("codex", &oai_pk),
        "m8-2".to_owned(),
    ];
    assert!(words[0].starts_with("claude-") && words[3].starts_with("codex-"));
    for account in [ada, bea, oai] {
        lab.accounts.insert(account.pk.clone(), account);
    }
    let agents: Vec<AgentSeen> = words
        .iter()
        .enumerate()
        .map(|(n, word)| {
            agent(
                &format!("s{n}"),
                Harness::Claude,
                State::Busy,
                Some(word.as_str()),
            )
        })
        .collect();
    let tables = tables(&Inputs {
        lab: live(lab, NOW),
        read: read_of(vec![MachineSeen::own(
            Some("m3"),
            answered(words.len() as u32, agents),
        )]),
        ..Inputs::default()
    });
    let accounts: Vec<&AccountRef> = tables.agents.iter().map(|row| &row.account).collect();
    assert_eq!(
        accounts,
        [
            &AccountRef::Resolved(ada_pk.clone()),
            &AccountRef::Resolved(ada_pk),
            &AccountRef::Resolved(bea_pk),
            &AccountRef::Resolved(oai_pk.clone()),
            &AccountRef::Resolved(oai_pk.clone()),
            &AccountRef::Resolved(oai_pk),
        ]
    );
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
    // The fixture's lab holds the name the guards of free text refuse: a Mac no machine row
    // binds is named by 8 digits of its uuid, and one such run is all decimal, which reads as
    // a phone number. It is a machine's name, and its cells admit it.
    // NEGATIVE CONTROL: with those cells typed as free text (as before), the check above fails
    // `accounts.seen_on holds [… {"at":1799206200000,"machine":"13000000","tabs":0}]`.
    let digits = &fixture::RETIRED_UUID[..8];
    assert!(digits.bytes().all(|b| b.is_ascii_digit()), "{digits}");
    assert!(!cell::admissible(
        &crate::typed::Ty::Text(32),
        &Value::from(digits)
    ));
    for named in ["seen_on", "last_seen_by", "observed_by"] {
        let spec = format!("{named}={digits}");
        assert!(kept(&tables, "accounts", &[&spec]) > 0, "{spec}");
    }

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
        one.contains("\"name\":\"login\"") && !one.contains("\"pii\""),
        "the login column is ordinary data: no column is marked"
    );
    assert!(print_schema(Some("nothing"), false).is_err());
}

/// THE FIXTURE'S MACHINES SAY WHAT A REAL ONE CAN: no use passes the capacity it is read
/// against (memory, swap, a link's traffic), so a pair or a ratio built on the fixture meets
/// no use over a zero capacity; and this Mac's own answer reads no drive grant, as its windows
/// answer a local read that asks for none. CONTROL: some machine has swap in use and some has a
/// grant, so neither law holds by everything being unread or zero.
#[test]
fn the_fixtures_looks_keep_use_within_capacity_and_its_own_mac_reads_no_grant() {
    let inputs = fixture::inputs(NOW);
    let latest = inputs.read.value().expect("the fixture's read");
    let within = |used: Option<u64>, total: Option<u64>| match (used, total) {
        (Some(used), Some(total)) => used <= total,
        (Some(used), None) => used == 0,
        (None, _) => true,
    };
    let (mut swapping, mut granted) = (0, 0);
    for (id, sighted) in &latest.machines {
        let Some(answered) = sighted.answered.as_ref().map(|at| &at.v) else {
            continue;
        };
        let look = &answered.look;
        assert!(
            within(look.mem_used_mb, look.mem_total_mb),
            "{id}: {look:?}"
        );
        assert!(
            within(look.swap_used_mb, look.swap_total_mb),
            "{id}: {look:?}"
        );
        if let Some(link) = look.net_link_kbps {
            assert!(within(look.net_rx_kbps, Some(link)), "{id}: {look:?}");
            assert!(within(look.net_tx_kbps, Some(link)), "{id}: {look:?}");
        }
        swapping += usize::from(look.swap_used_mb.is_some_and(|used| used > 0));
        granted += usize::from(answered.drive == Some(true));
        if id == OWN_ID {
            assert_eq!(answered.drive, None, "{answered:?}");
        }
    }
    assert!(swapping > 0 && granted > 0, "{swapping} {granted}");
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
    // This process's own folder is made without its fold thread, which its first ask starts.
    // Nothing else in this binary makes or asks it.
    let made = super::folder();
    assert!(
        !made.folding.load(Ordering::Acquire),
        "making this process's folder started its fold thread"
    );
    let _ = made.tables();
    assert!(
        !made.folding.load(Ordering::Acquire),
        "a waiting read started this process's fold thread"
    );
    let _ = made.try_tables();
    assert!(
        made.folding.load(Ordering::Acquire),
        "CONTROL: the first ask starts this process's fold thread"
    );
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
    fixture::name_accounts(&mut more);
    let counted = tables(&Inputs {
        lab: live(more, NOW),
        ..inputs.clone()
    });
    let zero = counted
        .accounts
        .iter()
        .find(|row| row.slug.as_deref() == Some("claude-two-lab"))
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

/// A LOAD SCORE IS TWO HUNDREDTHS OR NONE: `load=<level>:<over>` names the machine's dial in
/// hundredths, each part `0..=100`; `-`, one part alone, a third part, a sign, a decimal, an
/// empty part or one past 100 names no score, and a read `0:0` is a machine at rest. An over the
/// machine could not judge (`<level>:-`) reads unread beside its level, never an over of 0.
#[test]
fn a_load_score_is_two_hundredths_or_none() {
    use super::read::Load;
    let score = |level_x100, over_x100| {
        Some(Load {
            level_x100,
            over_x100,
        })
    };
    assert_eq!(Load::of("72:15"), score(72, Some(15)));
    assert_eq!(Load::of("0:0"), score(0, Some(0)));
    assert_eq!(Load::of("100:100"), score(100, Some(100)));
    assert_eq!(Load::of("007:012"), score(7, Some(12)));
    assert_eq!(Load::of("100:-"), score(100, None));
    for bad in [
        "-",
        "",
        ":",
        "72",
        "72:",
        ":15",
        "72:15:1",
        "101:0",
        "72:101",
        "-:15",
        "-:-",
        "72:--",
        "+7:15",
        "7.5:15",
        "x:y",
        "18446744073709551616:0",
    ] {
        assert_eq!(Load::of(bad), None, "{bad}");
    }
}

/// THE AGENTS RELATION SAYS WHETHER EACH AGENT IS PINNED, as its machine's row said it:
/// `aterm db fleet agents` prints `true`, `false`, or `?` (JSON `null`) for a row that said
/// neither, and the schema names the column. CONTROL: an unread pin is never printed `false`.
#[test]
fn an_agents_row_says_its_pin_and_an_unsaid_pin_is_unread() {
    let pinned = |sid: &str, pinned: Option<bool>| AgentSeen {
        pinned,
        ..agent(sid, Harness::Claude, State::Busy, None)
    };
    let tables = tables(&Inputs {
        lab: live(lab_of(&["m1"]), NOW),
        read: read_of(vec![peer(
            "m1",
            answered(
                3,
                vec![
                    pinned("s1", Some(true)),
                    pinned("s2", Some(false)),
                    pinned("s3", None),
                ],
            ),
        )]),
        ..Inputs::default()
    });
    let pins: Vec<(&str, Option<bool>)> = tables
        .agents
        .iter()
        .map(|row| (row.sid.as_str(), row.pinned))
        .collect();
    assert_eq!(
        pins,
        [("s1", Some(true)), ("s2", Some(false)), ("s3", None)]
    );
    let at = rows::AGENTS.position("pinned").expect("a pinned column");
    let cells: Vec<Cell> = tables
        .agents
        .iter()
        .map(|row| row.cells()[at].clone())
        .collect();
    assert_eq!(
        cells,
        [
            Cell::Value(Value::Bool(true)),
            Cell::Value(Value::Bool(false)),
            Cell::Unread
        ]
    );
    let json = print(&tables, "agents", &[], true)
        .expect("agents print")
        .text;
    assert!(json.contains("\"pinned\":true"), "{json}");
    assert!(json.contains("\"pinned\":false"), "{json}");
    assert!(json.contains("\"pinned\":null"), "{json}");
    let schema = print_schema(Some("agents"), false).expect("the schema prints");
    assert!(
        grid_of(&schema).iter().any(|line| line[1] == "pinned"),
        "{schema}"
    );
}

/// An agent of sid `sid` whose row sends its whole conversation's counters `(tok_in, tok_out)`,
/// counted for `for_ms` unbroken.
fn counting(sid: &str, tokens: (u64, u64), for_ms: u64) -> AgentSeen {
    AgentSeen {
        tok_in: Some(tokens.0),
        tok_out: Some(tokens.1),
        tok_for_ms: Some(for_ms),
        ..agent(sid, Harness::Claude, State::Busy, None)
    }
}

/// An answer of `agents`, answered at `answer_at_ms` on the machine's own clock: one reply, which
/// listed every agent.
fn answered_at(answer_at_ms: Option<u64>, agents: Vec<AgentSeen>) -> Answer {
    let agents = agents
        .into_iter()
        .map(|agent| AgentSeen {
            answer_at_ms,
            ..agent
        })
        .collect();
    replied(answer_at_ms, agents)
}

/// An answer of `agents`, answered at `answer_at_ms` (its first reply's time); each agent keeps
/// the time of the reply that listed it.
fn replied(answer_at_ms: Option<u64>, agents: Vec<AgentSeen>) -> Answer {
    let sessions = u32::try_from(agents.len()).expect("a few agents");
    let Answer::Answered(mut answer) = answered(sessions, agents) else {
        unreachable!("answered answers")
    };
    answer.answer_at_ms = answer_at_ms;
    Answer::Answered(answer)
}

/// Each agent's token rate as the reads so far say it, by sid, of the one machine `m1`.
fn rates_of(latest: &Latest) -> Vec<(String, Option<Rate>)> {
    let sighted = latest.machines.values().next().expect("m1 sighted");
    let answer = &sighted.answered.as_ref().expect("m1 answered").v;
    answer
        .agents
        .iter()
        .map(|agent| (agent.sid.clone(), agent.rate))
        .collect()
}

/// Two reads of `m1`: the first at `NOW` answering `then` at `then_at`, the second `read_gap`
/// later answering `now` at `now_at`. The reads so far after both. An agent that names its own
/// reply's time keeps it; the rest take the answer's.
fn two_reads(
    (then_at, then): (Option<u64>, Vec<AgentSeen>),
    read_gap: u64,
    (now_at, now): (Option<u64>, Vec<AgentSeen>),
) -> Latest {
    let stamped = |at: Option<u64>, agents: Vec<AgentSeen>| {
        let agents = agents
            .into_iter()
            .map(|agent| AgentSeen {
                answer_at_ms: agent.answer_at_ms.or(at),
                ..agent
            })
            .collect();
        peer("m1", replied(at, agents))
    };
    Latest::default()
        .with(whole(NOW, vec![stamped(then_at, then)]))
        .with(whole(NOW + read_gap, vec![stamped(now_at, now)]))
}

/// A RATE IS NEVER TAKEN AGAINST AN ANSWER OF ANOTHER PROOF STATE: the machine the book dials
/// as `m1` answers twice a minute apart; where one answer proved its node key and the other did
/// not (a claim, which any peer that read the sid could make), the later answer's agent has no
/// rate, whichever came first. Two proven answers give one, and so do two claims: a claim's
/// rate is kept as a claim, on a row marked unproven, as everything else a claim says.
/// CONTROL: the proof states not compared (a proven row's rate taken against a claim's
/// counters) is caught.
#[test]
fn a_rate_is_never_taken_against_an_answer_of_another_proof_state() {
    let answer = |unproven: bool, at: u64, agent: AgentSeen| {
        let seen = peer("m1", answered_at(Some(at), vec![agent]));
        if unproven { seen.unproven() } else { seen }
    };
    for (then_unproven, now_unproven, read) in [
        (false, false, true),
        (true, false, false),
        (false, true, false),
        (true, true, true),
    ] {
        let latest = Latest::default()
            .with(whole(
                NOW,
                vec![answer(then_unproven, NOW, counting("s1", (0, 0), 600_000))],
            ))
            .with(whole(
                NOW + 60_000,
                vec![answer(
                    now_unproven,
                    NOW + 60_000,
                    counting("s1", (9_000_000, 5_000_000), 600_000),
                )],
            ));
        assert_eq!(
            rates_of(&latest)[0].1.is_some(),
            read,
            "then unproven {then_unproven}, now unproven {now_unproven}"
        );
    }
}

/// A TOKEN RATE NEEDS TWO READS OF ONE SESSION: the first read of a session has none, however
/// large and however long-counted its totals; the second, a minute later, has what the counters
/// gained over that minute; a session the earlier answer did not list has none.
/// CONTROL: a rate made from one read (its totals over how long they counted) is caught.
#[test]
fn a_token_rate_needs_two_reads_of_one_session() {
    let first = Latest::default().with(whole(
        NOW,
        vec![peer(
            "m1",
            answered_at(Some(NOW), vec![counting("s1", (900_000, 60_000), 600_000)]),
        )],
    ));
    assert_eq!(rates_of(&first), [("s1".to_owned(), None)]);
    let latest = two_reads(
        (Some(NOW), vec![counting("s1", (900_000, 60_000), 600_000)]),
        60_000,
        (
            Some(NOW + 60_000),
            vec![
                counting("s1", (930_000, 61_200), 660_000),
                counting("s2", (5_000, 400), 900_000),
            ],
        ),
    );
    assert_eq!(
        rates_of(&latest),
        [
            (
                "s1".to_owned(),
                Some(Rate {
                    in_per_min: 30_000,
                    out_per_min: 1_200,
                    over_ms: 60_000
                })
            ),
            ("s2".to_owned(), None),
        ]
    );
    // Over two minutes, the gain is halved to a minute's.
    let latest = two_reads(
        (Some(NOW), vec![counting("s1", (900_000, 60_000), 600_000)]),
        120_000,
        (
            Some(NOW + 120_000),
            vec![counting("s1", (930_000, 61_200), 720_000)],
        ),
    );
    assert_eq!(
        rates_of(&latest)[0]
            .1
            .map(|rate| (rate.in_per_min, rate.out_per_min)),
        Some((15_000, 600))
    );
}

/// A COUNTER THAT WENT BACK GIVES NO RATE: input or output lower than the earlier read's is
/// another counter (a new conversation, a restart), never 0 tokens a minute. A counter the
/// earlier read did not hold gives none either. CONTROL: a saturating difference is caught.
#[test]
fn a_counter_that_went_back_gives_no_rate() {
    for (n, now) in [
        counting("s1", (899_999, 61_000), 660_000),
        counting("s1", (950_000, 59_999), 660_000),
        AgentSeen {
            tok_out: None,
            ..counting("s1", (950_000, 61_000), 660_000)
        },
    ]
    .into_iter()
    .enumerate()
    {
        let latest = two_reads(
            (Some(NOW), vec![counting("s1", (900_000, 60_000), 600_000)]),
            60_000,
            (Some(NOW + 60_000), vec![now]),
        );
        assert_eq!(rates_of(&latest), [("s1".to_owned(), None)], "{n}");
    }
    // The earlier read without its input counter: no rate.
    let latest = two_reads(
        (
            Some(NOW),
            vec![AgentSeen {
                tok_in: None,
                ..counting("s1", (900_000, 60_000), 600_000)
            }],
        ),
        60_000,
        (
            Some(NOW + 60_000),
            vec![counting("s1", (930_000, 61_000), 660_000)],
        ),
    );
    assert_eq!(rates_of(&latest), [("s1".to_owned(), None)]);
}

/// A RATE OVER TOO SHORT OR TOO LONG AN INTERVAL IS UNREAD: 30 s to 300 s, both ends kept.
/// CONTROL: either bound removed is caught.
#[test]
fn a_rate_over_too_short_or_too_long_an_interval_is_unread() {
    for (gap, read) in [
        (29_999, false),
        (30_000, true),
        (300_000, true),
        (300_001, false),
    ] {
        let latest = two_reads(
            (Some(NOW), vec![counting("s1", (900_000, 60_000), 600_000)]),
            gap,
            (
                Some(NOW + gap),
                vec![counting("s1", (930_000, 61_000), 3_600_000)],
            ),
        );
        let rate = rates_of(&latest)[0].1;
        assert_eq!(rate.is_some(), read, "{gap}");
        assert_eq!(rate.map(|rate| rate.over_ms), read.then_some(gap), "{gap}");
    }
}

/// A RATE IS TIMED BY THE ANSWERER'S CLOCK: two answers a minute apart on the machine's own
/// clock, heard by reads 90 s apart (the second waited on a slow peer), cover a minute. Where
/// either answer sends no time (an aterm before it), the two reads' times are the interval.
/// Where a machine answers in several replies (this Mac's windows), an agent is timed by the
/// reply that listed it, never by the first reply's clock: its counters were read there.
/// CONTROLS: the observer's read times used where both answers say their time is caught; the
/// answer's first reply timing an agent another reply listed is caught.
#[test]
fn a_rate_is_timed_by_the_answerers_clock() {
    const ANSWERER: u64 = 1_700_000_000_000;
    let latest = two_reads(
        (
            Some(ANSWERER),
            vec![counting("s1", (900_000, 60_000), 600_000)],
        ),
        90_000,
        (
            Some(ANSWERER + 60_000),
            vec![counting("s1", (930_000, 61_200), 660_000)],
        ),
    );
    assert_eq!(
        rates_of(&latest)[0].1,
        Some(Rate {
            in_per_min: 30_000,
            out_per_min: 1_200,
            over_ms: 60_000
        })
    );
    for (then_at, now_at) in [(None, Some(ANSWERER + 60_000)), (Some(ANSWERER), None)] {
        let latest = two_reads(
            (then_at, vec![counting("s1", (900_000, 60_000), 600_000)]),
            90_000,
            (now_at, vec![counting("s1", (930_000, 61_200), 690_000)]),
        );
        assert_eq!(
            rates_of(&latest)[0].1,
            Some(Rate {
                in_per_min: 20_000,
                out_per_min: 800,
                over_ms: 90_000
            }),
            "{then_at:?} {now_at:?}"
        );
    }
    // Two replies: the first (another window) answered at 1 000 000 and 1 060 000, the one that
    // lists s1 at 1 009 000 and 1 060 500, so s1's counters cover 51.5 s, not 60 s. They have
    // counted 55 s: a rate over s1's own interval, which the first reply's minute would refuse.
    let latest = two_reads(
        (
            Some(1_000_000),
            vec![AgentSeen {
                answer_at_ms: Some(1_009_000),
                ..counting("s1", (900_000, 100_000), 600_000)
            }],
        ),
        60_000,
        (
            Some(1_060_000),
            vec![AgentSeen {
                answer_at_ms: Some(1_060_500),
                ..counting("s1", (1_003_000, 151_500), 55_000)
            }],
        ),
    );
    assert_eq!(
        rates_of(&latest)[0].1,
        Some(Rate {
            in_per_min: 120_000,
            out_per_min: 60_000,
            over_ms: 51_500
        }),
        "the reply that listed it times it"
    );
    // An answerer's clock that went back gives no interval and no rate.
    let latest = two_reads(
        (
            Some(ANSWERER),
            vec![counting("s1", (900_000, 60_000), 600_000)],
        ),
        60_000,
        (
            Some(ANSWERER - 1),
            vec![counting("s1", (930_000, 61_200), 660_000)],
        ),
    );
    assert_eq!(rates_of(&latest)[0].1, None);
}

/// A COUNTER YOUNGER THAN THE INTERVAL GIVES NO RATE: counters that have counted this
/// conversation for less than the time between the reads began inside it (a resumed
/// conversation's whole total, a catch-up), so their gain is no minute's work; unsaid, they
/// give none; counted exactly as long, they give one. CONTROL: `tok_for_ms` ignored is caught.
#[test]
fn a_counter_younger_than_the_interval_gives_no_rate() {
    for (for_ms, read) in [
        (Some(45_000), false),
        (Some(59_999), false),
        (None, false),
        (Some(60_000), true),
    ] {
        let latest = two_reads(
            (Some(NOW), vec![counting("s1", (10_000, 1_000), 600_000)]),
            60_000,
            (
                Some(NOW + 60_000),
                vec![AgentSeen {
                    tok_for_ms: for_ms,
                    ..counting("s1", (2_400_000, 90_000), 0)
                }],
            ),
        );
        assert_eq!(rates_of(&latest)[0].1.is_some(), read, "{for_ms:?}");
    }
}

/// An agent in an open turn whose row says `step_ms`, `out_ms` and `out_src` as given.
fn in_turn(step_ms: Option<u64>, out: Option<(u64, OutSrc)>) -> TurnFacts {
    TurnFacts {
        turn_open: Some(true),
        turn_age_ms: Some(1_800_000),
        turn_end_age_ms: Some(3_600_000),
        step_age_ms: step_ms,
        out_age_ms: out.map(|(ms, _)| ms),
        out_src: out.map(|(_, src)| src),
        bg: Some(0),
        older: false,
        word_turn: true,
    }
}

/// AN AGENT AT A BOX IS ASKING, ONE IN A LONG TOOL CALL IS TOOL, AND NEITHER IS NO OUTPUT: in a
/// turn, an agent waiting on a person is `asking` however old its output, and one with a step
/// open (a 25-minute build writes no transcript row) is `tool`. Without either, output older
/// than ten minutes is `no-output`; transcript output within them `with-output`; a fresh screen
/// alone (a spinner) or no output age `output-unread`. A wall is `held` and an unknown state
/// `unknown`, before the turn is asked. CONTROL: the step ignored, and `waiting` classed by its
/// output age, are caught.
#[test]
fn an_agent_at_a_box_is_asking_one_in_a_long_tool_call_is_tool_and_neither_is_no_output() {
    let stale = Some((25 * 60_000, OutSrc::Transcript));
    let fresh = Some((120_000, OutSrc::Transcript));
    let cases = [
        (State::Waiting, in_turn(None, stale), Work::Asking),
        (State::Waiting, in_turn(None, fresh), Work::Asking),
        (State::Busy, in_turn(Some(25 * 60_000), stale), Work::Tool),
        (State::Busy, in_turn(None, stale), Work::NoOutput),
        (
            State::Busy,
            in_turn(None, Some((25 * 60_000, OutSrc::Screen))),
            Work::NoOutput,
        ),
        (State::Busy, in_turn(None, fresh), Work::WithOutput),
        (
            State::Busy,
            in_turn(None, Some((OUTPUT_KEEP_MS, OutSrc::Transcript))),
            Work::WithOutput,
        ),
        (
            State::Busy,
            in_turn(None, Some((120_000, OutSrc::Screen))),
            Work::OutputUnread,
        ),
        (State::Busy, in_turn(None, None), Work::OutputUnread),
        (State::Walled, in_turn(None, fresh), Work::Held),
        (State::Unknown, in_turn(None, fresh), Work::Unknown),
        (
            State::Waiting,
            TurnFacts {
                turn_open: Some(false),
                ..in_turn(None, stale)
            },
            Work::NoTurn,
        ),
    ];
    for (n, (state, row, work)) in cases.into_iter().enumerate() {
        assert_eq!(Work::of(state, &row), work, "{n}: {state:?} {row:?}");
    }
    assert_eq!(
        Work::WORDS,
        [
            Work::Held,
            Work::NoTurn,
            Work::Asking,
            Work::Tool,
            Work::WithOutput,
            Work::NoOutput,
            Work::OutputUnread,
            Work::Unknown
        ]
        .map(Work::word)
    );
}

/// BACKGROUND WORK IS A TURN ONLY WITH OUTPUT AFTER THE LAST TURN CLOSED: an agent with no turn
/// open and a background shell is in a turn where its transcript wrote output after its last
/// turn closed and within ten minutes; output older than the close is the closed turn's own, a
/// screen that moved proves nothing, and with no background work an idle agent is in no turn.
/// CONTROL: `bg` ignored, and an idle agent with a background shell taken as in a turn, are
/// caught.
#[test]
fn background_work_is_a_turn_only_with_output_after_the_last_turn_closed() {
    let idle = |bg: Option<u32>, out: Option<(u64, OutSrc)>, end: Option<u64>| TurnFacts {
        turn_open: Some(false),
        turn_age_ms: None,
        turn_end_age_ms: end,
        step_age_ms: None,
        out_age_ms: out.map(|(ms, _)| ms),
        out_src: out.map(|(_, src)| src),
        bg,
        older: false,
        word_turn: false,
    };
    let after = Some((120_000, OutSrc::Transcript));
    let cases = [
        (idle(Some(1), after, Some(300_000)), Work::WithOutput),
        (idle(Some(2), after, None), Work::WithOutput),
        (
            idle(Some(1), Some((360_000, OutSrc::Transcript)), Some(300_000)),
            Work::NoTurn,
        ),
        (
            idle(Some(1), Some((300_000, OutSrc::Transcript)), Some(300_000)),
            Work::NoTurn,
        ),
        (
            idle(Some(1), Some((720_000, OutSrc::Transcript)), Some(900_000)),
            Work::NoTurn,
        ),
        (
            idle(Some(1), Some((120_000, OutSrc::Screen)), Some(300_000)),
            Work::NoTurn,
        ),
        (idle(Some(1), None, Some(300_000)), Work::NoTurn),
        (idle(Some(0), after, Some(300_000)), Work::NoTurn),
        (idle(None, after, Some(300_000)), Work::NoTurn),
    ];
    for (n, (row, work)) in cases.into_iter().enumerate() {
        assert_eq!(Work::of(State::Idle, &row), work, "{n}: {row:?}");
    }
}

/// AN OLDER ROW IS IN A TURN WHERE ITS AGENT'S OWN WORD OPENS ONE: a row that says no `turn=`
/// (an aterm before the turn) is classed as that aterm's session log would class it, by the
/// word its turn tracker reads: `busy` and a box (`prompt`) open a turn, so an agent waiting at
/// a box is `asking` (its state `waiting`, whatever its output), and a busy or stalled one is
/// in a turn with its output unread; `question` (the agent ended its turn on one) and `idle`
/// close it, so an agent that waits on a person after its turn is in none. A row that says
/// `turn=` is never classed by its word.
/// CONTROLS: the fallback read from the state alone (`busy`, an agent at a box in no turn),
/// and from `busy` or `waiting` (an agent that ended its turn on a question asking), are caught.
#[test]
fn an_older_row_is_in_a_turn_where_its_agents_own_word_opens_one() {
    let older = |word_turn: bool| TurnFacts {
        word_turn,
        ..TurnFacts::OLDER
    };
    let cases = [
        (State::Busy, older(true), Work::OutputUnread),
        (State::Stuck, older(true), Work::OutputUnread),
        (State::Waiting, older(true), Work::Asking),
        (State::Waiting, older(false), Work::NoTurn),
        (State::Idle, older(false), Work::NoTurn),
        (State::Walled, older(true), Work::Held),
        (State::Unknown, older(true), Work::Unknown),
        (
            State::Busy,
            TurnFacts {
                turn_open: Some(false),
                older: false,
                ..older(true)
            },
            Work::NoTurn,
        ),
    ];
    for (n, (state, row, work)) in cases.into_iter().enumerate() {
        assert_eq!(Work::of(state, &row), work, "{n}: {state:?} {row:?}");
    }
}

/// The journal event `value` is, as the Mac of Commons uuid `uuid` logged it.
fn logged(uuid: &str, value: Value) -> (String, Event) {
    (
        uuid.to_owned(),
        Event::from_value(&value).expect("an event of the journal's vocabulary"),
    )
}

/// A lab of m3 (bound to [`UUID`]) and m22 (bound to [`UUID_2`]) with the Claude accounts
/// `<letter>@lab.example` of `letters`, named as `alab::account_names` names them
/// ([`fixture::name_accounts`]), so their rows come in that order, and their keys.
fn lab_with_accounts(letters: &[&str]) -> (LabView, Vec<String>) {
    let mut lab = lab_of(&["m3", "m22"]);
    lab.bound.insert(UUID.to_owned(), keys::machine_pk("m3"));
    lab.bound.insert(UUID_2.to_owned(), keys::machine_pk("m22"));
    let mut pks = Vec::new();
    for letter in letters {
        let mut row = account(
            "claude",
            &format!("{letter}@lab.example"),
            &format!("claude-{letter}"),
        );
        row.status = Some("pending".to_owned());
        pks.push(row.pk.clone());
        lab.accounts.insert(row.pk.clone(), row);
    }
    fixture::name_accounts(&mut lab);
    (lab, pks)
}

/// A PLAN IS THE REGISTRY'S WHERE THE REGISTRY STATES ONE, ELSE THE NEWEST A MAC OBSERVED, AND
/// THE ROW SAYS WHICH: `plan_source` is `set` or `observed`, the observed plan stands in its own
/// columns with when it was read and the machine that read it, and where a stated plan and an
/// observed one differ both are shown and `plan_mismatch` says so. The stated plan is a
/// person's: no observation is written over it.
/// NEGATIVE CONTROLS: with the fold taking `account.plan` alone (as before), the row nobody
/// stated a plan for shows none: `left: None right: Some("anthropic-max")`; with the observed
/// plan standing over the stated one, the differing row reads `left: Some("anthropic-max")
/// right: Some("anthropic-pro")`.
#[test]
fn a_plan_is_the_registrys_where_stated_else_the_newest_observed_and_a_mismatch_shows_both() {
    let now_s = NOW / 1_000;
    let (mut lab, pks) = lab_with_accounts(&["a", "b", "c", "d", "e"]);
    for (pk, plan) in [
        (0, "anthropic-max"),
        (2, "anthropic-pro"),
        (3, "anthropic-max"),
    ] {
        if let Some(row) = lab.accounts.get_mut(&pks[pk]) {
            row.plan = Some(plan.to_owned());
        }
    }
    let plan_seen = |uuid: &str, pk: &str, plan: &str, ago: u64| {
        logged(
            uuid,
            aterm_json::json!({"a": pk, "c": "claude", "k": "plan-seen", "src": "cli-output",
                "t": now_s - ago, "plan": plan}),
        )
    };
    // b: two Macs saw two plans, the newer logged first. c: a Mac no machine row binds saw
    // another plan than the stated one. d: a Mac saw the stated one.
    let seen = Seen::of_events(
        &[
            plan_seen(UUID_2, &pks[1], "max", 50),
            plan_seen(UUID, &pks[1], "pro", 100),
            plan_seen(UUID_UNBOUND, &pks[2], "max", 30),
            plan_seen(UUID, &pks[3], "max", 10),
        ],
        now_s,
    );
    let inputs = Inputs {
        lab: live(lab.clone(), NOW),
        journal: live(seen, NOW),
        ..Inputs::default()
    };
    let tables = tables(&inputs);
    let [stated, observed, differs, agrees, neither] = &tables.accounts[..] else {
        panic!("five accounts: {}", tables.accounts.len());
    };
    let plan = |row: &rows::AccountRow| {
        [
            shown_of(row, "plan"),
            shown_of(row, "plan_source"),
            shown_of(row, "plan_observed"),
            shown_of(row, "plan_by"),
            shown_of(row, "plan_mismatch"),
        ]
    };
    assert_eq!(plan(stated), ["anthropic-max", "set", "?", "?", "?"]);
    assert_eq!(cell_of(stated, "plan_at"), Cell::Unread);
    // The newest observation stands, whoever made it, with its time and its Mac.
    assert_eq!(observed.plan.as_deref(), Some("anthropic-max"));
    assert_eq!(
        plan(observed),
        ["anthropic-max", "observed", "anthropic-max", "m22", "?"]
    );
    assert_eq!(
        cell_of(observed, "plan_at"),
        Cell::Value(Value::from((now_s - 50) * 1_000))
    );
    // A stated plan stands beside an observed one that differs: both, and the mismatch.
    assert_eq!(differs.plan.as_deref(), Some("anthropic-pro"));
    assert_eq!(
        plan(differs),
        ["anthropic-pro", "set", "anthropic-max", "f0f1f2f3", "true"]
    );
    assert_eq!(
        plan(agrees),
        ["anthropic-max", "set", "anthropic-max", "m3", "false"]
    );
    assert_eq!(plan(neither), ["?", "?", "?", "?", "?"]);
    assert_eq!(kept(&tables, "accounts", &["plan_mismatch=true"]), 1);
    assert_eq!(kept(&tables, "accounts", &["plan_source=observed"]), 1);
    assert_eq!(kept(&tables, "accounts", &["plan_by=m22"]), 1);

    // The JSON carries the same cells, and a plan nobody observed is null there.
    let json = json_rows(&print(&tables, "accounts", &[], true).expect("print").text);
    assert_eq!(json[1]["plan"], Value::from("anthropic-max"));
    assert_eq!(json[1]["plan_source"], Value::from("observed"));
    assert_eq!(json[1]["plan_at"], Value::from((now_s - 50) * 1_000));
    assert_eq!(json[1]["plan_by"], Value::from("m22"));
    assert_eq!(json[2]["plan_mismatch"], Value::Bool(true));
    assert_eq!(json[0]["plan_observed"], Value::Null);
    assert_eq!(json[4]["plan_source"], Value::Null);

    // Control: with no journal read, what the registry states still stands and nothing is
    // observed.
    let unread = self::tables(&Inputs {
        lab: live(lab, NOW),
        ..Inputs::default()
    });
    assert_eq!(
        plan(&unread.accounts[0]),
        ["anthropic-max", "set", "?", "?", "?"]
    );
    assert_eq!(plan(&unread.accounts[1]), ["?", "?", "?", "?", "?"]);

    // The fixture holds all three cases: a plan only observed, one observed as stated and one
    // observed otherwise.
    let (_, fixture) = fixture_tables();
    assert!(kept(&fixture, "accounts", &["plan_source=observed"]) > 0);
    assert!(kept(&fixture, "accounts", &["plan_mismatch=true"]) > 0);
    assert!(kept(&fixture, "accounts", &["plan_mismatch=false"]) > 0);
}

/// A STATED PLAN THAT ADDS A TIER TO THE OBSERVED ONE IS NO MISMATCH. A CLI reports its plan's
/// word and no tier, so an account stated as `anthropic-max-20x` that a Mac saw on `max` is on
/// the plan the registry states; a stated key that is another plan (`anthropic-max-5x` beside
/// an observed `pro`), or that only begins with the observed word (`openai-prolite` beside an
/// observed `pro`), still differs, and both plans are shown either way.
/// NEGATIVE CONTROLS: with `plans_differ` comparing the two keys for equality (as before), the
/// Max 20x row reads `left: ["anthropic-max-20x", "anthropic-max", "true"]`; with it taking any
/// stated key the observed one begins, the Pro Lite row reads
/// `left: ["openai-prolite", "openai-pro", "false"]`.
#[test]
fn a_stated_plan_that_adds_a_tier_to_the_observed_one_is_no_mismatch() {
    let now_s = NOW / 1_000;
    let (mut lab, mut pks) = lab_with_accounts(&["a", "b"]);
    let mut codex = account("codex", "c@lab.example", "codex-c");
    codex.plan = Some("openai-prolite".to_owned());
    pks.push(codex.pk.clone());
    lab.accounts.insert(codex.pk.clone(), codex);
    for (pk, plan) in [(0, "anthropic-max-20x"), (1, "anthropic-max-5x")] {
        if let Some(row) = lab.accounts.get_mut(&pks[pk]) {
            row.plan = Some(plan.to_owned());
        }
    }
    let plan_seen = |pk: &str, cli: &str, plan: &str| {
        logged(
            UUID,
            aterm_json::json!({"a": pk, "c": cli, "k": "plan-seen", "src": "cli-output",
                "t": now_s - 10, "plan": plan}),
        )
    };
    let seen = Seen::of_events(
        &[
            plan_seen(&pks[0], "claude", "max"),
            plan_seen(&pks[1], "claude", "pro"),
            plan_seen(&pks[2], "codex", "pro"),
        ],
        now_s,
    );
    let tables = tables(&Inputs {
        lab: live(lab, NOW),
        journal: live(seen, NOW),
        ..Inputs::default()
    });
    let shown: BTreeMap<&str, [String; 3]> = tables
        .accounts
        .iter()
        .map(|row| {
            (
                row.account.as_str(),
                [
                    shown_of(row, "plan"),
                    shown_of(row, "plan_observed"),
                    shown_of(row, "plan_mismatch"),
                ],
            )
        })
        .collect();
    assert_eq!(
        shown[pks[0].as_str()],
        ["anthropic-max-20x", "anthropic-max", "false"]
    );
    assert_eq!(
        shown[pks[1].as_str()],
        ["anthropic-max-5x", "anthropic-pro", "true"]
    );
    assert_eq!(
        shown[pks[2].as_str()],
        ["openai-prolite", "openai-pro", "true"]
    );
    assert_eq!(kept(&tables, "accounts", &["plan_mismatch=true"]), 2);
}

/// AN OBSERVED PLAN THAT CARRIES ITS TIER IS COMPARED WITH IT. A Mac that saw `max-20x` agrees
/// with an account stated on Max 20x, differs from one stated on Max 5x, and differs from one
/// stated on the tierless `anthropic-max`: the registry states less than the Mac saw.
/// NEGATIVE CONTROL: with `plans_differ` also accepting an observed key that extends the
/// stated one, the tierless row reads `left: ["anthropic-max-20x", "false"]`.
#[test]
fn an_observed_tier_is_compared_with_the_stated_one() {
    let now_s = NOW / 1_000;
    let (mut lab, pks) = lab_with_accounts(&["a", "b", "c"]);
    let stated = ["anthropic-max-20x", "anthropic-max-5x", "anthropic-max"];
    for (pk, plan) in pks.iter().zip(stated) {
        if let Some(row) = lab.accounts.get_mut(pk) {
            row.plan = Some(plan.to_owned());
        }
    }
    let events: Vec<_> = pks
        .iter()
        .map(|pk| {
            logged(
                UUID,
                aterm_json::json!({"a": pk, "c": "claude", "k": "plan-seen",
                    "src": "cli-output", "t": now_s - 10, "plan": "max-20x"}),
            )
        })
        .collect();
    let tables = tables(&Inputs {
        lab: live(lab, NOW),
        journal: live(Seen::of_events(&events, now_s), NOW),
        ..Inputs::default()
    });
    let mismatch: BTreeMap<&str, [String; 2]> = tables
        .accounts
        .iter()
        .map(|row| {
            (
                row.account.as_str(),
                [
                    shown_of(row, "plan_observed"),
                    shown_of(row, "plan_mismatch"),
                ],
            )
        })
        .collect();
    for (pk, differs) in pks.iter().zip(["false", "true", "true"]) {
        assert_eq!(
            mismatch[pk.as_str()],
            ["anthropic-max-20x", differs],
            "{pk}"
        );
    }
}

/// One Mac's sign-in word for an account, as the journal's read holds it.
fn sign(seen: &mut Seen, pk: &str, uuid: &str, state: &'static str, t: u64) {
    seen.signins.entry(pk.to_owned()).or_default().insert(
        uuid.to_owned(),
        SignedIn {
            state,
            t,
            cli: "claude".to_owned(),
        },
    );
}

/// WHAT THE FLEET OBSERVED OF AN ACCOUNT STANDS BESIDE THE STATUS THE REGISTRY STATES, AND
/// NEVER REPLACES IT: `observed` is derived each fold from the evidence (signed in on any Mac
/// is `signed-in`; a Mac's word that is not `in`, with no `in` anywhere, is `signed-out`; a
/// place with no word is `place-only`; nothing is `never-seen`), with since when the newest
/// evidence holds and the Mac it is of, while `status` stays the registry's word: a pending
/// row a Mac is signed in to still says `pending`, and a paused one still says `paused`.
/// NEGATIVE CONTROLS: with `observed_state` not reading the words that are not `in`, the
/// signed-out row reads `left: ["pending", "place-only", "m3"] right: ["pending",
/// "signed-out", "m22"]`; with the fold giving an
/// observed sign-in as the row's status, the first row reads `left: Some("active") right:
/// Some("pending")`.
#[test]
fn what_was_observed_of_an_account_stands_beside_its_status_and_never_replaces_it() {
    let now_s = NOW / 1_000;
    let (mut lab, pks) = lab_with_accounts(&["a", "b", "c", "d", "e", "f"]);
    if let Some(row) = lab.accounts.get_mut(&pks[4]) {
        row.status = Some("paused".to_owned());
    }
    let mut seen = Seen::of_events(&[], now_s);
    // a: two Macs are signed in, m22 the later. b: one Mac's login ended and another signed
    // out later, and the first still holds a place. c: a place and no word. d: nothing.
    // e: paused by a person, and a Mac is signed in. f: only a Mac no machine row binds.
    sign(&mut seen, &pks[0], UUID, SIGNIN_IN, now_s - 200);
    sign(&mut seen, &pks[0], UUID_2, SIGNIN_IN, now_s - 100);
    sign(&mut seen, &pks[1], UUID, SIGNIN_ENDED, now_s - 300);
    sign(&mut seen, &pks[1], UUID_2, SIGNIN_OUT, now_s - 50);
    for pk in [&pks[1], &pks[2]] {
        seen.places.rows.push(Placed {
            uuid: UUID.to_owned(),
            account: pk.clone(),
            why: Why::Fill,
            since: now_s - 60,
            until: now_s + 60,
        });
    }
    sign(&mut seen, &pks[4], UUID, SIGNIN_IN, now_s - 10);
    sign(&mut seen, &pks[5], UUID_UNBOUND, SIGNIN_IN, now_s - 10);
    let inputs = Inputs {
        lab: live(lab.clone(), NOW),
        journal: live(seen, NOW),
        ..Inputs::default()
    };
    let folded = fold(&inputs, NOW);
    let tables = &folded.tables;
    let observed = |row: &rows::AccountRow| {
        [
            shown_of(row, "status"),
            shown_of(row, "observed"),
            shown_of(row, "observed_by"),
        ]
    };
    let at = |row: &rows::AccountRow| cell_of(row, "observed_at");
    let ms = |ago: u64| Cell::Value(Value::from((now_s - ago) * 1_000));
    let rows = &tables.accounts;
    assert_eq!(rows[0].status, Some("pending"));
    assert_eq!(observed(&rows[0]), ["pending", "signed-in", "m22"]);
    assert_eq!(at(&rows[0]), ms(100));
    assert_eq!(observed(&rows[1]), ["pending", "signed-out", "m22"]);
    assert_eq!(at(&rows[1]), ms(50));
    assert_eq!(observed(&rows[2]), ["pending", "place-only", "m3"]);
    assert_eq!(at(&rows[2]), ms(60));
    // Nothing seen is known: no evidence to date, which is empty and not unread.
    assert_eq!(observed(&rows[3]), ["pending", "never-seen", ""]);
    assert_eq!(at(&rows[3]), Cell::Empty);
    // A person's hold stands whatever a Mac observed, and the observation is beside it.
    assert_eq!(observed(&rows[4]), ["paused", "signed-in", "m3"]);
    // A Mac no live machine row binds is no Mac of the lab: its word places nothing.
    assert_eq!(observed(&rows[5]), ["pending", "never-seen", ""]);
    assert_eq!(rows[5].active.as_deref(), Some(&[][..]));
    assert_eq!(kept(tables, "accounts", &["status=pending"]), 5);
    assert_eq!(kept(tables, "accounts", &["observed=signed-in"]), 2);
    assert_eq!(
        kept(
            tables,
            "accounts",
            &["status=pending", "observed=signed-in"]
        ),
        1
    );
    let json = json_rows(&print(tables, "accounts", &[], true).expect("print").text);
    assert_eq!(json[0]["status"], Value::from("pending"));
    assert_eq!(json[0]["observed"], Value::from("signed-in"));
    assert_eq!(json[0]["observed_at"], Value::from((now_s - 100) * 1_000));
    assert_eq!(json[0]["observed_by"], Value::from("m22"));
    assert_eq!(json[3]["observed_at"], Value::Null);

    // A place ends at the instant the fold names, and what was observed changes then and not
    // before: the place-only row has never been seen signed in, and now is on no Mac.
    let ends = (now_s + 60) * 1_000;
    assert_eq!(folded.next_change_ms, Some(ends));
    assert_eq!(fold(&inputs, ends - 1).tables, *tables);
    let after = fold(&inputs, ends).tables;
    assert_eq!(after.accounts[2].observed, Some(ObservedState::NeverSeen));
    assert_eq!(after.accounts[1].observed, Some(ObservedState::SignedOut));

    // Control: with no journal read, no Mac's word was read, and what was observed is unread,
    // not never-seen.
    let unread = self::tables(&Inputs {
        lab: live(lab, NOW),
        ..Inputs::default()
    });
    assert_eq!(observed(&unread.accounts[0]), ["pending", "?", "?"]);
    assert_eq!(at(&unread.accounts[0]), Cell::Unread);

    // The fixture's lab holds the case this column is for: accounts the registry still calls
    // pending that a Mac is signed in to; and every state.
    let (_, fixture) = fixture_tables();
    assert!(
        kept(
            &fixture,
            "accounts",
            &["status=pending", "observed=signed-in"]
        ) > 0
    );
    for state in ObservedState::WORDS {
        assert!(
            kept(&fixture, "accounts", &[&format!("observed={state}")]) > 0,
            "{state}"
        );
    }
}

/// AN ACCOUNT ROW SAYS WHEN IT WAS LAST SEEN AND ON WHICH MACS: every Mac whose login files
/// name it (its `account-obs` row, a tab's `seen`, the machine sweep's `seen`), each with its
/// own newest sighting and the tabs that run it now, a Mac no machine row binds by the first 8
/// digits of its uuid. A sighting nobody noted a sign-in for is what was observed (`seen`). A
/// time is never after its read, a record dated past the clock-skew bound ahead of its read
/// is no sighting at all, a tab count lapses at the instant the fold names, and where the
/// sightings were read and none names the account the cells are empty, not unread.
/// NEGATIVE CONTROLS: with the fold listing only the Macs a tab runs it on (as the registry
/// view's `seen_on` does), the first row's `seen_on` lacks its third item, the sweep's Mac
/// `f0f1f2f3`; with a sighting's time not held to its read, `the clock ahead` fails `left:
/// Value(Number(Number(Unsigned(1800000300000)))) right:
/// Value(Number(Number(Unsigned(1800000000000))))`; with the fold not offering a sighting's
/// lapse to `next`, the instant reads `left: None right: Some(1800006600001)`; with the
/// sightings' fold believing a record however far ahead of its read it is dated, `a day
/// ahead` fails `left: Value(Number(Number(Unsigned(1800000000000)))) right: Empty`.
#[test]
fn an_account_says_when_it_was_last_seen_and_on_which_macs() {
    use crate::alab::sighted::{Obs, ObsState, SEEN_LIVE_MS};
    use aterm_json::json;
    let now_s = NOW / 1_000;
    let (mut lab, pks) = lab_with_accounts(&["a", "b", "c", "d", "e"]);
    let obs = |pk: &str, uuid: &str, at_ms: u64, tabs: u64| Obs {
        account: pk.to_owned(),
        uuid: uuid.to_owned(),
        at_ms,
        state: if tabs > 0 {
            ObsState::Running
        } else {
            ObsState::Idle
        },
        tabs,
    };
    // a: m3's row says two tabs run it; a tab on m22 was seen later; a Mac no machine row
    // binds found its login place two days ago. b: only that Mac's sweep. c: nothing.
    // d: m3's row is dated five minutes ahead of this Mac's clock, inside the clock-skew
    // bound. e: m3's row says two tabs run it, a day ahead.
    lab.account_obs = vec![
        obs(&pks[0], UUID, NOW - 600_000, 2),
        obs(&pks[3], UUID, NOW + 300_000, 0),
        obs(&pks[4], UUID, NOW + 86_400_000, 2),
    ];
    let seen_at = |pk: &str, ago_s: u64, tab: Option<&str>| {
        let mut value = json!({
            "a": pk, "c": "claude", "k": "seen", "src": "profile", "t": now_s - ago_s,
        });
        if let (Some(tab), Some(members)) = (tab, value.as_object_mut()) {
            members.insert("x".to_owned(), Value::from(tab));
        }
        value
    };
    let seen = Seen::of_events(
        &[
            logged(UUID_2, seen_at(&pks[0], 300, Some("0000aaaa"))),
            logged(UUID_UNBOUND, seen_at(&pks[0], 172_800, None)),
            logged(UUID_UNBOUND, seen_at(&pks[1], 172_800, None)),
        ],
        now_s,
    );
    let inputs = Inputs {
        lab: live(lab.clone(), NOW),
        journal: live(seen, NOW),
        ..Inputs::default()
    };
    let folded = fold(&inputs, NOW);
    let tables = &folded.tables;
    let rows = &tables.accounts;
    let time = |at_ms: u64| Cell::Value(Value::from(at_ms));
    let on = |machine: &str, tabs: u64, at_ms: u64| json!({"machine": machine, "tabs": tabs, "at": at_ms});
    let days_ago = NOW - 172_800_000;
    assert_eq!(cell_of(&rows[0], "last_seen"), time(NOW - 300_000));
    assert_eq!(
        cell_of(&rows[0], "seen_on"),
        Cell::Value(Value::Array(vec![
            on("m3", 2, NOW - 600_000),
            on("m22", 1, NOW - 300_000),
            on("f0f1f2f3", 0, days_ago),
        ]))
    );
    // No Mac states a sign-in word for it: what was observed is that it was seen, last on m22.
    assert_eq!(shown_of(&rows[0], "observed"), "seen");
    assert_eq!(shown_of(&rows[0], "observed_by"), "m22");
    assert_eq!(cell_of(&rows[0], "observed_at"), time(NOW - 300_000));
    assert_eq!(cell_of(&rows[1], "last_seen"), time(days_ago));
    assert_eq!(
        cell_of(&rows[1], "seen_on"),
        Cell::Value(Value::Array(vec![on("f0f1f2f3", 0, days_ago)]))
    );
    assert_eq!(shown_of(&rows[1], "observed_by"), "f0f1f2f3");
    // Read, and nothing names it: empty and not unread.
    assert_eq!(cell_of(&rows[2], "last_seen"), Cell::Empty);
    assert_eq!(
        cell_of(&rows[2], "seen_on"),
        Cell::Value(Value::Array(vec![]))
    );
    assert_eq!(shown_of(&rows[2], "observed"), "never-seen");
    // A row dated ahead of its read was seen no later than it was read.
    assert_eq!(cell_of(&rows[3], "last_seen"), time(NOW), "the clock ahead");
    assert_eq!(shown_of(&rows[3], "last_seen_by"), "m3");
    // One dated past the clock-skew bound ahead was written by a clock split from this one: it
    // is no sighting, it runs no tab, and the Mac is not the one that saw the account last.
    assert_eq!(cell_of(&rows[4], "last_seen"), Cell::Empty, "a day ahead");
    assert_eq!(
        cell_of(&rows[4], "seen_on"),
        Cell::Value(Value::Array(vec![]))
    );
    assert_eq!(shown_of(&rows[4], "observed"), "never-seen");
    assert_eq!(kept(tables, "accounts", &["seen_on=m22"]), 1);
    assert_eq!(kept(tables, "accounts", &["seen_on=f0f1f2f3"]), 2);
    let json = json_rows(&print(tables, "accounts", &[], true).expect("print").text);
    assert_eq!(json[0]["last_seen"], Value::from(NOW - 300_000));
    assert_eq!(json[0]["seen_on"][1], on("m22", 1, NOW - 300_000));
    assert_eq!(json[2]["last_seen"], Value::Null);

    // A sighting keeps a tab running for two hours: m3's row lapses first, at the instant the
    // fold names and not a millisecond before, and m3 stays listed with its sighting.
    let lapses = NOW - 600_000 + SEEN_LIVE_MS + 1;
    assert_eq!(folded.next_change_ms, Some(lapses));
    assert_eq!(fold(&inputs, lapses - 1).tables, *tables);
    let after = fold(&inputs, lapses);
    assert_eq!(
        cell_of(&after.tables.accounts[0], "seen_on"),
        Cell::Value(Value::Array(vec![
            on("m3", 0, NOW - 600_000),
            on("m22", 1, NOW - 300_000),
            on("f0f1f2f3", 0, days_ago),
        ]))
    );
    assert_eq!(after.next_change_ms, Some(NOW - 300_000 + SEEN_LIVE_MS + 1));

    // A sign-in word says more than a login file: with m22 signed out of it, m22's sighting
    // is no evidence of a login, and the other Macs' sightings still are.
    let mut worded = Seen::of_events(
        &[logged(UUID_2, seen_at(&pks[0], 300, Some("0000aaaa")))],
        now_s,
    );
    sign(&mut worded, &pks[0], UUID_2, SIGNIN_OUT, now_s - 100);
    let mut only = lab.clone();
    only.account_obs.clear();
    let out = self::tables(&Inputs {
        lab: live(only, NOW),
        journal: live(worded, NOW),
        ..Inputs::default()
    });
    assert_eq!(shown_of(&out.accounts[0], "observed"), "signed-out");
    assert_eq!(cell_of(&out.accounts[0], "last_seen"), time(NOW - 300_000));

    // Control: with no journal read the `seen` events were not read. A registry row still
    // dates the account it names; for an account no row names, where it was seen is unread.
    let unread = self::tables(&Inputs {
        lab: live(lab, NOW),
        ..Inputs::default()
    });
    assert_eq!(
        cell_of(&unread.accounts[0], "seen_on"),
        Cell::Value(Value::Array(vec![on("m3", 2, NOW - 600_000)]))
    );
    assert_eq!(cell_of(&unread.accounts[2], "last_seen"), Cell::Unread);
    assert_eq!(cell_of(&unread.accounts[2], "seen_on"), Cell::Unread);

    // The fixture's lab holds sightings by a tab, by the sweep and by a registry row.
    let (_, fixture) = fixture_tables();
    let seen_on = |row: &rows::AccountRow| row.seen_on.clone().unwrap_or_default();
    let listed: Vec<_> = fixture.accounts.iter().map(seen_on).collect();
    assert!(listed.iter().any(|macs| macs.len() > 1));
    assert!(listed.iter().flatten().any(|on| on.tabs == 2));
    assert!(listed.iter().flatten().any(|on| on.tabs == 0));
    assert!(
        fixture
            .accounts
            .iter()
            .any(|row| row.seen_on == Some(Vec::new()))
    );
}

/// A MAC THAT SAYS ITS LOGIN OF AN ACCOUNT ENDED IS NOT ONE THAT ONLY SIGHTED IT, WHATEVER
/// BECAME OF THE MAC: its word (a sign-in word that is not `in`, or its registry row saying
/// `signed-out`) stands over the login file it still holds, for a Mac a live machine row
/// binds, for one retired since, and for a machine whose word and sighting came from two of
/// its seats. `last_seen` and `seen_on` still say when and where the file was sighted. The
/// `in` of a retired Mac signs nothing in, so its sighting is what it says; another Mac's
/// sighting still stands; and a `seen` logged after a row said `signed-out` is a login that
/// is back.
/// NEGATIVE CONTROLS: with a sighting withheld only for a Mac a live machine row binds, and
/// only such a Mac's word read as signed out (as before), the first row reads `left: ["seen",
/// "f0f1f2f3"] right: ["signed-out", "f0f1f2f3"]`; with a word withholding only its own
/// uuid's sighting, `two seats of one machine` fails `left: ["seen", "m3"] right:
/// ["signed-out", "m3"]`; with the sightings' fold dating a sighting from a row that says
/// `signed-out`, `the row's word` fails `left: ["seen", "m3"] right: ["signed-out", "m3"]`.
#[test]
fn a_macs_word_that_its_login_ended_stands_over_its_own_sighting_bound_or_not() {
    use crate::alab::sighted::{Obs, ObsState};
    use aterm_json::json;
    let now_s = NOW / 1_000;
    let (mut lab, pks) = lab_with_accounts(&["a", "b", "c", "d", "e", "f", "g"]);
    // m3 was enrolled twice: two Commons uuids are seats of its one machine row.
    lab.bound.insert(UUID_3.to_owned(), keys::machine_pk("m3"));
    let swept = |uuid: &str, pk: &str, ago_s: u64| {
        logged(
            uuid,
            json!({"a": pk, "c": "claude", "k": "seen", "src": "profile", "t": now_s - ago_s}),
        )
    };
    let signed_out = |pk: &str, ago_ms: u64| Obs {
        account: pk.to_owned(),
        uuid: UUID.to_owned(),
        at_ms: NOW - ago_ms,
        state: ObsState::SignedOut,
        tabs: 0,
    };
    // f: m3's row says it signed out after it last sighted the login. g: m3 sighted the login
    // again after its row said so.
    lab.account_obs = vec![signed_out(&pks[5], 100_000), signed_out(&pks[6], 600_000)];
    let mut seen = Seen::of_events(
        &[
            swept(UUID_UNBOUND, &pks[0], 1_000),
            swept(UUID_UNBOUND, &pks[1], 1_000),
            swept(UUID_3, &pks[2], 50),
            swept(UUID_2, &pks[4], 700),
            swept(UUID, &pks[5], 1_000),
            swept(UUID, &pks[6], 60),
        ],
        now_s,
    );
    // a: a Mac sighted the login, signed out of it, and was retired. b: a retired Mac's last
    // word was `in`. c: m3's first seat signed out, and its second seat still finds the file.
    // d: a retired Mac signed out, and nothing sighted it. e: a retired Mac signed out, and
    // m22, which states nothing, sighted it.
    sign(&mut seen, &pks[0], UUID_UNBOUND, SIGNIN_OUT, now_s - 100);
    sign(&mut seen, &pks[1], UUID_UNBOUND, SIGNIN_IN, now_s - 100);
    sign(&mut seen, &pks[2], UUID, SIGNIN_OUT, now_s - 500);
    sign(&mut seen, &pks[3], UUID_UNBOUND, SIGNIN_ENDED, now_s - 100);
    sign(&mut seen, &pks[4], UUID_UNBOUND, SIGNIN_OUT, now_s - 100);
    let tables = self::tables(&Inputs {
        lab: live(lab, NOW),
        journal: live(seen, NOW),
        ..Inputs::default()
    });
    let rows = &tables.accounts;
    let observed =
        |row: &rows::AccountRow| [shown_of(row, "observed"), shown_of(row, "observed_by")];
    let at = |row: &rows::AccountRow| cell_of(row, "observed_at");
    let ms = |ago_s: u64| Cell::Value(Value::from((now_s - ago_s) * 1_000));
    assert_eq!(observed(&rows[0]), ["signed-out", "f0f1f2f3"]);
    assert_eq!(at(&rows[0]), ms(100));
    // Where its file was sighted is still a fact of the row.
    assert_eq!(cell_of(&rows[0], "last_seen"), ms(1_000));
    assert_eq!(shown_of(&rows[0], "last_seen_by"), "f0f1f2f3");
    assert_eq!(kept(&tables, "accounts", &["seen_on=f0f1f2f3"]), 2);
    // A retired Mac's `in` signs nothing in: the file it sighted is all it says.
    assert_eq!(observed(&rows[1]), ["seen", "f0f1f2f3"]);
    assert_eq!(at(&rows[1]), ms(1_000));
    assert_eq!(rows[1].active.as_deref(), Some(&[][..]));
    assert_eq!(
        observed(&rows[2]),
        ["signed-out", "m3"],
        "two seats of one machine"
    );
    assert_eq!(at(&rows[2]), ms(500));
    assert_eq!(cell_of(&rows[2], "last_seen"), ms(50));
    // A word with no sighting is still a Mac's word: signed out, not never seen.
    assert_eq!(observed(&rows[3]), ["signed-out", "f0f1f2f3"]);
    assert_eq!(cell_of(&rows[3], "last_seen"), Cell::Empty);
    // Another Mac's sighting is not withheld by this Mac's word.
    assert_eq!(observed(&rows[4]), ["seen", "m22"]);
    assert_eq!(at(&rows[4]), ms(700));
    // A registry row that says `signed-out` is no sighting and is its Mac's word.
    assert_eq!(observed(&rows[5]), ["signed-out", "m3"], "the row's word");
    assert_eq!(at(&rows[5]), ms(100));
    assert_eq!(cell_of(&rows[5], "last_seen"), ms(1_000));
    // A sighting after the row said so is a login that is back.
    assert_eq!(observed(&rows[6]), ["seen", "m3"]);
    assert_eq!(at(&rows[6]), ms(60));
}

/// `seen_on` LISTS AT MOST ITS BOUND OF MACS, THE ONES THAT SIGHTED THE ACCOUNT LAST: with
/// more Macs than the bound, the oldest sightings are the ones left out, whatever the Macs'
/// names, the list is of its column's type, and `last_seen` is the newest of them all.
/// NEGATIVE CONTROLS: with the list cut before it is ordered by time, it holds a Mac that
/// sighted the account among the oldest: `left: 9 right: 0`; with no cut, `left: 75 right:
/// 64`.
#[test]
fn seen_on_lists_the_macs_that_sighted_an_account_last_up_to_its_bound() {
    use crate::alab::sighted::{Obs, ObsState};
    const MACS: u64 = 75;
    let (mut lab, pks) = lab_with_accounts(&["a"]);
    // Each Mac's sighting is `age(i)` minutes old, in an order that is not the names': the
    // newest is Mac 0's, and the 11 oldest are spread over the names.
    let name = |i: u64| format!("a{i:07x}");
    let age = |i: u64| (i * 7) % MACS + 1;
    lab.account_obs = (0..MACS)
        .map(|i| Obs {
            account: pks[0].clone(),
            uuid: format!("{}{:024x}", name(i), 0),
            at_ms: NOW - age(i) * 60_000,
            state: ObsState::Idle,
            tabs: 0,
        })
        .collect();
    let tables = self::tables(&Inputs {
        lab: live(lab, NOW),
        journal: live(Seen::of_events(&[], NOW / 1_000), NOW),
        ..Inputs::default()
    });
    let row = &tables.accounts[0];
    let listed = row.seen_on.as_deref().expect("the sightings were read");
    let max = u64::try_from(rows::SEEN_ON_MAX).expect("a small bound");
    assert_eq!(listed.len(), rows::SEEN_ON_MAX);
    let old = listed
        .iter()
        .filter(|on| on.at_ms < NOW - max * 60_000)
        .count();
    assert_eq!(old, 0);
    // Every Mac that sighted it within the bound's newest is listed, by name.
    let newest: Vec<String> = (0..MACS).filter(|i| age(*i) <= max).map(name).collect();
    let names: Vec<&str> = listed.iter().map(|on| on.on.as_str()).collect();
    assert_eq!(names, newest);
    assert_eq!(
        cell_of(row, "last_seen"),
        Cell::Value(Value::from(NOW - 60_000))
    );
    assert_eq!(shown_of(row, "last_seen_by"), name(0));
    let schema = rows::AccountRow::SCHEMA;
    let column = schema.position("seen_on").expect("a column");
    let Cell::Value(cell) = cell_of(row, "seen_on") else {
        panic!("the list is read");
    };
    assert!(cell::admissible(&schema.columns[column].column.ty, &cell));
}

/// The accounts relation's facts: each a column a machine observed, with the column that says
/// when it was read and the one that says which machine read it.
const ACCOUNT_FACTS: &[(&str, &str, &str)] = &[
    ("w5h_reset", "w5h_at", "w5h_by"),
    ("w7d_reset", "w7d_at", "w7d_by"),
    ("wall_until", "wall_at", "wall_by"),
    ("last_use", "last_use", "last_use_on"),
    ("plan_observed", "plan_at", "plan_by"),
    ("observed", "observed_at", "observed_by"),
    ("last_seen", "last_seen", "last_seen_by"),
];

/// EVERY FACT OF AN ACCOUNT ROW SAYS WHEN IT WAS READ AND WHICH MAC READ IT, IN ONE SHAPE: a
/// `<fact>_at` time and a `<fact>_by` machine name beside it, read together or not at all, the
/// Mac named by its label, else its machine key, else the first 8 digits of its uuid, and the
/// time never after the journal's read. Where a fact is known to be nothing (no limit
/// stands), its provenance is empty; where nobody read it, unread.
/// NEGATIVE CONTROLS: with the fold crediting each window to the row's newest read (as
/// `read_by` does), `w5h_at` of the first row reads `left:
/// Value(Number(Number(Unsigned(1799999700000)))) right:
/// Value(Number(Number(Unsigned(1799999100000))))`; with a reset-passed window dated only by
/// a figure, `the reset's read` fails `left: Unread`.
#[test]
fn every_fact_of_an_account_says_when_it_was_read_and_by_which_mac() {
    let now_s = NOW / 1_000;
    let (lab, pks) = lab_with_accounts(&["a", "b", "c", "d"]);
    let quota = |pk: &str, window: &str, t: u64, reset: u64| {
        aterm_json::json!({
            "a": pk, "c": "claude", "k": "quota", "src": "statusline", "t": t, "w": window,
            "p": 400, "r": reset, "rr": "minute",
        })
    };
    let seen = Seen::of_events(
        &[
            // a: m3 read its 5-hour window, m22 its 7-day one later, and a Mac no machine row
            // binds hit a wall.
            logged(UUID, quota(&pks[0], "5h", now_s - 900, now_s + 3_600)),
            logged(UUID_2, quota(&pks[0], "7d", now_s - 300, now_s + 86_400)),
            logged(
                UUID_UNBOUND,
                aterm_json::json!({
                    "a": pks[0].as_str(), "c": "claude", "k": "wall", "src": "cli-output",
                    "t": now_s - 120, "w": "7d", "u": now_s + 7_200,
                }),
            ),
            // b: m22 read a 5-hour window whose reset has passed since.
            logged(UUID_2, quota(&pks[1], "5h", now_s - 7_200, now_s - 60)),
            // c: the journal knows it (a 7-day reading) and no limit stands.
            logged(UUID, quota(&pks[2], "7d", now_s - 600, now_s + 86_400)),
        ],
        now_s,
    );
    let tables = self::tables(&Inputs {
        lab: live(lab, NOW),
        journal: live(seen, NOW),
        ..Inputs::default()
    });
    let rows = &tables.accounts;
    let of = |row: &rows::AccountRow, columns: [&str; 2]| columns.map(|c| shown_of(row, c));
    let ms = |ago: u64| Cell::Value(Value::from((now_s - ago) * 1_000));
    assert_eq!(cell_of(&rows[0], "w5h_at"), ms(900));
    assert_eq!(shown_of(&rows[0], "w5h_by"), "m3");
    assert_eq!(cell_of(&rows[0], "w7d_at"), ms(300));
    assert_eq!(shown_of(&rows[0], "w7d_by"), "m22");
    assert_eq!(shown_of(&rows[0], "walled"), "true");
    assert_eq!(cell_of(&rows[0], "wall_at"), ms(120));
    assert_eq!(shown_of(&rows[0], "wall_by"), "f0f1f2f3");
    // The row's one `read_by` still names the Mac of the newest window read.
    assert_eq!(shown_of(&rows[0], "read_by"), "m22");
    // A window whose reset passed prints that reset: the read that named it dates it.
    assert_eq!(shown_of(&rows[1], "w5h_state"), "reset-passed");
    assert_eq!(shown_of(&rows[1], "w5h_used_x10"), "?");
    assert_eq!(cell_of(&rows[1], "w5h_at"), ms(7_200), "the reset's read");
    assert_eq!(shown_of(&rows[1], "w5h_by"), "m22");
    assert_eq!(of(&rows[1], ["w7d_at", "w7d_by"]), ["?", "?"]);
    // No limit stands, and that is known: nothing to date, which is empty and not unread.
    assert_eq!(shown_of(&rows[2], "walled"), "false");
    assert_eq!(cell_of(&rows[2], "wall_at"), Cell::Empty);
    assert_eq!(cell_of(&rows[2], "wall_by"), Cell::Empty);
    // The journal says nothing of d: every fact of it is unread, and so is who read it.
    for (_, at, by) in ACCOUNT_FACTS.iter().filter(|(fact, ..)| {
        // What was observed and where it was seen are known to be nothing once the journal
        // was read (`never-seen`, no sighting).
        !["observed", "last_seen"].contains(fact)
    }) {
        assert_eq!(cell_of(&rows[3], at), Cell::Unread, "{at}");
        assert_eq!(cell_of(&rows[3], by), Cell::Unread, "{by}");
    }
    let json = json_rows(&print(&tables, "accounts", &[], true).expect("print").text);
    assert_eq!(json[0]["w5h_by"], Value::from("m3"));
    assert_eq!(json[0]["wall_at"], Value::from((now_s - 120) * 1_000));
    assert_eq!(json[0]["wall_by"], Value::from("f0f1f2f3"));
    assert_eq!(json[2]["wall_by"], Value::Null);
    assert_eq!(kept(&tables, "accounts", &["w5h_by=m22"]), 1);

    // One shape, in the schema and in every row of the fixture's lab: a time and a machine
    // name, read together, and no fact dated after its read.
    let schema = rows::AccountRow::SCHEMA;
    for (fact, at, by) in ACCOUNT_FACTS {
        for column in [fact, at, by] {
            assert!(schema.position(column).is_some(), "{column}");
        }
        let ty = |column: &str| {
            let at = schema.position(column).expect("a column");
            &schema.columns[at].column.ty
        };
        assert!(matches!(ty(at), crate::typed::Ty::Time), "{at}");
        assert!(matches!(ty(by), crate::typed::Ty::Key), "{by}");
    }
    let (_, fixture) = fixture_tables();
    let mut read = [0_usize; ACCOUNT_FACTS.len()];
    for row in &fixture.accounts {
        for (n, (fact, at, by)) in ACCOUNT_FACTS.iter().enumerate() {
            let (at, by) = (cell_of(row, at), cell_of(row, by));
            match (&at, &by) {
                (Cell::Value(at), Cell::Value(by)) => {
                    read[n] += 1;
                    assert!(at.as_u64().is_some_and(|at| at <= NOW), "{fact}: {at:?}");
                    assert!(by.as_str().is_some_and(|by| !by.is_empty()), "{fact}");
                }
                (Cell::Unread, Cell::Unread) | (Cell::Empty, Cell::Empty) => {}
                _ => panic!("{fact} of {}: {at:?} by {by:?}", row.account),
            }
        }
    }
    assert!(read.iter().all(|read| *read > 0), "{read:?}");
}

/// NO ACCOUNT ROW HOLDS AN ADDRESS OUTSIDE ITS `login` COLUMN: who read a fact is a machine's
/// label, key or uuid digits, an observed plan a plan row's key, a sighting a machine and a
/// time. Checked over the fixture's lab in both printings.
/// NEGATIVE CONTROL: with `last_seen_by` printing the row's login, the scan fails naming
/// `last_seen_by`; and the `login` column itself does hold addresses, so the scan sees them.
#[test]
fn no_account_fact_or_its_provenance_holds_an_address() {
    let (_, tables) = fixture_tables();
    let schema = rows::AccountRow::SCHEMA;
    let login = schema.position("login").expect("the login column");
    let printed = print(&tables, "accounts", &[], false).expect("print").text;
    let grid = grid_of(&printed);
    assert_eq!(grid.len(), fixture::ACCOUNTS + 1);
    let mut addresses = 0;
    for line in &grid[1..] {
        assert_eq!(line.len(), schema.columns.len());
        for (at, cell) in line.iter().enumerate() {
            if at == login {
                addresses += usize::from(cell.contains('@'));
            } else {
                assert!(!cell.contains('@'), "{}", schema.columns[at].column.name);
            }
        }
    }
    assert!(addresses > 0, "the control: a login is an address");
    for row in json_rows(&print(&tables, "accounts", &[], true).expect("print").text) {
        let members = row.as_object().expect("a row is an object");
        assert_eq!(members.len(), schema.columns.len());
        for (name, value) in members {
            if name != "login" {
                assert!(!value.to_string().contains('@'), "{name}");
            }
        }
    }
}

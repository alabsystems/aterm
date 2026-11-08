// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which row a source's machine means, over hand-built registry histories: the replica's
//! `computers` table, the sheets and the roster, each alone and all in one run.

use super::super::{
    Input, Mode, Plan, Sheet, SheetKind, Step, covered, dump_sheets, plan,
    tests::{SEALER, Stand, logins, vault},
};
use super::*;
use crate::{
    alab::registry::{MACHINE, PREFERENCE},
    typed::{
        cell::Cells,
        grid::tests::{Book, elem, put},
    },
};
use aterm_json::{Value, json};

const NODE: &str = "00112233445566778899aabbccddeeff";
const OTHER_NODE: &str = "ffeeddccbbaa99887766554433221100";
/// A Mac's node principal, as the roster derives it and the Mac publishes it.
const PRINCIPAL: &str = "0011223344556677";
const OTHER_PRINCIPAL: &str = "ffeeddccbbaa9988";
/// A synthetic `[everyone]` key the dumps' sources are keyed under.
const KEY: [u8; 32] = [0x5a; 32];

fn input<'a>(sheets: &'a [Sheet], macs: &'a [KnownMac]) -> Input<'a> {
    Input {
        sheets,
        macs,
        vendors: &Stand,
        sealer: &SEALER,
        now_ms: 1_800_000_000_000,
        births: true,
        unread: &[],
    }
}

fn replica(computers: Value, mode: Mode) -> Vec<Sheet> {
    let dump = aterm_json::to_vec(&json!({"computers": computers})).unwrap();
    dump_sheets("fleet-db-m104.json", &dump, mode, &KEY).unwrap()
}

fn roster(label: u32, aliases: &[u32], node: Option<&str>) -> KnownMac {
    KnownMac {
        label: Some(Label::plain(label)),
        aliases: aliases.iter().copied().map(Label::plain).collect(),
        node: node.map(str::to_owned),
        fleet_node: None,
        hosts: Vec::new(),
    }
}

fn apply(book: &mut Book, plan: &Plan) -> Cells {
    for changes in &plan.rows {
        book.write(1, &[], changes.clone());
    }
    book.cells()
}

fn kinds(plan: &Plan) -> Vec<&'static str> {
    plan.findings.iter().map(|f| f.kind.word()).collect()
}

fn new_machines(plan: &Plan) -> usize {
    plan.counts
        .get(MACHINE.name)
        .map_or(0, |count| count.new_rows)
}

fn step<'p>(plan: &'p Plan, pk: &str, column: &str) -> Option<&'p Step> {
    plan.planned
        .iter()
        .find(|p| p.collection == MACHINE.name && p.pk == pk && p.column == column)
        .map(|p| &p.step)
}

fn wanted<'p>(plan: &'p Plan, pk: &str, column: &str) -> Option<&'p Value> {
    plan.planned
        .iter()
        .find(|p| p.collection == MACHINE.name && p.pk == pk && p.column == column)
        .map(|p| &p.x)
}

/// A node-bound row under the Mac's old label, carrying the new name as an alias.
fn relabelled(book: &mut Book) -> String {
    let pk = keys::machine_pk("m36");
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &pk, "label", json!("m36")),
            elem(&MACHINE, &pk, "aliases", "m104"),
            put(&MACHINE, &pk, "fleet_node", json!(NODE)),
            put(&MACHINE, &pk, "status", json!("active")),
            put(&MACHINE, &pk, "row", json!("live")),
        ],
    );
    pk
}

/// The row a name's second holder, as a roster or a sheet once made it.
fn second_holder(book: &mut Book, label: &str) -> String {
    let pk = keys::machine_pk(label);
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &pk, "label", json!(label)),
            put(&MACHINE, &pk, "status", json!("active")),
            put(&MACHINE, &pk, "row", json!("live")),
        ],
    );
    pk
}

/// A replica row under a name a row holds as an alias matches that row, stated with its node or
/// without, and births nothing; applied, the same replica plans nothing new.
/// NEGATIVE CONTROL: with `rows_named` reading only the label (its alias arm `false`), the replica
/// row births a second machine: `left: 1 right: 0`.
#[test]
fn a_replica_row_under_a_rows_alias_matches_it_and_settles() {
    for node in [None, Some(NODE)] {
        let mut book = Book::new();
        let row = relabelled(&mut book);
        let cells = book.cells();
        let sheets = replica(
            json!([{"label": "m104", "hostname": "m104.local", "node_id": node,
                "make": "Apple", "status": "active"}]),
            Mode::Authoritative,
        );
        let first = plan(&input(&sheets, &[]), &cells).unwrap();
        assert_eq!(new_machines(&first), 0, "{:?}", first.counts);
        assert_eq!(wanted(&first, &row, "make"), Some(&json!("Apple")));
        assert!(
            !kinds(&first).contains(&"label-ambiguous"),
            "{:?}",
            first.findings
        );
        let cells = apply(&mut book, &first);
        let again = plan(&input(&sheets, &[]), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
        assert_eq!(new_machines(&again), 0);
    }
}

/// A row is matched by its fleet node, then a host, then a name: the replica's label a row does
/// not hold joins that row's names, and a label another row holds is a finding, written nowhere.
/// Where each key answers a different row, the earlier key's row takes the machine.
/// NEGATIVE CONTROL: with `rows_on_host` comparing exact strings, the replica's `Studio` finds no
/// row and births one: `left: 1 right: 0`. With `alias_element` ignoring a clash, `m7` goes on a
/// second row: `left: Some(Bool(true)) right: None`. With `stated_row` asking the `node` cell, the
/// host's row takes the machine its node finds: `left: None right: Some(String("parked"))`.
#[test]
fn a_node_then_a_host_match_before_a_name() {
    let host_row = keys::machine_pk("m50");
    let mut book = Book::new();
    let row = relabelled(&mut book);
    let held = second_holder(&mut book, "m7");
    let by_name = second_holder(&mut book, "m60");
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &host_row, "label", json!("m50")),
            put(&MACHINE, &host_row, "hosts", json!(["studio.local"])),
            put(&MACHINE, &host_row, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let sheets = replica(
        json!([
            {"label": "m200", "hostname": "m200.local", "node_id": NODE, "status": "active"},
            {"label": "m201", "hostname": "Studio", "status": "parked"},
        ]),
        Mode::Authoritative,
    );
    let first = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&first), 0, "{:?}", first.counts);
    assert_eq!(wanted(&first, &row, "aliases.m200"), Some(&json!(true)));
    assert_eq!(
        wanted(&first, &host_row, "aliases.m201"),
        Some(&json!(true))
    );
    assert_eq!(wanted(&first, &host_row, "status"), Some(&json!("parked")));
    let sheets = replica(
        json!([{"label": "m7", "hostname": "m7.local", "node_id": NODE, "status": "active"}]),
        Mode::Authoritative,
    );
    let clash = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&clash), 0);
    assert_eq!(wanted(&clash, &row, "aliases.m7"), None);
    assert!(
        clash.planned.iter().all(|p| p.pk != held),
        "{:?}",
        clash.planned
    );
    assert!(clash.findings.iter().any(|f| f.kind == Kind::LabelAmbiguous
        && f.pk.as_deref() == Some(row.as_str())
        && f.column.as_deref() == Some("aliases")));
    for (stated, expected) in [
        (
            json!({"label": "m60", "hostname": "Studio", "node_id": NODE, "status": "parked"}),
            &row,
        ),
        (
            json!({"label": "m60", "hostname": "Studio", "status": "parked"}),
            &host_row,
        ),
    ] {
        let sheets = replica(json!([stated]), Mode::Authoritative);
        let order = plan(&input(&sheets, &[]), &cells).unwrap();
        for pk in [&row, &host_row, &by_name] {
            let wants = (pk == expected).then(|| json!("parked"));
            assert_eq!(wanted(&order, pk, "status"), wants.as_ref(), "{pk}");
        }
    }
}

/// What a source states beside a name finds its row, whichever is read first. A vault sheet read
/// before the replica's `computers` table names `m104`, and the row that table's node or host finds
/// takes it and the sheet's preference: nothing is born beside that row, which would share no name
/// with it, so nothing could ever fold the two.
/// NEGATIVE CONTROL: with `NameLinks::table` recording no node, or no host, the sheet births
/// `m104`: `left: 1 right: 0`.
#[test]
fn a_name_read_before_its_computers_row_finds_the_row_its_node_or_host_finds() {
    for computer in [
        json!({"label": "m104", "hostname": "m104.local", "node_id": NODE, "status": "active"}),
        json!({"label": "m104", "hostname": "Desk", "status": "active"}),
    ] {
        let row = keys::machine_pk("m36");
        let mut book = Book::new();
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, &row, "label", json!("m36")),
                put(&MACHINE, &row, "fleet_node", json!(NODE)),
                put(&MACHINE, &row, "hosts", json!(["desk.local"])),
                put(&MACHINE, &row, "status", json!("active")),
                put(&MACHINE, &row, "row", json!("live")),
            ],
        );
        let mut sheets = replica(json!([computer]), Mode::FillOnly);
        sheets.push(vault(&[",ann@example.invalid,m104,,1,,,,,,,,"]));
        let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
        assert_eq!(new_machines(&first), 0, "{:?}", first.counts);
        assert_eq!(wanted(&first, &row, "aliases.m104"), Some(&json!(true)));
        let preference = first
            .planned
            .iter()
            .find(|p| p.collection == PREFERENCE.name && p.column == "machine")
            .unwrap();
        assert_eq!(preference.x, json!(row));
        let cells = apply(&mut book, &first);
        let again = plan(&input(&sheets, &[]), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
    }
}

/// A row holding another node than the one stated is another Mac's. A shared host finds no row for
/// a machine stating a different node, and the host goes on the first row only, a finding on the
/// other: a Mac with that host could otherwise adopt neither row. The roster's word for a host
/// crosses no node either.
/// NEGATIVE CONTROL: with `other_mac` ignoring the transport id, `m2` folds into `m1`: `left: 1
/// right: 2`. With `known_found` not reading it, the roster's Mac takes the row: `left: 0 right: 1`.
#[test]
fn a_host_never_crosses_a_node() {
    let sheets = replica(
        json!([
            {"label": "m1", "hostname": "Studio", "node_id": NODE, "status": "active"},
            {"label": "m2", "hostname": "Studio", "node_id": OTHER_NODE, "status": "active"},
        ]),
        Mode::Authoritative,
    );
    let mut book = Book::new();
    let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert_eq!(new_machines(&first), 2, "{:?}", first.counts);
    let (one, two) = (keys::machine_pk("m1"), keys::machine_pk("m2"));
    assert_eq!(wanted(&first, &one, "hosts"), Some(&json!(["Studio"])));
    assert_eq!(wanted(&first, &two, "hosts"), None);
    assert_eq!(wanted(&first, &two, "fleet_node"), Some(&json!(OTHER_NODE)));
    assert!(first.findings.iter().any(|f| f.kind == Kind::HostAmbiguous
        && f.pk.as_deref() == Some(two.as_str())
        && f.column.as_deref() == Some("hosts")));
    let cells = apply(&mut book, &first);
    let again = plan(&input(&sheets, &[]), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
    let uncovered = covered::judge(&again, &|_| false);
    assert!(
        uncovered.iter().any(|u| u.kind == "host-ambiguous"),
        "{uncovered:?}"
    );

    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &one, "label", json!("m1")),
            put(&MACHINE, &one, "node", json!(OTHER_PRINCIPAL)),
            put(&MACHINE, &one, "hosts", json!(["Studio"])),
            put(&MACHINE, &one, "row", json!("live")),
        ],
    );
    let macs = [KnownMac {
        hosts: vec!["Studio".to_owned()],
        ..roster(9, &[], Some(PRINCIPAL))
    }];
    let known = plan(&input(&[], &macs), &book.cells()).unwrap();
    assert_eq!(new_machines(&known), 1, "{:?}", known.counts);
    assert_eq!(wanted(&known, &one, "label"), None);
}

/// fleet_db wrote `<label>.local` for every row, so that spelling states no host: it finds no row
/// by host, even one holding it.
/// NEGATIVE CONTROL: with `host_word` taking the default as stated, `m201` lands on the row that
/// holds it: `left: 0 right: 1`.
#[test]
fn a_default_hostname_states_no_host() {
    let row = keys::machine_pk("m50");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &row, "label", json!("m50")),
            put(&MACHINE, &row, "hosts", json!(["m201.local"])),
            put(&MACHINE, &row, "row", json!("live")),
        ],
    );
    let sheets = replica(
        json!([{"label": "m201", "hostname": "m201.local", "status": "active"}]),
        Mode::Authoritative,
    );
    let plan = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert_eq!(new_machines(&plan), 1, "{:?}", plan.counts);
    assert_eq!(wanted(&plan, &row, "aliases.m201"), None);
}

/// A name answered by a row another Mac's node holds is a finding, from a replica row stating
/// another transport id or the roster stating another principal: nothing is written on the row and
/// nothing is born under its name.
/// NEGATIVE CONTROL: with `Planner::found` not reading `other_mac`, the plan wants the other node on
/// the row, and the `all(|p| p.pk != row)` assertion prints it.
#[test]
fn a_name_on_another_macs_row_is_a_finding() {
    let row = keys::machine_pk("m5");
    let cases: [(&str, &str, Vec<Sheet>, Vec<KnownMac>); 2] = [
        (
            "fleet_node",
            NODE,
            replica(
                json!([{"label": "m5", "hostname": "m5.local", "node_id": OTHER_NODE,
                    "status": "active"}]),
                Mode::Authoritative,
            ),
            Vec::new(),
        ),
        (
            "node",
            PRINCIPAL,
            Vec::new(),
            vec![roster(5, &[], Some(OTHER_PRINCIPAL))],
        ),
    ];
    for (column, node, sheets, macs) in &cases {
        let mut book = Book::new();
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, &row, "label", json!("m5")),
                put(&MACHINE, &row, column, json!(node)),
                put(&MACHINE, &row, "status", json!("active")),
                put(&MACHINE, &row, "row", json!("live")),
            ],
        );
        let plan = plan(&input(sheets, macs), &book.cells()).unwrap();
        assert_eq!(new_machines(&plan), 0, "{column} {:?}", plan.counts);
        assert!(
            plan.planned.iter().all(|p| p.pk != row),
            "{:?}",
            plan.planned
        );
        assert!(kinds(&plan).contains(&"label-ambiguous"), "{column}");
    }
}

/// The roster lists a member before its Mac joins this registry, so the row it births is `planned`,
/// the one state `register::pick` adopts by name; an `active` row with no node or host would make
/// the Mac birth a second row. Only the Mac's own published principal makes it `active`.
/// NEGATIVE CONTROL: with `known_row` wanting `active` on a birth: `left: Some(String("active"))
/// right: Some(String("planned"))`.
#[test]
fn a_roster_birth_stays_planned_until_its_mac_publishes_its_node() {
    let macs = [roster(5, &[], Some(PRINCIPAL))];
    let row = keys::machine_pk(&format!("n-{PRINCIPAL}"));
    let mut book = Book::new();
    let born = plan(&input(&[], &macs), &book.cells()).unwrap();
    assert_eq!(wanted(&born, &row, "status"), Some(&json!("planned")));
    assert_eq!(wanted(&born, &row, "node"), None);
    let cells = apply(&mut book, &born);
    let again = plan(&input(&[], &macs), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&MACHINE, &row, "node", json!(PRINCIPAL))],
    );
    let joined = plan(&input(&[], &macs), &book.cells()).unwrap();
    assert_eq!(wanted(&joined, &row, "status"), Some(&json!("active")));
    let cells = apply(&mut book, &joined);
    let settled = plan(&input(&[], &macs), &cells).unwrap();
    assert!(settled.rows.is_empty(), "{:?}", settled.planned);
}

/// A row the roster born under a Mac's own key is its row before the Mac publishes its node: a
/// relabel moves it rather than birthing another, and a Mac with no label reaches a fixed point.
/// NEGATIVE CONTROL: with `row_under_key` answering `None`, the relabel births: `left: 1 right: 0`.
#[test]
fn a_roster_row_is_found_under_its_own_key() {
    let row = keys::machine_pk(&format!("n-{PRINCIPAL}"));
    let mut book = Book::new();
    let born = plan(
        &input(&[], &[roster(12, &[], Some(PRINCIPAL))]),
        &book.cells(),
    )
    .unwrap();
    let cells = apply(&mut book, &born);
    let macs = [roster(120, &[], Some(PRINCIPAL))];
    let relabelled = plan(&input(&[], &macs), &cells).unwrap();
    assert_eq!(new_machines(&relabelled), 0, "{:?}", relabelled.counts);
    assert_eq!(wanted(&relabelled, &row, "label"), Some(&json!("m120")));

    let unlabelled = [KnownMac {
        label: None,
        aliases: Vec::new(),
        node: Some(PRINCIPAL.to_owned()),
        fleet_node: None,
        hosts: Vec::new(),
    }];
    let mut book = Book::new();
    let first = plan(&input(&[], &unlabelled), &book.cells()).unwrap();
    assert_eq!(new_machines(&first), 1, "{:?}", first.counts);
    let cells = apply(&mut book, &first);
    let again = plan(&input(&[], &unlabelled), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
    assert_eq!(new_machines(&again), 0);
}

/// Links close across sources, so a name two sources state as an alias of different labels may mean
/// two machines: it links nothing and is written on neither. A link joining two of the roster's
/// members is a finding too, not a fold, even with each member's row found by its own node. A roster
/// name the sources contest births nothing. A computers sheet's link joins one Mac's names as a
/// replica's does.
/// NEGATIVE CONTROL: with `NameLinks::read` recording no contest across tables, `m3` folds into
/// `m1`: `left: 1 right: 2`. With `known_found` skipping its contested label, the roster's `m2` is
/// born: `left: 3 right: 2`. With `named` taking the first of two members, the sheet's `make` lands
/// on that member's row: `left: Some(String("Apple")) right: None`.
#[test]
fn a_name_two_sources_claim_links_nothing() {
    let header: &[u8] =
        b"ID,Make,Arch,OS,Processor,RAM,HD,Aliases,Nickname,Nickname Aliases,Location,Notes\n";
    let sheet = |row: &str| {
        let bytes = [header, row.as_bytes()].concat();
        Sheet::parse(SheetKind::Computers, "alab-computers.csv", &bytes).unwrap()
    };
    let mut sheets = replica(
        json!([{"label": "m3", "hostname": "m3.local", "aliases": ["m2"], "status": "active"}]),
        Mode::Authoritative,
    );
    sheets.push(sheet("m1,Apple,,,,,,m2,,,,\n"));
    let first = plan(&input(&sheets, &[]), &Cells::new()).unwrap();
    assert_eq!(new_machines(&first), 2, "{:?}", first.counts);
    for pk in [keys::machine_pk("m1"), keys::machine_pk("m3")] {
        assert_eq!(wanted(&first, &pk, "aliases.m2"), None);
    }
    assert!(kinds(&first).contains(&"label-ambiguous"));
    let named = [roster(2, &[], None)];
    let contested = plan(&input(&sheets, &named), &Cells::new()).unwrap();
    assert_eq!(new_machines(&contested), 2, "{:?}", contested.counts);

    let stale = [sheet("m36,Apple,,,,,,m104,,,,\n")];
    let macs = [roster(36, &[], None), roster(104, &[], None)];
    let joined = plan(&input(&stale, &macs), &Cells::new()).unwrap();
    assert_eq!(new_machines(&joined), 0, "{:?}", joined.counts);
    assert!(kinds(&joined).contains(&"label-ambiguous"));

    let mut book = Book::new();
    for (label, node) in [("m36", PRINCIPAL), ("m104", OTHER_PRINCIPAL)] {
        let pk = keys::machine_pk(label);
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, &pk, "label", json!(label)),
                put(&MACHINE, &pk, "node", json!(node)),
                put(&MACHINE, &pk, "row", json!("live")),
            ],
        );
    }
    let macs = [
        roster(36, &[], Some(PRINCIPAL)),
        roster(104, &[], Some(OTHER_PRINCIPAL)),
    ];
    let bound = plan(&input(&stale, &macs), &book.cells()).unwrap();
    for label in ["m36", "m104"] {
        let pk = keys::machine_pk(label);
        assert_eq!(wanted(&bound, &pk, "make"), None, "{label}");
    }
    assert!(kinds(&bound).contains(&"label-ambiguous"));

    let linked = [
        sheet("m36,Apple,,,,,,m104,,,,\n"),
        vault(&[",ann@example.invalid,m104,,1,,,,,,,,"]),
    ];
    let one = plan(&input(&linked, &[]), &Cells::new()).unwrap();
    assert_eq!(new_machines(&one), 1, "{:?}", one.counts);
}

/// Each of the roster's words finds a row: a known Mac's name that cannot land stays the row's
/// alias, a label another row holds is a finding on the column, and a row's owner keeps its label.
/// NEGATIVE CONTROL: with `land_label` not wanting the name as an alias (its `label_lands` branch
/// dropped), the name the roster now gives the Mac is wanted nowhere: `left: None
/// right: Some(Bool(true))`.
#[test]
fn a_roster_name_the_row_cannot_take_stays_an_alias() {
    let row = keys::machine_pk("m36");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &row, "label", json!("m36")),
            put(&MACHINE, &row, "node", json!(PRINCIPAL)),
            put(&MACHINE, &row, "status", json!("active")),
            put(&MACHINE, &row, "row", json!("live")),
        ],
    );
    let earlier = [roster(36, &[104], Some(PRINCIPAL))];
    let first = plan(&input(&[], &earlier), &book.cells()).unwrap();
    assert_eq!(wanted(&first, &row, "aliases.m104"), Some(&json!(true)));
    let cells = apply(&mut book, &first);
    let macs = [roster(104, &[36], Some(PRINCIPAL))];
    let renamed = plan(&input(&[], &macs), &cells).unwrap();
    assert_eq!(wanted(&renamed, &row, "aliases.m104"), Some(&json!(true)));
    assert_eq!(wanted(&renamed, &row, "aliases.m36"), None);
    assert!(
        kinds(&renamed).contains(&"owner-kept"),
        "{:?}",
        renamed.findings
    );
    let cells = apply(&mut book, &renamed);
    let again = plan(&input(&[], &macs), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
}

/// The roster's label on a row found by node, while another row holds that name, is a finding on
/// `label`: the label stays put and is not given to a second holder.
/// NEGATIVE CONTROL: with `known_label` landing the label without asking `label_clash`:
/// `left: Some(String("m120")) right: None`.
#[test]
fn a_roster_label_another_row_holds_is_a_finding() {
    let row = keys::machine_pk("m12");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &row, "label", json!("m12")),
            put(&MACHINE, &row, "node", json!(PRINCIPAL)),
            put(&MACHINE, &row, "row", json!("live")),
        ],
    );
    second_holder(&mut book, "m120");
    let macs = [roster(120, &[], Some(PRINCIPAL))];
    let plan = plan(&input(&[], &macs), &book.cells()).unwrap();
    assert_eq!(wanted(&plan, &row, "label"), None);
    assert!(plan.findings.iter().any(|f| f.kind == Kind::LabelAmbiguous
        && f.pk.as_deref() == Some(row.as_str())
        && f.column.as_deref() == Some("label")));
}

/// Each candidate of a conflicted label answers the row, as `register::birth_label` reserves it.
/// NEGATIVE CONTROL: with `rows_named` comparing the shown value alone, whichever candidate the
/// digest order hides births a row: `left: 1 right: 0`.
#[test]
fn every_candidate_of_a_conflicted_label_answers() {
    let row = keys::machine_pk("m5");
    let mut book = Book::new();
    book.write(1, &[], vec![put(&MACHINE, &row, "row", json!("live"))]);
    book.write(1, &[], vec![put(&MACHINE, &row, "label", json!("m5"))]);
    book.write(3, &[], vec![put(&MACHINE, &row, "label", json!("m6"))]);
    let cells = book.cells();
    for label in ["m5", "m6"] {
        let sheets = replica(
            json!([{"label": label, "hostname": format!("{label}.local"), "status": "active"}]),
            Mode::Authoritative,
        );
        let plan = plan(&input(&sheets, &[]), &cells).unwrap();
        assert_eq!(new_machines(&plan), 0, "{label} {:?}", plan.counts);
    }
}

/// A host the row already answers says nothing new: a register's fuller list is not rewritten or
/// reported as a difference, so the replica's cell judges covered.
/// NEGATIVE CONTROL: with `hosts_cell` always answering `[host]`, the register's cell is the
/// owner's, so `kinds` holds `owner-kept`, which the panic prints.
#[test]
fn a_host_the_row_answers_is_covered() {
    let row = keys::machine_pk("m36");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &row, "label", json!("m36")),
            put(&MACHINE, &row, "hosts", json!(["Studio-MBP", "Studio MBP"])),
            put(&MACHINE, &row, "status", json!("active")),
            put(&MACHINE, &row, "row", json!("live")),
        ],
    );
    let sheets = replica(
        json!([{"label": "m36", "hostname": "Studio-MBP.local", "status": "active"}]),
        Mode::Authoritative,
    );
    let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert!(
        !kinds(&first).contains(&"owner-kept"),
        "{:?}",
        first.findings
    );
    let cells = apply(&mut book, &first);
    let again = plan(&input(&sheets, &[]), &cells).unwrap();
    let uncovered = covered::judge(&again, &|_| false);
    assert!(uncovered.is_empty(), "{uncovered:?}");
}

/// One run reads the rows it is making as it reads the registry's: two replica rows stating one
/// node, or one host, make one row, a sheet naming an alias a replica row wants this run finds that
/// row, and a name this run gave one row is not given to another.
/// NEGATIVE CONTROL: with the plan arm of `rows_on_node` dropped, `m50` and `m51` are two rows:
/// `left: 2 right: 1`. With the plan alias arm of `rows_named` dropped, `m9` goes on both rows:
/// `left: 2 right: 1`.
#[test]
fn a_run_reads_the_rows_it_is_making() {
    for computers in [
        json!([
            {"label": "m50", "hostname": "m50.local", "node_id": NODE, "status": "active"},
            {"label": "m51", "hostname": "m51.local", "node_id": NODE, "status": "active"},
        ]),
        json!([
            {"label": "m50", "hostname": "Studio", "status": "active"},
            {"label": "m51", "hostname": "Studio", "status": "active"},
        ]),
    ] {
        let sheets = replica(computers, Mode::Authoritative);
        let mut book = Book::new();
        let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
        assert_eq!(new_machines(&first), 1, "{:?}", first.counts);
        let cells = apply(&mut book, &first);
        let again = plan(&input(&sheets, &[]), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
    }
    let mut book = Book::new();
    relabelled(&mut book);
    let mut sheets = replica(
        json!([{"label": "m200", "hostname": "m200.local", "node_id": NODE, "status": "active"}]),
        Mode::Authoritative,
    );
    sheets.push(vault(&[",ann@example.invalid,m200,,1,,,,,,,,"]));
    let joined = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert_eq!(new_machines(&joined), 0, "{:?}", joined.counts);

    let mut book = Book::new();
    let rows = [("m50", NODE), ("m51", OTHER_NODE)].map(|(label, node)| {
        let pk = keys::machine_pk(label);
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, &pk, "label", json!(label)),
                put(&MACHINE, &pk, "fleet_node", json!(node)),
                put(&MACHINE, &pk, "row", json!("live")),
            ],
        );
        pk
    });
    let sheets = replica(
        json!([
            {"label": "m9", "hostname": "m9.local", "node_id": NODE, "status": "active"},
            {"label": "m9", "hostname": "m9.local", "node_id": OTHER_NODE, "status": "active"},
        ]),
        Mode::Authoritative,
    );
    let shared = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    let holders = rows
        .iter()
        .filter(|pk| wanted(&shared, pk, "aliases.m9").is_some())
        .count();
    assert_eq!(holders, 1, "{:?}", shared.planned);
    assert!(kinds(&shared).contains(&"label-ambiguous"));
}

/// What a replica holds concurrent values for finds nothing and links nothing: a row with a
/// concurrent delete imports nothing, its finding naming the row its node finds; a disputed node,
/// host or alias does not match the row it would name, so the machine keeps its own name.
/// NEGATIVE CONTROL: with `stated_node` ignoring `conflicted`, `m200` lands on the row that node
/// holds: `left: 0 right: 1`. With `sheet_rows` reading a row with a concurrent delete, a sheet's
/// `m104` finds the row by that row's node: `left: Some(Bool(true)) right: None`.
#[test]
fn what_a_replica_disputes_finds_no_row() {
    let row = keys::machine_pk("m36");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &row, "label", json!("m36")),
            put(&MACHINE, &row, "fleet_node", json!(NODE)),
            put(&MACHINE, &row, "hosts", json!(["desk.local"])),
            put(&MACHINE, &row, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let sheets = replica(
        json!([{"label": "m104", "node_id": NODE, "_conflicts": [{"_deleted": true}]}]),
        Mode::Authoritative,
    );
    let deleted = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&deleted), 0, "{:?}", deleted.counts);
    assert!(deleted.planned.iter().all(|p| p.collection != MACHINE.name));
    assert!(
        deleted
            .findings
            .iter()
            .any(|f| f.kind == Kind::DumpConflicted && f.pk.as_deref() == Some(row.as_str())),
        "{:?}",
        deleted.findings
    );
    let mut named = sheets.clone();
    named.push(vault(&[",ann@example.invalid,m104,,1,,,,,,,,"]));
    let named = plan(&input(&named, &[]), &cells).unwrap();
    assert_eq!(wanted(&named, &row, "aliases.m104"), None);
    for computer in [
        json!({"label": "m200", "hostname": "m200.local", "node_id": NODE,
            "_conflicts": [{"label": "m200", "hostname": "m200.local", "node_id": OTHER_NODE}]}),
        json!({"label": "m200", "hostname": "Desk",
            "_conflicts": [{"label": "m200", "hostname": "Other"}]}),
        json!({"label": "m200", "hostname": "m200.local", "aliases": ["m36"],
            "_conflicts": [{"label": "m200", "hostname": "m200.local", "aliases": ["m37"]}]}),
    ] {
        let sheets = replica(json!([computer]), Mode::Authoritative);
        let disputed = plan(&input(&sheets, &[]), &cells).unwrap();
        assert_eq!(new_machines(&disputed), 1, "{:?}", disputed.counts);
        assert_eq!(wanted(&disputed, &row, "aliases.m200"), None);
    }
}

/// A name two rows answer births nothing and writes on neither, from the replica, a sheet or the
/// roster, and what it leaves unwritten keeps the replica uncovered.
/// NEGATIVE CONTROL: with `sole` answering the smallest key, the sheet's preference lands on one of
/// the rows: `left: Some(1) right: None`.
#[test]
fn a_name_two_rows_answer_is_a_finding_not_a_birth() {
    let mut book = Book::new();
    let row = relabelled(&mut book);
    let duplicate = second_holder(&mut book, "m104");
    let cells = book.cells();
    let macs = [roster(104, &[], None)];
    let sheets = [vault(&[",ann@example.invalid,m104,,1,,,,,,,,"])];
    let by_sheet = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&by_sheet), 0);
    assert!(kinds(&by_sheet).contains(&"label-ambiguous"));
    assert_eq!(by_sheet.counts.get(PREFERENCE.name).map(|c| c.rows), None);
    let by_roster = plan(&input(&[], &macs), &cells).unwrap();
    assert_eq!(new_machines(&by_roster), 0, "{:?}", by_roster.counts);
    assert!(kinds(&by_roster).contains(&"label-ambiguous"));
    assert!(
        by_roster
            .planned
            .iter()
            .all(|p| p.pk != row && p.pk != duplicate),
        "{:?}",
        by_roster.planned
    );
    let sheets = replica(
        json!([{"label": "m104", "hostname": "m104.local", "status": "active"}]),
        Mode::Authoritative,
    );
    let by_replica = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&by_replica), 0);
    assert!(kinds(&by_replica).contains(&"label-ambiguous"));
    assert!(
        by_replica
            .planned
            .iter()
            .all(|p| p.pk != row && p.pk != duplicate),
        "{:?}",
        by_replica.planned
    );
    let uncovered = covered::judge(&by_replica, &|_| false);
    assert!(
        uncovered.iter().any(|u| u.kind == "label-ambiguous"),
        "{uncovered:?}"
    );
}

/// Two rows holding one transport id are a finding for a replica row stating it.
/// NEGATIVE CONTROL: with `sole` answering the smallest key, the replica's cells land on it and no
/// finding names it: the `node-ambiguous` assertion prints no findings.
#[test]
fn two_rows_on_one_node_are_a_finding() {
    let mut book = Book::new();
    let mut rows = Vec::new();
    for label in ["m50", "m51"] {
        let pk = keys::machine_pk(label);
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, &pk, "label", json!(label)),
                put(&MACHINE, &pk, "fleet_node", json!(NODE)),
                put(&MACHINE, &pk, "row", json!("live")),
            ],
        );
        rows.push(pk);
    }
    let sheets = replica(
        json!([{"label": "m201", "hostname": "m201.local", "node_id": NODE, "make": "Apple",
            "status": "active"}]),
        Mode::Authoritative,
    );
    let plan = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert_eq!(new_machines(&plan), 0, "{:?}", plan.counts);
    assert!(
        kinds(&plan).contains(&"node-ambiguous"),
        "{:?}",
        plan.findings
    );
    assert!(
        plan.planned.iter().all(|p| !rows.contains(&p.pk)),
        "{:?}",
        plan.planned
    );
}

/// Two live rows holding one host are a finding, spelled with or without `.local`.
/// NEGATIVE CONTROL: with `rows_on_host` comparing exact strings, the replica's `Studio.local`
/// matches no row and births one: `left: 1 right: 0`.
#[test]
fn two_rows_on_one_host_are_a_finding() {
    let mut book = Book::new();
    for label in ["m50", "m51"] {
        let pk = keys::machine_pk(label);
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, &pk, "label", json!(label)),
                put(&MACHINE, &pk, "hosts", json!(["studio"])),
                put(&MACHINE, &pk, "row", json!("live")),
            ],
        );
    }
    let cells = book.cells();
    let sheets = replica(
        json!([{"label": "m201", "hostname": "Studio.local", "status": "active"}]),
        Mode::Authoritative,
    );
    let plan = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&plan), 0, "{:?}", plan.counts);
    assert!(
        kinds(&plan).contains(&"host-ambiguous"),
        "{:?}",
        plan.findings
    );
}

/// One run over an empty registry makes one row per Mac whichever source names it first: a
/// replica row stating `m36` with the alias `m104`, a sheet naming each, and the roster naming the
/// new one. The roster's label wins a birth; with no roster the first source's does. Applied, the
/// same sources plan nothing.
/// NEGATIVE CONTROL: with `NameLinks::read` answering no links, the replica's label beats the
/// roster's: `left: String("m36") right: String("m104")`. With the plan label arm of `rows_named`
/// dropped, the fill-only run reads the sheet first and births both of its names: `left: 2 right: 1`.
#[test]
fn one_run_makes_one_row_whichever_source_names_it_first() {
    let computers = json!([{"label": "m36", "hostname": "m36.local", "node_id": NODE,
        "aliases": ["m104"], "status": "active"}]);
    let macs = [roster(104, &[], None)];
    for (mode, known, label, alias) in [
        (Mode::Authoritative, &macs[..], "m104", "m36"),
        (Mode::FillOnly, &macs[..], "m104", "m36"),
        (Mode::Authoritative, &[][..], "m36", "m104"),
        (Mode::FillOnly, &[][..], "m104", "m36"),
    ] {
        let mut sheets = replica(computers.clone(), mode);
        sheets.push(vault(&[
            ",ann@example.invalid,m104,,1,,,,,,,,",
            ",bob@example.invalid,m36,,1,,,,,,,,",
        ]));
        let mut book = Book::new();
        let first = plan(&input(&sheets, known), &book.cells()).unwrap();
        assert_eq!(new_machines(&first), 1, "{mode:?} {:?}", first.counts);
        let machine = first
            .planned
            .iter()
            .find(|p| p.collection == MACHINE.name && p.column == "label")
            .unwrap();
        assert_eq!(machine.x, json!(label), "{mode:?}");
        let pk = machine.pk.clone();
        assert_eq!(
            wanted(&first, &pk, &format!("aliases.{alias}")),
            Some(&json!(true))
        );
        assert_eq!(wanted(&first, &pk, "fleet_node"), Some(&json!(NODE)));
        assert!(
            !kinds(&first).contains(&"label-ambiguous"),
            "{:?}",
            first.findings
        );
        let cells = apply(&mut book, &first);
        let again = plan(&input(&sheets, known), &cells).unwrap();
        assert!(again.rows.is_empty(), "{mode:?} {:?}", again.planned);
    }
}

/// Two rows of one source claiming one name mean two machines by that source's own word: the name
/// links nothing, resolves to nothing and is written on neither, and each row keeps its label. What
/// names only that name reaches no row, which is its own finding.
/// NEGATIVE CONTROL: with `name_clash` not reading a contest, the name goes on the rows: `left:
/// Some(Bool(true)) right: None`. With `NameLinks::of` walking through it, `m1` and `m3` become one
/// row: `left: 1 right: 2`. With `machine_for` returning at a contest, the sheet's name is dropped
/// with no finding, and the `f.pk.is_none()` assertion fails.
#[test]
fn two_rows_of_one_source_claiming_a_name_contest_it() {
    let sheets = replica(
        json!([
            {"label": "m1", "hostname": "m1.local", "aliases": ["m2"], "status": "active"},
            {"label": "m3", "hostname": "m3.local", "aliases": ["m2"], "status": "active"},
        ]),
        Mode::Authoritative,
    );
    let mut all = sheets;
    all.push(vault(&[",ann@example.invalid,m2,,1,,,,,,,,"]));
    let mut book = Book::new();
    let first = plan(&input(&all, &[]), &book.cells()).unwrap();
    assert_eq!(new_machines(&first), 2, "{:?}", first.counts);
    for label in ["m1", "m3"] {
        let pk = keys::machine_pk(label);
        assert_eq!(wanted(&first, &pk, "label"), Some(&json!(label)));
        assert_eq!(wanted(&first, &pk, "aliases.m2"), None);
    }
    assert!(
        first
            .findings
            .iter()
            .any(|f| f.kind == Kind::LabelAmbiguous && f.pk.is_none()),
        "{:?}",
        first.findings
    );
    let cells = apply(&mut book, &first);
    let again = plan(&input(&all, &[]), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
}

/// A name stated as one row's label and another's alias is contested by that source, and a row's own
/// node still finds it: the row takes the replica's cells, never the name.
/// NEGATIVE CONTROL: with `machine_at` going straight to `machine_for`, the contested label reaches
/// no row: `left: None right: Some(String("Apple"))`. With `NameLinks::table` recording no contest,
/// the name goes on the row: `left: Some(Bool(true)) right: None`.
#[test]
fn a_stated_node_finds_its_row_under_a_contested_name() {
    let mut book = Book::new();
    let row = relabelled(&mut book);
    let sheets = replica(
        json!([
            {"label": "m2", "hostname": "m2.local", "node_id": NODE, "make": "Apple",
                "status": "active"},
            {"label": "m3", "hostname": "m3.local", "aliases": ["m2"], "status": "active"},
        ]),
        Mode::Authoritative,
    );
    let plan = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert_eq!(wanted(&plan, &row, "make"), Some(&json!("Apple")));
    assert_eq!(wanted(&plan, &row, "aliases.m2"), None);
    assert_eq!(wanted(&plan, &keys::machine_pk("m3"), "aliases.m2"), None);
    assert!(kinds(&plan).contains(&"label-ambiguous"));
}

/// A name a sheet still says survives the roster replacing its own label with a new one: it stays
/// the row's as an alias, wanted by the sheet from then on, so no later run births a row for it or
/// retracts it. A name nothing says any more is simply gone.
/// NEGATIVE CONTROL: with `land_label` keeping no alias (`replaced` always `None`), the row loses
/// the sheet's old name: `left: None right: Some(Bool(true))`. With `alias_element` not recording
/// the name, a replica's alias column does the same.
#[test]
fn a_name_a_sheet_still_says_survives_the_rosters_rename() {
    for sheets in [
        vec![vault(&[",ann@example.invalid,m12,,1,,,,,,,,"])],
        replica(
            json!([{"label": "m36", "hostname": "m36.local", "aliases": ["m12"],
                "status": "active"}]),
            Mode::Authoritative,
        ),
    ] {
        let row = keys::machine_pk(&format!("n-{PRINCIPAL}"));
        let mut book = Book::new();
        let born = plan(
            &input(&[], &[roster(12, &[], Some(PRINCIPAL))]),
            &book.cells(),
        )
        .unwrap();
        apply(&mut book, &born);
        // The Mac publishes its own node, which is how the roster's new name finds this row.
        let seen = book.all();
        book.write(
            2,
            &seen,
            vec![put(&MACHINE, &row, "node", json!(PRINCIPAL))],
        );
        let cells = book.cells();
        let macs = [roster(120, &[], Some(PRINCIPAL))];
        let first = plan(&input(&sheets, &macs), &cells).unwrap();
        assert_eq!(wanted(&first, &row, "label"), Some(&json!("m120")));
        assert_eq!(wanted(&first, &row, "aliases.m12"), Some(&json!(true)));
        assert_eq!(new_machines(&first), 0);
        let cells = apply(&mut book, &first);
        let again = plan(&input(&sheets, &macs), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
        assert_eq!(new_machines(&again), 0);
        let silent = plan(&input(&[], &macs), &cells).unwrap();
        assert_eq!(wanted(&silent, &row, "aliases.m12"), Some(&json!(false)));
    }
}

/// A roster name that cannot be the label because this plan already wants another for the row stays
/// the row's alias from the first plan, so the second has nothing to add.
/// NEGATIVE CONTROL: with `label_lands` not reading this plan's want, the first plan gives the name
/// nowhere to go and the second writes it: `left: None right: Some(Bool(true))`.
#[test]
fn a_roster_name_a_birth_cannot_take_stays_an_alias_from_the_first_plan() {
    let sheets = replica(
        json!([{"label": "m36", "hostname": "m36.local", "node_id": NODE, "status": "active"}]),
        Mode::Authoritative,
    );
    let macs = [KnownMac {
        fleet_node: Some(NODE.to_owned()),
        ..roster(104, &[], None)
    }];
    let row = keys::machine_pk("m36");
    let mut book = Book::new();
    let first = plan(&input(&sheets, &macs), &book.cells()).unwrap();
    assert_eq!(new_machines(&first), 1, "{:?}", first.counts);
    assert_eq!(wanted(&first, &row, "label"), Some(&json!("m36")));
    assert_eq!(wanted(&first, &row, "aliases.m104"), Some(&json!(true)));
    let cells = apply(&mut book, &first);
    let again = plan(&input(&sheets, &macs), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
}

/// A name a source still states beside a row's own is not retracted because a clash left it unwanted
/// this run: it is what shows two rows are one Mac, and what the merge groups them by.
/// NEGATIVE CONTROL: with `finish` not reading `linked_to`, a replica row reaching that row by its
/// node retracts the name: `left: Some(Bool(false)) right: None`.
#[test]
fn a_clash_does_not_retract_the_name_it_is_about() {
    let macs = [roster(36, &[104], None)];
    let row = keys::machine_pk("m36");
    let mut book = Book::new();
    let born = plan(&input(&[], &macs), &book.cells()).unwrap();
    apply(&mut book, &born);
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&MACHINE, &row, "fleet_node", json!(NODE))],
    );
    second_holder(&mut book, "m104");
    let sheets = replica(
        json!([{"label": "m200", "hostname": "m200.local", "node_id": NODE, "status": "active"}]),
        Mode::Authoritative,
    );
    let plan = plan(&input(&sheets, &macs), &book.cells()).unwrap();
    assert_eq!(wanted(&plan, &row, "aliases.m104"), None);
    assert_eq!(wanted(&plan, &row, "aliases.m200"), Some(&json!(true)));
    assert!(kinds(&plan).contains(&"label-ambiguous"));
}

/// What a machine row assigns keeps its preference while the row reaches no machine: it may be the
/// only thing stating it, so a clash on the row's name does not end it.
/// NEGATIVE CONTROL: with `dump_computer` not calling `unsettle_assigned`, the preference ends while
/// the replica still states it: `left: Some(String("ended")) right: None`.
#[test]
fn a_row_that_reaches_no_machine_ends_no_preference() {
    let dump = |computers: Value| {
        let dump = aterm_json::to_vec(&json!({
            "ai_accounts": [{"id": "claude:ann@example.invalid", "provider": "anthropic",
                "service": "claude", "email": "ann@example.invalid", "assigned_computers": [],
                "tier": "1", "status": "active", "notes": "max plan"}],
            "computers": computers,
        }))
        .unwrap();
        dump_sheets("fleet-db-m104.json", &dump, Mode::Authoritative, &KEY).unwrap()
    };
    let sheets = dump(
        json!([{"label": "m36", "hostname": "m36.local", "status": "active",
        "assigned_accounts": ["claude:ann@example.invalid"]}]),
    );
    let mut book = Book::new();
    let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    let account = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let preference = keys::preference_pk(&keys::machine_pk("m36"), &account).unwrap();
    assert!(
        first.planned.iter().any(|p| p.pk == preference),
        "{:?}",
        first.counts
    );
    apply(&mut book, &first);
    let other = second_holder(&mut book, "m104");
    let seen = book.all();
    book.write(2, &seen, vec![elem(&MACHINE, &other, "aliases", "m36")]);
    let again = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert!(kinds(&again).contains(&"label-ambiguous"));
    let ended = again
        .planned
        .iter()
        .find(|p| p.pk == preference && p.column == "row")
        .map(|p| &p.x);
    assert_eq!(ended, None);
}

/// A node finds a row only when the row's principal does not say it is another Mac's: the roster's
/// name, linked to that node by a replica or stating it itself, is a finding, and the row keeps its
/// label.
/// NEGATIVE CONTROL: with `Planner::found` not reading `other_mac` on a node's row, the roster's name
/// lands on it: `left: Some(String("m40")) right: None`. With `known_found` not reading it, the same.
#[test]
fn a_node_on_another_principals_row_finds_it_nothing() {
    let row = keys::machine_pk("m7");
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &row, "label", json!("m7")),
            put(&MACHINE, &row, "fleet_node", json!(NODE)),
            put(&MACHINE, &row, "node", json!(OTHER_PRINCIPAL)),
            put(&MACHINE, &row, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let sheets = replica(
        json!([{"label": "m40", "hostname": "m40.local", "node_id": NODE, "status": "active"}]),
        Mode::Authoritative,
    );
    let macs = [roster(40, &[], Some(PRINCIPAL))];
    let linked = plan(&input(&sheets, &macs), &cells).unwrap();
    assert_eq!(wanted(&linked, &row, "label"), None);
    assert!(kinds(&linked).contains(&"label-ambiguous"));
    let macs = [KnownMac {
        fleet_node: Some(NODE.to_owned()),
        ..roster(40, &[], Some(PRINCIPAL))
    }];
    let stated = plan(&input(&[], &macs), &cells).unwrap();
    assert_eq!(wanted(&stated, &row, "label"), None);
    assert!(kinds(&stated).contains(&"node-ambiguous"));
}

/// Two rows of a replica on one machine stating different words of `status` settle on the word that
/// says something, whichever is listed first, with a finding, and a second plan writes nothing.
/// NEGATIVE CONTROL: with `machine_statuses` applying the words in table order, an `active` listed
/// first says nothing and the disagreement goes unreported:
/// `assertion failed: kinds(&first).contains(&"sources-disagree")`.
#[test]
fn two_rows_on_one_machine_settle_on_one_status() {
    for computers in [
        json!([
            {"label": "m50", "hostname": "m50.local", "node_id": NODE, "status": "active"},
            {"label": "m51", "hostname": "m51.local", "node_id": NODE, "status": "parked"},
        ]),
        json!([
            {"label": "m51", "hostname": "m51.local", "node_id": NODE, "status": "parked"},
            {"label": "m50", "hostname": "m50.local", "node_id": NODE, "status": "active"},
        ]),
    ] {
        let sheets = replica(computers, Mode::Authoritative);
        let mut book = Book::new();
        let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
        assert_eq!(new_machines(&first), 1, "{:?}", first.counts);
        let status = first
            .planned
            .iter()
            .find(|p| p.collection == MACHINE.name && p.column == "status")
            .map(|p| &p.x);
        assert_eq!(status, Some(&json!("parked")));
        assert!(kinds(&first).contains(&"sources-disagree"));
        let cells = apply(&mut book, &first);
        let again = plan(&input(&sheets, &[]), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
        assert!(kinds(&again).contains(&"sources-disagree"));
    }
}

/// A Mac has two node cells and each source states one: the roster's principal finds `node`, a
/// replica's `node_id` finds `fleet_node`, so a row holding both is one row under either. The
/// import writes neither from the roster: `node` is the Mac's own to publish.
/// NEGATIVE CONTROL: with `known_found` asking the `fleet_node` cell for the principal, the roster's
/// new name births beside the row: `left: 1 right: 0`.
#[test]
fn each_source_finds_a_row_by_the_node_cell_it_states() {
    let mut book = Book::new();
    let row = relabelled(&mut book);
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&MACHINE, &row, "node", json!(PRINCIPAL))],
    );
    let cells = book.cells();
    let macs = [roster(300, &[], Some(PRINCIPAL))];
    let by_roster = plan(&input(&[], &macs), &cells).unwrap();
    assert_eq!(new_machines(&by_roster), 0, "{:?}", by_roster.counts);
    assert_eq!(wanted(&by_roster, &row, "label"), Some(&json!("m300")));
    assert_eq!(wanted(&by_roster, &row, "node"), None);
    assert!(
        by_roster
            .findings
            .iter()
            .any(|f| f.kind == Kind::AdoptedByNode
                && f.pk.as_deref() == Some(row.as_str())
                && f.column.as_deref() == Some("node"))
    );
    let sheets = replica(
        json!([{"label": "m301", "hostname": "m301.local", "node_id": NODE, "status": "active"}]),
        Mode::Authoritative,
    );
    let by_replica = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&by_replica), 0, "{:?}", by_replica.counts);
    assert_eq!(
        wanted(&by_replica, &row, "aliases.m301"),
        Some(&json!(true))
    );
    let older = [KnownMac {
        label: Some(Label::plain(302)),
        aliases: Vec::new(),
        node: None,
        fleet_node: Some(NODE.to_owned()),
        hosts: Vec::new(),
    }];
    let by_transport = plan(&input(&[], &older), &cells).unwrap();
    assert_eq!(new_machines(&by_transport), 0, "{:?}", by_transport.counts);
    assert_eq!(wanted(&by_transport, &row, "label"), Some(&json!("m302")));
    assert!(
        by_transport
            .findings
            .iter()
            .any(|f| f.kind == Kind::AdoptedByNode && f.column.as_deref() == Some("fleet_node"))
    );
}

/// The one word fleet_db wrote for every row says nothing of a row no Mac has joined: it stays
/// `planned`, on its birth and on every run after, a stated node or not, so the plan settles and
/// the replica's machine cells judge covered. Another word is a statement and is carried, on a
/// birth as on a held row.
/// NEGATIVE CONTROL: with `dump_computer`'s `says_nothing` always false, a birth goes `active`:
/// `left: Some(String("active")) right: Some(String("planned"))`. With the old rule (`status`
/// skipped only for a row minted this run) the second plan writes `status` and `src`, which is what
/// left 38 live rows' 76 cells `unwritten`: the `again.rows.is_empty()` assertion prints that plan.
#[test]
fn active_says_nothing_of_a_planned_row_and_the_plan_settles() {
    let sheets = replica(
        json!([
            {"label": "m50", "hostname": "m50.local", "make": "Apple", "status": "active"},
            {"label": "m51", "hostname": "m51.local", "node_id": NODE, "status": "active"},
            {"label": "m52", "hostname": "m52.local", "node_id": OTHER_NODE, "status": "parked"},
            {"label": "m53", "hostname": "m53.local", "status": "parked"},
        ]),
        Mode::Authoritative,
    );
    let mut book = Book::new();
    let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    for (label, status) in [
        ("m50", "planned"),
        ("m51", "planned"),
        ("m52", "parked"),
        ("m53", "parked"),
    ] {
        let pk = keys::machine_pk(label);
        assert_eq!(
            wanted(&first, &pk, "status"),
            Some(&json!(status)),
            "{label}"
        );
        assert_eq!(step(&first, &pk, "status"), Some(&Step::Write), "{label}");
    }
    assert!(
        !kinds(&first).contains(&"sources-disagree"),
        "{:?}",
        first.findings
    );
    let cells = apply(&mut book, &first);
    let again = plan(&input(&sheets, &[]), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
    assert_eq!(wanted(&again, &keys::machine_pk("m50"), "status"), None);
    // On a row that is not `planned` the word is a statement like any other.
    let back = replica(
        json!([{"label": "m53", "hostname": "m53.local", "status": "active"}]),
        Mode::Authoritative,
    );
    let moved = plan(&input(&back, &[]), &cells).unwrap();
    assert_eq!(
        wanted(&moved, &keys::machine_pk("m53"), "status"),
        Some(&json!("active"))
    );
    let uncovered = covered::judge(&again, &|_| false);
    assert!(uncovered.is_empty(), "{uncovered:?}");
    assert_eq!(new_machines(&again), 0);
    // The owner's `planned` needs no finding from a word that says nothing.
    let owner = keys::machine_pk("m59");
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &owner, "label", json!("m59")),
            put(&MACHINE, &owner, "status", json!("planned")),
            put(&MACHINE, &owner, "row", json!("live")),
        ],
    );
    let sheets = replica(
        json!([{"label": "m59", "hostname": "m59.local", "status": "active"}]),
        Mode::Authoritative,
    );
    let quiet = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    assert!(
        !kinds(&quiet).contains(&"owner-kept"),
        "{:?}",
        quiet.findings
    );
}

/// A logins sheet comparing against the vault sheet's machine, named by an alias a row holds, reads
/// the same row: nothing is compared against a placeholder only `machine_key` imagined.
/// NEGATIVE CONTROL: with `machine_key` answering every name from its birth key, the lower source
/// disagrees over its own machine: `kinds` holds `sources-disagree`.
#[test]
fn a_lower_source_compares_against_the_row_the_alias_means() {
    let mut book = Book::new();
    relabelled(&mut book);
    let cells = book.cells();
    let sheets = [
        vault(&[",ann@example.invalid,m36,,1,,,,,,,,"]),
        logins(&["m104,ann@example.invalid,claude,box@proton.example,x,"]),
    ];
    let plan = plan(&input(&sheets, &[]), &cells).unwrap();
    assert_eq!(new_machines(&plan), 0, "{:?}", plan.counts);
    assert!(
        !kinds(&plan).contains(&"sources-disagree"),
        "{:?}",
        plan.findings
    );
    // The name stays the row's while that sheet says it.
    assert_eq!(
        wanted(&plan, &keys::machine_pk("m36"), "aliases.m104"),
        Some(&json!(true))
    );
}

fn computers_sheet(rows: &[&str]) -> Sheet {
    let mut bytes =
        b"ID,Make,Arch,OS,Processor,RAM,HD,Aliases,Nickname,Nickname Aliases,Location,Notes\n"
            .to_vec();
    for row in rows {
        bytes.extend_from_slice(row.as_bytes());
        bytes.push(b'\n');
    }
    Sheet::parse(SheetKind::Computers, "alab-computers.csv", &bytes).unwrap()
}

fn aliases_of(cells: &Cells, pk: &str) -> Vec<String> {
    cell::project(&MACHINE, cells).rows[pk]
        .fields
        .get("aliases")
        .map(|field| field.members().iter().map(|m| (*m).to_owned()).collect())
        .unwrap_or_default()
}

/// Every name on more than one live row.
fn names_on_two_rows(cells: &Cells) -> Vec<String> {
    let machines = cell::project(&MACHINE, cells);
    let mut held: BTreeMap<&str, usize> = BTreeMap::new();
    for row in machines.rows.values().filter(|row| row.live()) {
        for name in row_names(row) {
            *held.entry(name).or_default() += 1;
        }
    }
    held.into_iter()
        .filter(|(_, rows)| *rows > 1)
        .map(|(name, _)| name.to_owned())
        .collect()
}

fn label_at(cells: &Cells, pk: &str) -> Option<Value> {
    cell::project(&MACHINE, cells).rows[pk]
        .get("label")
        .cloned()
}

/// A replica a fuller source of the same import wrote first adds no cell: the plan of it alone wants
/// only its own source row, and that is no cell of the replica, so it judges covered for good. A row
/// the registry lacks still does not.
/// NEGATIVE CONTROL: with `judge` calling any plan that writes uncovered, a replica repeating the sheet never
/// settles: `[Uncovered { kind: "unwritten", collection: None, pk: None, column: None }]`.
#[test]
fn a_replica_a_fuller_source_wrote_judges_covered() {
    let computers = json!([{"label": "m50", "hostname": "m50.local", "make": "Apple",
        "status": "active"}]);
    let mut sheets = vec![computers_sheet(&["m50,Apple,,,,,,,,,,"])];
    sheets.extend(replica(computers.clone(), Mode::FillOnly));
    let mut book = Book::new();
    let first = plan(&input(&sheets, &[]), &book.cells()).unwrap();
    let cells = apply(&mut book, &first);
    let alone = plan(&input(&replica(computers, Mode::FillOnly), &[]), &cells).unwrap();
    assert_eq!(alone.rows.len(), 1, "{:?}", alone.planned);
    assert_eq!(
        alone.rows[0][0].collection,
        crate::alab::registry::SOURCE.name
    );
    let uncovered = covered::judge(&alone, &|_| false);
    assert!(uncovered.is_empty(), "{uncovered:?}");
    let more = json!([{"label": "m51", "hostname": "m51.local", "status": "active"}]);
    let more = plan(&input(&replica(more, Mode::FillOnly), &[]), &cells).unwrap();
    assert!(!covered::judge(&more, &|_| false).is_empty());
}

/// A table that moves an alias from one row it lists to another moves it in one plan, whether the
/// row it moves to exists or not: no finding, no fold into the row that held it, and nothing left
/// for the next plan.
/// NEGATIVE CONTROL: with `rows_named` never asking `gives_up`, `m7` clashes on the name: `kinds` holds
/// `label-ambiguous`, printed as `[Finding { kind: LabelAmbiguous, collection: Some("machine"), pk: None, ..`.
#[test]
fn a_name_a_table_moved_to_another_row_settles_in_one_plan() {
    let holder = keys::machine_pk("m5");
    let row = keys::machine_pk("m7");
    for before in [
        vec!["m5,Apple,,,,,,m104,,,,"],
        vec!["m5,Apple,,,,,,m104,,,,", "m7,Apple,,,,,,,,,,"],
    ] {
        let mut book = Book::new();
        let first = plan(&input(&[computers_sheet(&before)], &[]), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
        let after = [computers_sheet(&[
            "m5,Apple,,,,,,,,,,",
            "m7,Apple,,,,,,m104,,,,",
        ])];
        let moved = plan(&input(&after, &[]), &cells).unwrap();
        assert!(
            !kinds(&moved).contains(&"label-ambiguous"),
            "{:?}",
            moved.findings
        );
        let cells = apply(&mut book, &moved);
        assert!(aliases_of(&cells, &holder).is_empty());
        assert_eq!(aliases_of(&cells, &row), ["m104"]);
        let again = plan(&input(&after, &[]), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
    }
}

/// The row that gives a name up is in the plan that takes it, also where its own table row found no
/// row, so the name never ends up on two rows.
/// NEGATIVE CONTROL: with `names_given_up` leaving the holder out of the plan, `m5` keeps the name `m7`
/// now holds too: `assertion failed: aliases_of(&cells, &holder).is_empty()`. With `disowns` ignoring
/// `contested`, the last row loses the name: `left: [] right: ["m104"]`.
#[test]
fn the_row_giving_a_name_up_is_in_the_plan_that_takes_it() {
    let holder = keys::machine_pk("m5");
    let mut book = Book::new();
    let before = [computers_sheet(&["m5,Apple,,,,,,m104,,,,"])];
    let first = plan(&input(&before, &[]), &book.cells()).unwrap();
    apply(&mut book, &first);
    let other = keys::machine_pk("m5/2");
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &other, "label", json!("m5")),
            put(&MACHINE, &other, "row", json!("live")),
        ],
    );
    let after = [computers_sheet(&[
        "m5,Apple,,,,,,,,,,",
        "m7,Apple,,,,,,m104,,,,",
    ])];
    let moved = plan(&input(&after, &[]), &book.cells()).unwrap();
    assert!(
        kinds(&moved).contains(&"label-ambiguous"),
        "{:?}",
        moved.findings
    );
    let cells = apply(&mut book, &moved);
    assert!(aliases_of(&cells, &holder).is_empty());
    assert_eq!(aliases_of(&cells, &keys::machine_pk("m7")), ["m104"]);

    // A name two sources give to two rows is taken from no row this plan does not otherwise reach.
    let mut book = Book::new();
    let first = plan(&input(&before, &[]), &book.cells()).unwrap();
    apply(&mut book, &first);
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &other, "label", json!("m5")),
            put(&MACHINE, &other, "row", json!("live")),
        ],
    );
    let mut contested = replica(
        json!([{"label": "m8", "hostname": "m8.local", "aliases": ["m104"], "status": "active"}]),
        Mode::Authoritative,
    );
    contested.push(computers_sheet(&[
        "m5,Apple,,,,,,,,,,",
        "m7,Apple,,,,,,m104,,,,",
    ]));
    let held = plan(&input(&contested, &[]), &book.cells()).unwrap();
    let cells = apply(&mut book, &held);
    assert_eq!(aliases_of(&cells, &holder), ["m104"]);
}

/// A name no table moves stays the row's, which finds it for a source that goes by another name: a
/// row of another table that states the name (a table lists its own rows, no other's), and a name a
/// table's own row states beside a label it contests.
/// NEGATIVE CONTROL: with `disowns` reading the alias off any table, the other source's `m7` births a row:
/// `left: 1 right: 0`. With `disowns` ignoring the table's own row, the replica's `m104` is born under
/// its own label: `left: Some(String("m104")) right: None`.
#[test]
fn a_name_no_table_moved_still_finds_its_row() {
    let holder = keys::machine_pk("m5");
    let seeded = || {
        let mut book = Book::new();
        let first = plan(
            &input(&[computers_sheet(&["m5,Apple,,,,,,m104,,,,"])], &[]),
            &book.cells(),
        )
        .unwrap();
        let cells = apply(&mut book, &first);
        (book, cells)
    };
    let (_, cells) = seeded();
    let mut apart = replica(
        json!([{"label": "m7", "hostname": "m7.local", "aliases": ["m104"], "status": "active"}]),
        Mode::Authoritative,
    );
    apart.push(computers_sheet(&["m5,Apple,,,,,,,,,,"]));
    let found = plan(&input(&apart, &[]), &cells).unwrap();
    assert_eq!(new_machines(&found), 0, "{:?}", found.counts);
    assert_eq!(wanted(&found, &holder, "aliases.m7"), Some(&json!(true)));

    let (_, cells) = seeded();
    let mut own = replica(
        json!([{"label": "m104", "hostname": "m104.local", "status": "active"}]),
        Mode::Authoritative,
    );
    own.push(computers_sheet(&[
        "m5,Apple,,,,,,m104,,,,",
        "m6,Apple,,,,,,m5,,,,",
    ]));
    let found = plan(&input(&own, &[]), &cells).unwrap();
    assert_eq!(wanted(&found, &keys::machine_pk("m104"), "label"), None);
}

/// A name moved beside another the row keeps (`m104` and `m6` together, off a row holding both) stays
/// a finding, the two names showing the row is that Mac: nothing is born and no name has two holders.
/// A row the sources link to the table's row by its own label is that row, the owner's alias and all.
/// NEGATIVE CONTROL: with `gives_up` ignoring `linked_to`, or `holds_disowned` answering `false`, the moved
/// names raise no finding: the assertion prints `[]`. With `holds_disowned` ignoring the label, the
/// linked row is a finding: `[Finding { kind: LabelAmbiguous, ..`.
#[test]
fn a_name_linked_to_what_a_row_keeps_stays_on_it() {
    let holder = keys::machine_pk("m5");
    let mut book = Book::new();
    let first = plan(
        &input(&[computers_sheet(&["m5,Apple,,,,,,m104 m6,,,,"])], &[]),
        &book.cells(),
    )
    .unwrap();
    let cells = apply(&mut book, &first);
    assert_eq!(aliases_of(&cells, &holder), ["m104", "m6"]);
    let moved = [computers_sheet(&[
        "m5,Apple,,,,,,,,,,",
        "m7,Apple,,,,,,m104 m6,,,,",
    ])];
    let held = plan(&input(&moved, &[]), &cells).unwrap();
    assert!(
        kinds(&held).contains(&"label-ambiguous"),
        "{:?}",
        held.findings
    );
    assert_eq!(new_machines(&held), 0, "{:?}", held.counts);
    let cells = apply(&mut book, &held);
    assert_eq!(aliases_of(&cells, &holder), ["m104", "m6"]);

    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &holder, "label", json!("m5")),
            elem(&MACHINE, &holder, "aliases", "m104"),
            put(&MACHINE, &holder, "row", json!("live")),
        ],
    );
    let mut linked = replica(
        json!([{"label": "m104", "hostname": "m104.local", "aliases": ["m5"], "status": "active"}]),
        Mode::Authoritative,
    );
    linked.push(computers_sheet(&[
        "m5,Apple,,,,,,,,,,",
        "m7,Apple,,,,,,m104,,,,",
    ]));
    let found = plan(&input(&linked, &[]), &book.cells()).unwrap();
    assert!(
        !kinds(&found).contains(&"label-ambiguous"),
        "{:?}",
        found.findings
    );
    assert_eq!(new_machines(&found), 0, "{:?}", found.counts);
    assert_eq!(wanted(&found, &holder, "aliases.m7"), Some(&json!(true)));
}

/// The owner's alias stands against a table that moves the name: a finding, nothing folded into the
/// row that holds it, no row born for the table's row, and no second holder. A table that states
/// neither what the row is called nor a second row for the name still finds the row, as a renamed
/// Mac's old name does.
/// NEGATIVE CONTROL: with `gives_up` taking any writer's alias, the table's `m7` is born with no finding:
/// the `label-ambiguous` assertion prints `[]`. With `holds_disowned` answering `false`, `m7` folds into
/// the owner's row, and the same assertion prints `[]`.
#[test]
fn an_owners_alias_stands_against_a_table_that_moves_it() {
    let mut book = Book::new();
    let holder = keys::machine_pk("m5");
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &holder, "label", json!("m5")),
            elem(&MACHINE, &holder, "aliases", "m104"),
            put(&MACHINE, &holder, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let moved = [computers_sheet(&[
        "m5,Apple,,,,,,,,,,",
        "m7,Apple,,,,,,m104,,,,",
    ])];
    let held = plan(&input(&moved, &[]), &cells).unwrap();
    assert!(
        kinds(&held).contains(&"label-ambiguous"),
        "{:?}",
        held.findings
    );
    assert_eq!(new_machines(&held), 0, "{:?}", held.counts);
    assert_eq!(wanted(&held, &holder, "aliases.m7"), None);
    assert_eq!(wanted(&held, &holder, "aliases.m104"), None);
    let found = [computers_sheet(&["m7,Apple,,,,,,m104,,,,"])];
    let found = plan(&input(&found, &[]), &cells).unwrap();
    assert_eq!(new_machines(&found), 0, "{:?}", found.counts);
    assert_eq!(wanted(&found, &holder, "aliases.m7"), Some(&json!(true)));
}

/// A chain of roster renames lands whole in one plan, whichever way the roster lists it, with no
/// finding and nothing left for the next plan. A row that trades labels with another stays as it is,
/// a finding on both.
/// NEGATIVE CONTROL: with `label_clash` ignoring `leaves`, the Mac that waits never lands:
/// `[Finding { kind: LabelAmbiguous, ..`, and likewise the three-Mac chain with one pass of `waits`. With
/// no finding for a label that never lands, the trade prints `left: 0 right: 2`. With `leaves` trusting a
/// label that cannot land, the assertion prints `[Finding { kind: OwnerKept, ..`, with no clash found;
/// ignoring an alias the row holds, it prints `[]`; ignoring one it wants, the waiting Mac lands:
/// `left: Some(String("m2")) right: Some(String("m1"))`, as it does with `label_clash` ignoring
/// `contested`.
#[test]
fn a_chain_of_roster_renames_settles_in_one_plan() {
    let was = [
        roster(1, &[], Some(PRINCIPAL)),
        roster(2, &[], Some(OTHER_PRINCIPAL)),
    ];
    let label_of = |cells: &Cells, node: &str| {
        cell::project(&MACHINE, cells).rows[&keys::machine_pk(&format!("n-{node}"))]
            .get("label")
            .cloned()
    };
    let start = || {
        let mut book = Book::new();
        let first = plan(&input(&[], &was), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        assert_eq!(label_of(&cells, PRINCIPAL), Some(json!("m1")));
        assert_eq!(label_of(&cells, OTHER_PRINCIPAL), Some(json!("m2")));
        (book, cells)
    };
    let a = roster(2, &[], Some(PRINCIPAL));
    let b = roster(3, &[], Some(OTHER_PRINCIPAL));
    for now in [[a.clone(), b.clone()], [b, a]] {
        let (mut book, cells) = start();
        let moved = plan(&input(&[], &now), &cells).unwrap();
        assert!(
            !kinds(&moved).contains(&"label-ambiguous"),
            "{:?}",
            moved.findings
        );
        let cells = apply(&mut book, &moved);
        assert_eq!(label_of(&cells, PRINCIPAL), Some(json!("m2")));
        assert_eq!(label_of(&cells, OTHER_PRINCIPAL), Some(json!("m3")));
        let again = plan(&input(&[], &now), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
    }
    let (_, cells) = start();
    let swap = [
        roster(2, &[], Some(PRINCIPAL)),
        roster(1, &[], Some(OTHER_PRINCIPAL)),
    ];
    let swapped = plan(&input(&[], &swap), &cells).unwrap();
    let held = swapped
        .findings
        .iter()
        .filter(|finding| finding.kind == Kind::LabelAmbiguous)
        .count();
    assert_eq!(held, 2, "{:?}", swapped.findings);
    assert!(
        swapped
            .planned
            .iter()
            .all(|planned| planned.column != "label" || planned.step != Step::Write),
        "{:?}",
        swapped.planned
    );

    // Each pass lands what the last one freed, however long the chain and whichever Mac comes first.
    const THIRD: &str = "aabbccddeeff0011";
    let mut book = Book::new();
    let three = [
        roster(1, &[], Some(PRINCIPAL)),
        roster(2, &[], Some(OTHER_PRINCIPAL)),
        roster(3, &[], Some(THIRD)),
    ];
    let first = plan(&input(&[], &three), &book.cells()).unwrap();
    let cells = apply(&mut book, &first);
    let renamed = [
        roster(2, &[], Some(PRINCIPAL)),
        roster(3, &[], Some(OTHER_PRINCIPAL)),
        roster(4, &[], Some(THIRD)),
    ];
    let moved = plan(&input(&[], &renamed), &cells).unwrap();
    assert!(
        !kinds(&moved).contains(&"label-ambiguous"),
        "{:?}",
        moved.findings
    );
    let cells = apply(&mut book, &moved);
    assert_eq!(label_of(&cells, PRINCIPAL), Some(json!("m2")));
    assert_eq!(label_of(&cells, THIRD), Some(json!("m4")));

    // A row that keeps the label after all keeps it from the Mac that waits: the owner labelled it,
    // it holds the name as an alias too, or a sheet still says it.
    let moves = [
        roster(2, &[], Some(PRINCIPAL)),
        roster(3, &[], Some(OTHER_PRINCIPAL)),
    ];
    let second = keys::machine_pk(&format!("n-{OTHER_PRINCIPAL}"));
    let stays = |book: &mut Book, sheets: &[Sheet], cells: Cells| {
        let held = plan(&input(sheets, &moves), &cells).unwrap();
        assert!(
            kinds(&held).contains(&"label-ambiguous"),
            "{:?}",
            held.findings
        );
        let cells = apply(book, &held);
        assert_eq!(label_of(&cells, PRINCIPAL), Some(json!("m1")));
        cells
    };
    let (mut book, _) = start();
    let seen = book.all();
    book.write(2, &seen, vec![put(&MACHINE, &second, "label", json!("m2"))]);
    let cells = book.cells();
    let cells = stays(&mut book, &[], cells);
    assert_eq!(label_of(&cells, OTHER_PRINCIPAL), Some(json!("m2")));
    let (mut book, _) = start();
    book.write(2, &[], vec![elem(&MACHINE, &second, "aliases", "m2")]);
    let cells = book.cells();
    stays(&mut book, &[], cells);
    let (mut book, cells) = start();
    let sheet = [computers_sheet(&["m2,Apple,,,,,,,,,,"])];
    let cells = stays(&mut book, &sheet, cells);
    assert_eq!(label_of(&cells, OTHER_PRINCIPAL), Some(json!("m3")));
    assert_eq!(aliases_of(&cells, &second), ["m2"]);

    // A label a source contests never lands, on a row its node found and no row holding it.
    let mut book = Book::new();
    let one = [roster(1, &[], Some(PRINCIPAL))];
    let first = plan(&input(&[], &one), &book.cells()).unwrap();
    let cells = apply(&mut book, &first);
    let sheet = [computers_sheet(&[
        "m8,Apple,,,,,,m2,,,,",
        "m9,Apple,,,,,,m2,,,,",
    ])];
    let contested = [roster(2, &[], Some(PRINCIPAL))];
    let held = plan(&input(&sheet, &contested), &cells).unwrap();
    assert!(
        kinds(&held).contains(&"label-ambiguous"),
        "{:?}",
        held.findings
    );
    let cells = apply(&mut book, &held);
    assert_eq!(label_of(&cells, PRINCIPAL), Some(json!("m1")));
}

/// A phone label a replica's phones table states replaces one an import made up for a number given
/// with none (by a phones or emails sheet, or by a replica's emails table), in the same run and in a
/// later one under `--fill-only` too, each in one plan that a plan over its result finds nothing to
/// add to. A made-up label keeps clear of the found phone's new one, and a row the replica holds a
/// concurrent delete for, minted for its number, takes the label it states. A label a replica's
/// phones table wrote is judged as any cell is: a later fill-only dump's other label differs, and is
/// not written.
/// NEGATIVE CONTROL: with `finish` never letting a replica's label replace one an import made up,
/// taking every dump table to state phone labels, or `phone` given no label for a row the replica
/// holds a concurrent delete for: `[Uncovered { kind: "differs", collection: Some("phone"), ..
/// column: Some("label") }]`. With every phone label an import wrote taken for one it made up, a
/// later fill-only dump relabels the found phone: `[Planned { collection: "phone", .. column:
/// "label", x: String("kitchen"), step: Write }, ..`.
#[test]
fn a_phone_label_a_replica_states_replaces_one_an_import_made_up() {
    let sheets = |phones: Value, mode: Mode| {
        dump_sheets(
            "fleet-db-m104.json",
            &aterm_json::to_vec(&phones).unwrap(),
            mode,
            &KEY,
        )
        .unwrap()
    };
    let numbered = |pairs: &[(&str, &str)]| {
        let phones: Vec<Value> = pairs
            .iter()
            .map(|(last4, label)| json!({"phone": format!("+1415555{last4}"), "label": label}))
            .collect();
        json!({ "phones": phones })
    };
    let labels = |cells: &Cells| -> BTreeMap<String, String> {
        cell::project(&crate::alab::registry::PHONE, cells)
            .rows
            .values()
            .filter_map(|row| {
                let last4 = row.get("last4")?.as_str()?.to_owned();
                Some((last4, row.get("label")?.as_str()?.to_owned()))
            })
            .collect()
    };
    let settles = |book: &mut Book, sheets: &[Sheet]| {
        let read = plan(&input(sheets, &[]), &book.cells()).unwrap();
        let cells = apply(book, &read);
        let again = plan(&input(sheets, &[]), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
        let judged = covered::judge(&again, &|_| false);
        assert!(
            !judged
                .iter()
                .any(|uncovered| uncovered.column.as_deref() == Some("label")),
            "{judged:?}"
        );
        cells
    };
    let want = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(last4, label)| ((*last4).to_owned(), (*label).to_owned()))
            .collect()
    };

    // A phones sheet states no label: the replica's lands beside it in one run, and over the label
    // an earlier run made up for the sheet's number in the next.
    let csv = Sheet::parse(
        SheetKind::Phones,
        "alab-phones.csv",
        b"Phone Number,Carrier / Provider\n415-555-0102,Tello Mobile\n",
    )
    .unwrap();
    let desk = sheets(numbered(&[("0102", "desk")]), Mode::FillOnly);
    let mut together = vec![csv.clone()];
    together.extend(desk.iter().cloned());
    let mut book = Book::new();
    let cells = settles(&mut book, &together);
    assert_eq!(labels(&cells), want(&[("0102", "desk")]));
    let mut book = Book::new();
    let cells = settles(&mut book, std::slice::from_ref(&csv));
    assert_eq!(labels(&cells), want(&[("0102", "phone-1")]));
    let cells = settles(&mut book, &desk);
    assert_eq!(labels(&cells), want(&[("0102", "desk")]));
    // So does a replica's emails table, whose phone numbers carry no label.
    let mut book = Book::new();
    let emails = json!({"emails": [{"email": "ann@example.invalid", "phone": "+14155550109"}]});
    let cells = settles(&mut book, &sheets(emails, Mode::FillOnly));
    assert_eq!(labels(&cells), want(&[("0109", "phone-1")]));
    let kitchen = numbered(&[("0109", "kitchen")]);
    let cells = settles(&mut book, &sheets(kitchen, Mode::FillOnly));
    assert_eq!(labels(&cells), want(&[("0109", "kitchen")]));

    // An address's number the phones table lacks is minted a label clear of the found phone's new
    // one.
    let mut book = Book::new();
    settles(
        &mut book,
        &sheets(numbered(&[("0101", "phone-1")]), Mode::FillOnly),
    );
    let mut both = numbered(&[("0101", "phone-2")]);
    both.as_object_mut().unwrap().insert(
        "emails".to_owned(),
        json!([{"email": "ann@example.invalid", "phone": "+14155550109"}]),
    );
    let cells = settles(&mut book, &sheets(both, Mode::Authoritative));
    assert_eq!(
        labels(&cells),
        want(&[("0101", "phone-2"), ("0109", "phone-3")])
    );

    // A row the replica holds a concurrent delete for, minted for its number, takes its label.
    let mut book = Book::new();
    let deleted = json!({"phones": [{"phone": "+14155550102", "label": "desk",
        "_conflicts": [{"_deleted": true}]}]});
    let read = plan(&input(&sheets(deleted, Mode::FillOnly), &[]), &book.cells()).unwrap();
    apply(&mut book, &read);
    let cells = settles(&mut book, &desk);
    assert_eq!(labels(&cells), want(&[("0102", "desk")]));

    // A label a replica's phones table wrote is judged as any cell is: another a later fill-only dump
    // states differs, and is not written over it.
    let number = |label: &str| json!({"phones": [{"phone": "+14155550100", "label": label}]});
    let mut book = Book::new();
    let first = dump_sheets(
        "fleet-db-m104.json",
        &aterm_json::to_vec(&number("desk")).unwrap(),
        Mode::FillOnly,
        &KEY,
    )
    .unwrap();
    let minted = plan(&input(&first, &[]), &book.cells()).unwrap();
    let cells = apply(&mut book, &minted);
    let renamed = dump_sheets(
        "fleet-db-m200.json",
        &aterm_json::to_vec(&number("kitchen")).unwrap(),
        Mode::FillOnly,
        &KEY,
    )
    .unwrap();
    let read = plan(&input(&renamed, &[]), &cells).unwrap();
    let judged = covered::judge(&read, &|_| false);
    assert!(
        judged
            .iter()
            .any(|uncovered| uncovered.column.as_deref() == Some("label")),
        "{judged:?}"
    );
    assert!(
        !read
            .planned
            .iter()
            .any(|planned| planned.column == "label" && planned.step == Step::Write),
        "{:?}",
        read.planned
    );
    let again = plan(&input(&first, &[]), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
}

/// A row giving a name up leaves the name in the plan that takes it, whichever way the taker gets
/// it: the roster's label on a row it holds, or on a row it births, where the table's own row that
/// states the name found nothing.
/// NEGATIVE CONTROL: with `names_given_up` leaving the giver out of the plan, or not run:
/// `assertion failed: aliases_of(&cells, &giver).is_empty()`. With `rows_named` never asking
/// `gives_up`, the roster's label never lands: `left: Some(String("m1")) right: Some(String("m2"))`.
#[test]
fn a_name_given_up_leaves_its_row_however_it_is_taken() {
    let giver = keys::machine_pk("m5");
    let others = roster(7, &[], Some(OTHER_PRINCIPAL));
    let held = (
        vec![roster(1, &[], Some(PRINCIPAL)), others.clone()],
        vec!["m5,,,,,,,,,,,", "m7,,,,,,,m2,,,,"],
        vec![roster(2, &[], Some(PRINCIPAL)), others.clone()],
    );
    // `m8` contests `m7`, so the name `m2` links to no other Mac and the roster births its row.
    let born = (
        Vec::new(),
        vec!["m5,,,,,,,,,,,", "m7,,,,,,,m2,,,,", "m8,,,,,,,m7,,,,"],
        vec![roster(2, &[], Some(PRINCIPAL))],
    );
    for (was, after, now) in [held, born] {
        let mut book = Book::new();
        let before = [computers_sheet(&["m5,,,,,,,m2,,,,"])];
        let first = plan(&input(&before, &was), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        assert_eq!(aliases_of(&cells, &giver), ["m2"]);
        let moved = plan(&input(&[computers_sheet(&after)], &now), &cells).unwrap();
        let cells = apply(&mut book, &moved);
        let taker = keys::machine_pk(&format!("n-{PRINCIPAL}"));
        assert_eq!(label_at(&cells, &taker), Some(json!("m2")));
        assert!(aliases_of(&cells, &giver).is_empty());
        assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
    }
}

/// A roster name a row cannot take as its label lands as its alias while the row holding it moves
/// off, in one plan, whichever way the roster lists them: no finding, nothing left for the next plan.
/// NEGATIVE CONTROL: with `land_label` asking `alias_element` for the name, which sees the row still
/// leaving it: `[Finding { kind: OwnerKept, .. }, Finding { kind: LabelAmbiguous, collection:
/// Some("machine"), pk: Some("mc-332a629b80e94d25"), ..`. With the alias branch dropped: `left: []
/// right: ["m2"]`.
#[test]
fn a_name_a_row_cannot_take_as_its_label_moves_with_the_row_leaving_it() {
    let owned = keys::machine_pk(&format!("n-{PRINCIPAL}"));
    let moving = keys::machine_pk(&format!("n-{OTHER_PRINCIPAL}"));
    let (a, b) = (
        roster(2, &[], Some(PRINCIPAL)),
        roster(3, &[], Some(OTHER_PRINCIPAL)),
    );
    for now in [[a.clone(), b.clone()], [b, a]] {
        let mut book = Book::new();
        let was = [roster(2, &[], Some(OTHER_PRINCIPAL))];
        let first = plan(&input(&[], &was), &book.cells()).unwrap();
        apply(&mut book, &first);
        let seen = book.all();
        book.write(
            2,
            &seen,
            vec![
                put(&MACHINE, &owned, "label", json!("m1")),
                put(&MACHINE, &owned, "row", json!("live")),
            ],
        );
        let moved = plan(&input(&[], &now), &book.cells()).unwrap();
        assert!(
            !kinds(&moved).contains(&"label-ambiguous"),
            "{:?}",
            moved.findings
        );
        let cells = apply(&mut book, &moved);
        assert_eq!(label_at(&cells, &owned), Some(json!("m1")));
        assert_eq!(aliases_of(&cells, &owned), ["m2"]);
        assert_eq!(label_at(&cells, &moving), Some(json!("m3")));
        let again = plan(&input(&[], &now), &cells).unwrap();
        assert!(again.rows.is_empty(), "{:?}", again.planned);
    }
}

/// A plan moving names reaches the log so that no prefix of it shows a name on two rows, whichever
/// row's key sorts first: a run can stop between records and a replica can hold only the first.
/// NEGATIVE CONTROL: with `free_names_first` not run, a prefix shows the moved label on two rows:
/// `left: ["m2"] right: []`; likewise with `land_label` pairing no row it frees, a pair waiting on a
/// row already placed, and `names_given_up` pairing no taker by label (`left: ["m2"]`) or by alias
/// (`left: ["m104"]`). With a pair of one row counted, the row taking its own alias waits behind
/// itself: `left: ["m3"] right: []`.
#[test]
fn no_prefix_of_a_plan_shows_a_name_on_two_rows() {
    let stays_one = |book: &mut Book, moved: &Plan| {
        for changes in &moved.rows {
            book.write(1, &[], changes.clone());
            let cells = book.cells();
            assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
        }
    };
    const THIRD: &str = "aabbccddeeff0011";
    for (a, b, c) in [
        (PRINCIPAL, OTHER_PRINCIPAL, THIRD),
        (THIRD, OTHER_PRINCIPAL, PRINCIPAL),
        (OTHER_PRINCIPAL, PRINCIPAL, THIRD),
        (OTHER_PRINCIPAL, THIRD, PRINCIPAL),
    ] {
        let mut book = Book::new();
        let was = [
            roster(1, &[], Some(a)),
            roster(2, &[], Some(b)),
            roster(3, &[], Some(c)),
        ];
        let first = plan(&input(&[], &was), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        let now = [
            roster(2, &[], Some(a)),
            roster(3, &[], Some(b)),
            roster(4, &[], Some(c)),
        ];
        let moved = plan(&input(&[], &now), &cells).unwrap();
        stays_one(&mut book, &moved);
        assert_eq!(
            label_at(&book.cells(), &keys::machine_pk(&format!("n-{a}"))),
            Some(json!("m2"))
        );
    }
    // A row taking its own alias as its label, freeing its label for another, either key first.
    for (x, y) in [(PRINCIPAL, OTHER_PRINCIPAL), (OTHER_PRINCIPAL, PRINCIPAL)] {
        let mut book = Book::new();
        let was = [roster(3, &[4], Some(x)), roster(2, &[], Some(y))];
        let first = plan(&input(&[], &was), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        let now = [roster(4, &[], Some(x)), roster(3, &[], Some(y))];
        let moved = plan(&input(&[], &now), &cells).unwrap();
        stays_one(&mut book, &moved);
        assert_eq!(
            label_at(&book.cells(), &keys::machine_pk(&format!("n-{y}"))),
            Some(json!("m3"))
        );
    }
    // An alias a table gives up and the roster gives another row as its label, whichever key sorts
    // first.
    for giver in ["m5", "m6", "m8", "m9", "m10", "m11"] {
        let mut book = Book::new();
        let was = [
            roster(1, &[], Some(PRINCIPAL)),
            roster(7, &[], Some(OTHER_PRINCIPAL)),
        ];
        let before = [computers_sheet(&[&format!("{giver},,,,,,,m2,,,,")])];
        let first = plan(&input(&before, &was), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        let after = [computers_sheet(&[
            &format!("{giver},,,,,,,,,,,"),
            "m7,,,,,,,m2,,,,",
        ])];
        let now = [
            roster(2, &[], Some(PRINCIPAL)),
            roster(7, &[], Some(OTHER_PRINCIPAL)),
        ];
        let moved = plan(&input(&after, &now), &cells).unwrap();
        stays_one(&mut book, &moved);
        assert!(aliases_of(&book.cells(), &keys::machine_pk(giver)).is_empty());
    }
    // An alias a table moves, to a row whose key sorts on either side of the row giving it up.
    for (giver, taker) in [("m5", "m7"), ("m7", "m5")] {
        let mut book = Book::new();
        let before = [computers_sheet(&[
            &format!("{giver},Apple,,,,,,m104,,,,"),
            &format!("{taker},Apple,,,,,,,,,,"),
        ])];
        let first = plan(&input(&before, &[]), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        let after = [computers_sheet(&[
            &format!("{giver},Apple,,,,,,,,,,"),
            &format!("{taker},Apple,,,,,,m104,,,,"),
        ])];
        let moved = plan(&input(&after, &[]), &cells).unwrap();
        stays_one(&mut book, &moved);
        assert_eq!(
            aliases_of(&book.cells(), &keys::machine_pk(taker)),
            ["m104"]
        );
    }
}

fn dump_of(file: &str, computers: Value) -> Vec<Sheet> {
    let dump = aterm_json::to_vec(&json!({"computers": computers})).unwrap();
    dump_sheets(file, &dump, Mode::FillOnly, &KEY).unwrap()
}

fn goes_by(cells: &Cells, pk: &str, name: &str) -> bool {
    row_names(&cell::project(&MACHINE, cells).rows[pk]).contains(name)
}

/// An alias an edited replica no longer states of its row stays there, in either mode: what the
/// replica's earlier import wrote is only reported, so the row the edit names it of does not take it
/// too, and the name stays on one row.
/// NEGATIVE CONTROL: with `reads_as_written` asking `said` for `wrote`, or `gives_up` not asking it,
/// the name ends on two rows: `left: ["m104"] right: []`.
#[test]
fn an_alias_an_edited_replica_moves_stays_on_one_row() {
    let holder = keys::machine_pk("m5");
    for mode in [Mode::FillOnly, Mode::Authoritative] {
        let mut book = Book::new();
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE, &holder, "label", json!("m5")),
                put(&MACHINE, &holder, "row", json!("live")),
            ],
        );
        let read = replica(
            json!([{"label": "m5", "aliases": ["m104"], "status": "active"}]),
            mode,
        );
        let first = plan(&input(&read, &[]), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
        let mut edited = replica(
            json!([{"label": "m5", "status": "active"},
                {"label": "m7", "aliases": ["m104"], "status": "active"}]),
            mode,
        );
        for sheet in &mut edited {
            sheet.earlier = read.iter().map(|sheet| sheet.source.clone()).collect();
        }
        let moved = plan(&input(&edited, &[]), &cells).unwrap();
        let cells = apply(&mut book, &moved);
        assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
    }
}

/// A row a source reaches by what else it states, a host, a transport id or a mesh id, still
/// answers the name that source calls it, as does a row a known Mac going by the name reaches: it
/// is that row's, another table's claim is a finding, and the name stays on one row.
/// NEGATIVE CONTROL: with `reached_stating` ignoring a source's node or host, or `gives_up`
/// ignoring `reached_stating`, the name ends on two rows: `left: ["m104"] right: []`. With
/// `reached_stating` ignoring a source's mesh id, the name moves off the row: `left: [] right:
/// ["m104"]`. Ignoring a known Mac's principal cell, its transport or its host, the row loses it
/// and the assertion prints its key, `mc-f6ec03807c70e0f8`; ignoring the Mac's own key,
/// `mc-332a629b80e94d25`.
#[test]
fn a_name_a_source_still_gives_a_row_it_reaches_stays_on_it() {
    let holder = keys::machine_pk("m5");
    let other = keys::machine_pk("m7");
    let other_row = |cell: (&str, Value)| {
        vec![
            put(&MACHINE, &other, "label", json!("m7")),
            put(&MACHINE, &other, cell.0, cell.1),
            put(&MACHINE, &other, "row", json!("live")),
        ]
    };
    let moving = |node_id: Value| {
        json!([
            {"label": "m5", "hostname": "m5.local", "status": "active"},
            {"label": "m7", "hostname": "m7.local", "node_id": node_id, "aliases": ["m104"],
                "status": "active"},
        ])
    };
    let transport = (("fleet_node", json!(OTHER_NODE)), json!(OTHER_NODE));
    // A replica that found the row by its host, its transport id or its mesh id wrote the alias, and
    // says so again.
    for (stating, more, (theirs, their_id)) in [
        (
            json!({"label": "m104", "hostname": "Studio", "status": "active"}),
            ("hosts", json!(["Studio"])),
            transport.clone(),
        ),
        (
            json!({"label": "m104", "hostname": "m104.local", "node_id": NODE, "status": "active"}),
            ("fleet_node", json!(NODE)),
            transport.clone(),
        ),
        (
            json!({"label": "m104", "hostname": "m104.local", "node_id": format!("n-{PRINCIPAL}"),
                "status": "active"}),
            ("node", json!(PRINCIPAL)),
            (
                ("node", json!(OTHER_PRINCIPAL)),
                json!(format!("n-{OTHER_PRINCIPAL}")),
            ),
        ),
    ] {
        let mut book = Book::new();
        let mut rows = vec![
            put(&MACHINE, &holder, "label", json!("m5")),
            put(&MACHINE, &holder, more.0, more.1),
            put(&MACHINE, &holder, "row", json!("live")),
        ];
        rows.extend(other_row(theirs));
        book.write(2, &[], rows);
        let stated = dump_of("fleet-db-m200.json", json!([stating]));
        let first = plan(&input(&stated, &[]), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
        let mut sheets = dump_of("fleet-db-m104.json", moving(their_id));
        sheets.extend(stated);
        let moved = plan(&input(&sheets, &[]), &cells).unwrap();
        let cells = apply(&mut book, &moved);
        assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
        assert!(aliases_of(&cells, &other).is_empty());
    }
    // A known Mac going by the name, by its principal, under its own key, by its transport and by
    // its host.
    let keyed = keys::machine_pk(&format!("n-{PRINCIPAL}"));
    let mac = |more: fn(&mut KnownMac)| {
        let mut mac = roster(104, &[], None);
        more(&mut mac);
        mac
    };
    for (mac, pk, cell) in [
        (
            mac(|mac| mac.node = Some(PRINCIPAL.to_owned())),
            &holder,
            Some(("node", json!(PRINCIPAL))),
        ),
        (
            mac(|mac| mac.node = Some(PRINCIPAL.to_owned())),
            &keyed,
            None,
        ),
        (
            mac(|mac| mac.fleet_node = Some(NODE.to_owned())),
            &holder,
            Some(("fleet_node", json!(NODE))),
        ),
        (
            mac(|mac| mac.hosts = vec!["Studio".to_owned()]),
            &holder,
            Some(("hosts", json!(["Studio"]))),
        ),
    ] {
        let mut book = Book::new();
        if *pk == keyed {
            let born = plan(
                &input(&[], &[roster(5, &[], Some(PRINCIPAL))]),
                &book.cells(),
            )
            .unwrap();
            apply(&mut book, &born);
        }
        let before = [computers_sheet(&["m5,Apple,,,,,,m104,,,,"])];
        let first = plan(&input(&before, &[]), &book.cells()).unwrap();
        apply(&mut book, &first);
        let seen = book.all();
        let mut rows = other_row(transport.0.clone());
        rows.extend(cell.map(|(column, x)| put(&MACHINE, pk, column, x)));
        book.write(2, &seen, rows);
        assert_eq!(aliases_of(&book.cells(), pk), ["m104"]);
        let mut sheets = vec![computers_sheet(&["m5,Apple,,,,,,,,,,"])];
        sheets.extend(dump_of("fleet-db-m104.json", moving(transport.1.clone())));
        let moved = plan(&input(&sheets, &[mac]), &book.cells()).unwrap();
        let cells = apply(&mut book, &moved);
        assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
        assert!(goes_by(&cells, pk, "m104"), "{pk}");
        assert!(aliases_of(&cells, &other).is_empty(), "{pk}");
    }
}

/// A row this plan comes to reach by a host or a node a source states beside a name still answers
/// that name, whichever source the run reads first: a table read before that source gives the name
/// to another row, and the plan keeps it on its row from the start, the table's claim a finding.
/// NEGATIVE CONTROL: with `reached_later` finding nothing, or `reached_stating` not reading what the
/// plan wants, the name ends on two rows: `left: ["m104"] right: []`.
#[test]
fn a_name_a_host_or_node_this_plan_writes_reaches_stays_on_its_row() {
    let (holder, other) = (keys::machine_pk("m5"), keys::machine_pk("m7"));
    for (one, two) in [
        (
            json!({"label": "m5", "hostname": "Studio", "status": "active"}),
            json!({"label": "m104", "hostname": "Studio", "status": "active"}),
        ),
        (
            json!({"label": "m5", "node_id": NODE, "status": "active"}),
            json!({"label": "m104", "node_id": NODE, "status": "active"}),
        ),
    ] {
        let mut book = Book::new();
        let before = [computers_sheet(&[
            "m5,Apple,,,,,,m104,,,,",
            "m7,Apple,,,,,,,,,,",
        ])];
        let first = plan(&input(&before, &[]), &book.cells()).unwrap();
        let cells = apply(&mut book, &first);
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
        let mut sheets = vec![computers_sheet(&[
            "m5,Apple,,,,,,,,,,",
            "m7,Apple,,,,,,m104,,,,",
        ])];
        sheets.extend(dump_of("fleet-db-m104.json", json!([one, two])));
        let moved = plan(&input(&sheets, &[]), &cells).unwrap();
        assert!(
            kinds(&moved).contains(&"label-ambiguous"),
            "{:?}",
            moved.findings
        );
        let cells = apply(&mut book, &moved);
        assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
        assert!(aliases_of(&cells, &other).is_empty());
        let again = plan(&input(&sheets, &[]), &cells).unwrap();
        let cells = apply(&mut book, &again);
        assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
        assert_eq!(aliases_of(&cells, &holder), ["m104"]);
    }
}

/// Rows trading names write one of them first, and a row that only takes a name a trading row frees,
/// or takes one from a row it trades nothing with, still waits for it, whichever key sorts first:
/// no prefix shows that name on two rows.
/// NEGATIVE CONTROL: with the fallback taking the lowest key left, the row taking `m103` goes first:
/// `["m103"]`. With it taking the first row in any trade, `m6`'s trade goes before the row freeing
/// `m104`: `["m102", "m104"]`. With only a trade waiting on no row outside it preferred, `first`
/// goes before `middle`: `["m102", "m105"]`.
#[test]
fn a_row_taking_a_name_from_rows_trading_names_waits_for_it() {
    let mut labels = ["m5", "m6", "m7"];
    labels.sort_by_key(|label| keys::machine_pk(label));
    let [taker, a, b] = labels;
    let mut book = Book::new();
    let before = [computers_sheet(&[
        &format!("{a},Apple,,,,,,m101,,,,"),
        &format!("{b},Apple,,,,,,m102 m103,,,,"),
        &format!("{taker},Apple,,,,,,,,,,"),
    ])];
    let first = plan(&input(&before, &[]), &book.cells()).unwrap();
    apply(&mut book, &first);
    let after = [computers_sheet(&[
        &format!("{a},Apple,,,,,,m102,,,,"),
        &format!("{b},Apple,,,,,,m101,,,,"),
        &format!("{taker},Apple,,,,,,m103,,,,"),
    ])];
    let moved = plan(&input(&after, &[]), &book.cells()).unwrap();
    for changes in &moved.rows {
        book.write(1, &[], changes.clone());
        let doubled = names_on_two_rows(&book.cells());
        assert!(!doubled.contains(&"m103".to_owned()), "{doubled:?}");
    }
    let cells = book.cells();
    assert_eq!(aliases_of(&cells, &keys::machine_pk(a)), ["m102"]);
    assert_eq!(aliases_of(&cells, &keys::machine_pk(b)), ["m101"]);
    assert_eq!(aliases_of(&cells, &keys::machine_pk(taker)), ["m103"]);
    assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());

    // Two trades, `m6` with `m8` and `m7` with `m9` (keys sorting `m6`, `m8`, `m7`, `m9`), and `m104`
    // moving from `m7` to `m6`: the trade `m6` is in waits on the other, so that one goes first.
    let mut book = Book::new();
    let before = [computers_sheet(&[
        "m6,Apple,,,,,,m101,,,,",
        "m8,Apple,,,,,,m102,,,,",
        "m7,Apple,,,,,,m103 m104,,,,",
        "m9,Apple,,,,,,m105,,,,",
    ])];
    let first = plan(&input(&before, &[]), &book.cells()).unwrap();
    apply(&mut book, &first);
    let after = [computers_sheet(&[
        "m6,Apple,,,,,,m102 m104,,,,",
        "m8,Apple,,,,,,m101,,,,",
        "m7,Apple,,,,,,m105,,,,",
        "m9,Apple,,,,,,m103,,,,",
    ])];
    let moved = plan(&input(&after, &[]), &book.cells()).unwrap();
    for changes in &moved.rows {
        book.write(1, &[], changes.clone());
        let doubled = names_on_two_rows(&book.cells());
        assert!(!doubled.contains(&"m104".to_owned()), "{doubled:?}");
    }
    let cells = book.cells();
    assert_eq!(
        aliases_of(&cells, &keys::machine_pk("m6")),
        ["m102", "m104"]
    );
    assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());

    // Two trades sharing a row (`first` with `middle`, `middle` with `last`) and `m105` moving from
    // `last` to `first`, which trade nothing: the row trading with each row it takes from goes
    // first, so `m105` never shows on two rows.
    let [first, middle, last] = labels;
    let mut book = Book::new();
    let before = [computers_sheet(&[
        &format!("{first},Apple,,,,,,m101,,,,"),
        &format!("{middle},Apple,,,,,,m102 m103,,,,"),
        &format!("{last},Apple,,,,,,m104 m105,,,,"),
    ])];
    let born = plan(&input(&before, &[]), &book.cells()).unwrap();
    apply(&mut book, &born);
    let after = [computers_sheet(&[
        &format!("{first},Apple,,,,,,m102 m105,,,,"),
        &format!("{middle},Apple,,,,,,m101 m104,,,,"),
        &format!("{last},Apple,,,,,,m103,,,,"),
    ])];
    let moved = plan(&input(&after, &[]), &book.cells()).unwrap();
    for changes in &moved.rows {
        book.write(1, &[], changes.clone());
        let doubled = names_on_two_rows(&book.cells());
        assert!(!doubled.contains(&"m105".to_owned()), "{doubled:?}");
    }
    let cells = book.cells();
    assert_eq!(
        aliases_of(&cells, &keys::machine_pk(first)),
        ["m102", "m105"]
    );
    assert_eq!(aliases_of(&cells, &keys::machine_pk(last)), ["m103"]);
    assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());
}

/// A table that lists a row by any name it goes by disowns the alias it states of another row: a
/// renamed Mac's old name, and either candidate of a conflicted label. The renamed row gives the
/// alias up; a conflicted label settles nothing, so it is a finding whichever candidate shows and
/// the row keeps the name until it settles.
/// NEGATIVE CONTROL: with `disowned` reading only the label shown, the renamed row keeps the
/// name and folds the table's `m7` in: `left: ["m104", "m200", "m7"] right: ["m104"]`. With
/// `gives_up` taking a conflicted label as settled, the table's claim raises nothing: `m5: []`. With
/// the name left out of `names_kept`, or `finish` not reading it, the row loses it: the assertion
/// prints `m5`. With `holds_disowned` reading only the label shown, a candidate's own row is a
/// finding: `m5: [Finding { kind: LabelAmbiguous, ..`; ignoring the label, `m7: [Finding { ..`.
#[test]
fn a_table_disowns_by_any_name_a_row_goes_by() {
    let holder = keys::machine_pk("m36");
    let mut book = Book::new();
    let before = [computers_sheet(&["m36,Apple,,,,,,m104 m200,,,,"])];
    let first = plan(&input(&before, &[]), &book.cells()).unwrap();
    let cells = apply(&mut book, &first);
    let after = [computers_sheet(&[
        "m104,Apple,,,,,,,,,,",
        "m7,Apple,,,,,,m200,,,,",
    ])];
    let moved = plan(&input(&after, &[]), &cells).unwrap();
    let cells = apply(&mut book, &moved);
    assert_eq!(aliases_of(&cells, &holder), ["m104"]);
    assert_eq!(aliases_of(&cells, &keys::machine_pk("m7")), ["m200"]);
    assert_eq!(names_on_two_rows(&cells), Vec::<String>::new());

    // A table stating a name of a row whose label is in dispute beside a candidate of it finds that
    // row, whichever candidate shows.
    let holder = keys::machine_pk("m5");
    for (alone, beside) in [("m5", "m7"), ("m7", "m5")] {
        let mut book = Book::new();
        let before = [computers_sheet(&["m5,Apple,,,,,,m200,,,,"])];
        let first = plan(&input(&before, &[]), &book.cells()).unwrap();
        apply(&mut book, &first);
        book.write(3, &[], vec![put(&MACHINE, &holder, "label", json!("m7"))]);
        let after = [computers_sheet(&[
            &format!("{alone},Apple,,,,,,,,,,"),
            &format!("{beside},Apple,,,,,,m200,,,,"),
        ])];
        let found = plan(&input(&after, &[]), &book.cells()).unwrap();
        assert!(
            !kinds(&found).contains(&"label-ambiguous"),
            "{beside}: {:?}",
            found.findings
        );
        assert_eq!(new_machines(&found), 0, "{beside}: {:?}", found.counts);
    }

    for named in ["m5", "m6"] {
        let mut book = Book::new();
        let before = [computers_sheet(&["m5,Apple,,,,,,m200,,,,"])];
        let first = plan(&input(&before, &[]), &book.cells()).unwrap();
        apply(&mut book, &first);
        book.write(3, &[], vec![put(&MACHINE, &holder, "label", json!("m6"))]);
        let after = [computers_sheet(&[
            &format!("{named},Apple,,,,,,,,,,"),
            "m7,Apple,,,,,,m200,,,,",
        ])];
        let held = plan(&input(&after, &[]), &book.cells()).unwrap();
        assert!(
            kinds(&held).contains(&"label-ambiguous"),
            "{named}: {:?}",
            held.findings
        );
        assert_eq!(new_machines(&held), 0, "{named}: {:?}", held.counts);
        assert_eq!(wanted(&held, &holder, "aliases.m7"), None);
        let cells = apply(&mut book, &held);
        assert!(
            aliases_of(&cells, &holder).contains(&"m200".to_owned()),
            "{named}"
        );
        let again = plan(&input(&after, &[]), &cells).unwrap();
        assert!(again.rows.is_empty(), "{named}: {:?}", again.planned);
    }
}

/// A roster name that a row's conflicted label shows asks for no alias of it; with another candidate
/// showing, the name is wanted as an alias, so it still answers the row.
/// NEGATIVE CONTROL: with `land_label` wanting the name as an alias of a row whose label shows
/// it: `m6 beside Some(String("m6"))` and `left: true right: false`. With the alias branch dropped,
/// the hidden candidate answers nothing: `m5 beside Some(String("m6"))`, `left: false right: true`.
#[test]
fn a_roster_name_a_conflicted_label_shows_wants_no_alias() {
    let row = keys::machine_pk(&format!("n-{PRINCIPAL}"));
    for name in [5, 6] {
        let mut book = Book::new();
        let born = plan(
            &input(&[], &[roster(5, &[], Some(PRINCIPAL))]),
            &book.cells(),
        )
        .unwrap();
        apply(&mut book, &born);
        book.write(3, &[], vec![put(&MACHINE, &row, "label", json!("m6"))]);
        let cells = book.cells();
        let shown = label_at(&cells, &row);
        let label = format!("m{name}");
        let read = plan(&input(&[], &[roster(name, &[], Some(PRINCIPAL))]), &cells).unwrap();
        assert_eq!(
            wanted(&read, &row, &format!("aliases.{label}")).is_some(),
            shown != Some(json!(label)),
            "{label} beside {shown:?}: {:?}",
            read.planned
        );
    }
}

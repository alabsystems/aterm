// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which row a source's machine means, over hand-built registry histories: the replica's
//! `computers` table, the sheets and the roster, each alone and all in one run.

use super::super::{
    Input, Mode, Plan, Sheet, Step, covered, dump_sheets, plan,
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
/// NEGATIVE CONTROL: with `known_label` not wanting the name as an alias (its `label_lands` branch
/// dropped), the import retracts the name the roster now gives the Mac: `left: Some(Bool(false))
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
/// NEGATIVE CONTROL: with `known_label`'s `name_clash` return dropped: `left: Some(String("m120"))
/// right: None`.
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
/// NEGATIVE CONTROL: with `known_label` keeping no alias (`replaced` always `None`), the row loses
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

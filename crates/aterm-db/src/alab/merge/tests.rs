// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The repair's plans over hand-built registry histories, the live shape first: a node-bound row
//! under a Mac's old label holding its new name as an alias, beside the row a roster born under the
//! new name, each with a preference for one account.

use super::*;
use crate::{
    alab::{
        import::{
            self, Input, KnownMac, Mode, Sheet,
            tests::{SEALER, Stand, vault},
        },
        registry::SOURCE,
    },
    machines::Label,
    typed::{
        cell::{self, Cells},
        grid::tests::{Book, elem, put},
    },
};
use aterm_json::json;

const NODE: &str = "00112233445566778899aabbccddeeff";
const OTHER_NODE: &str = "ffeeddccbbaa99887766554433221100";
const KEY: [u8; 32] = [0x5a; 32];
const ANN: &str = "ann@example.invalid";

#[test]
fn a_chosen_number_binds_its_sheet_row_despite_a_stale_label_clash() {
    let own = "mc-3212e085d1b70b59";
    let sheet = "mc-ce20a1499bba52af";
    let other = "mc-f538a6db070c0f9b";
    let node = "a20d9412c90c0be0";
    let other_node = "0011223344556677";
    let roster = BTreeMap::from([
        ("m3".into(), BTreeMap::from([("node", node.into())])),
        ("m87".into(), BTreeMap::from([("node", other_node.into())])),
    ]);
    let mut book = Book::new();
    machine_row(
        &mut book,
        own,
        "m87",
        vec![put(&MACHINE, own, "node", json!(node))],
    );
    machine_row(
        &mut book,
        other,
        "m87",
        vec![put(&MACHINE, other, "node", json!(other_node))],
    );
    machine_row(
        &mut book,
        sheet,
        "m3",
        vec![elem(&MACHINE, sheet, "aliases", "m91")],
    );
    let only_choosing_mac =
        BTreeMap::from([("m3".into(), BTreeMap::from([("node", node.into())]))]);
    let by_node = plan_with(&book.cells(), &only_choosing_mac);
    assert_eq!(
        by_node.merges.len(),
        1,
        "the other real row's m87 need not be its roster number: {by_node:?}"
    );
    let first = plan_with(&book.cells(), &roster);
    assert_eq!(first.merges.len(), 1, "{first:?}");
    assert_eq!(first.merges[0].keep, own);
    assert_eq!(first.merges[0].gone, [sheet]);
    let cells = apply(&mut book, &first);
    assert_eq!(row(&cells, own).get("label"), Some(&json!("m3")));
    assert_eq!(row(&cells, other).get("label"), Some(&json!("m87")));
    assert!(!row(&cells, own).fields["aliases"].has_member("m87"));
    assert!(row(&cells, own).fields["aliases"].has_member("m91"));
    assert!(!row(&cells, sheet).live());
    assert!(plan_with(&cells, &roster).rows.is_empty());

    // A real holder on the target row prevents binding it to a different Mac.
    let mut book = Book::new();
    machine_row(
        &mut book,
        own,
        "m87",
        vec![put(&MACHINE, own, "node", json!(node))],
    );
    machine_row(
        &mut book,
        sheet,
        "m3",
        vec![put(&MACHINE, sheet, "node", json!(other_node))],
    );
    assert!(plan_with(&book.cells(), &roster).merges.is_empty());
}

#[test]
fn a_sheet_number_does_not_bypass_another_rows_node_binding() {
    let own = "mc-3212e085d1b70b59";
    let sheet = "mc-ce20a1499bba52af";
    let other = "mc-f538a6db070c0f9b";
    let mut book = Book::new();
    machine_row(
        &mut book,
        own,
        "m87",
        vec![put(&MACHINE, own, "node", json!("a20d9412c90c0be0"))],
    );
    machine_row(&mut book, sheet, "m3", Vec::new());
    machine_row(
        &mut book,
        other,
        "m3",
        vec![put(&MACHINE, other, "node", json!("0011223344556677"))],
    );
    let roster = BTreeMap::from([
        (
            "m3".into(),
            BTreeMap::from([("node", "a20d9412c90c0be0".into())]),
        ),
        (
            "m87".into(),
            BTreeMap::from([("node", "0011223344556677".into())]),
        ),
    ]);
    let plan = plan_with(&book.cells(), &roster);
    assert!(
        plan.merges.is_empty(),
        "a bound row still holds m3: {plan:?}"
    );
}
/// A hold's `why` and what writes its cause into a book, given the two keys its case names.
type Shape = (&'static str, fn(&mut Book, &str, &str));

fn roster_of(labels: &[&str]) -> Roster {
    labels
        .iter()
        .map(|label| ((*label).to_owned(), BTreeMap::new()))
        .collect()
}

/// A plan with a facts row naming each of `bound` as its Mac's row.
fn run(cells: &Cells, bound: &[&str], roster: &[&str]) -> Plan {
    let facts = bound
        .iter()
        .map(|pk| (Some((*pk).to_owned()), None))
        .collect();
    run_facts(cells, facts, roster)
}

fn run_facts(cells: &Cells, facts: Vec<(Option<String>, Option<String>)>, roster: &[&str]) -> Plan {
    plan(
        &cell::project(&MACHINE, cells),
        &cell::project(&PREFERENCE, cells),
        &Facts::new(facts),
        &roster_of(roster),
    )
    .unwrap()
}

fn apply(book: &mut Book, plan: &Plan) -> Cells {
    apply_rows(book, &plan.rows)
}

fn apply_rows(book: &mut Book, rows: &[Vec<Change>]) -> Cells {
    for changes in rows {
        let seen = book.all();
        book.write(2, &seen, changes.clone());
    }
    book.cells()
}

fn row(cells: &Cells, pk: &str) -> Row {
    cell::project(&MACHINE, cells).rows[pk].clone()
}

fn machine_row(book: &mut Book, pk: &str, label: &str, more: Vec<Change>) {
    let mut changes = vec![
        put(&MACHINE, pk, "label", json!(label)),
        put(&MACHINE, pk, "row", json!("live")),
    ];
    changes.extend(more);
    book.write(2, &[], changes);
}

fn account() -> String {
    keys::account_pk("claude", ANN).unwrap()
}

/// `(keep, duplicate)` of the shape every test starts from: `m36` bound to its node with the alias
/// `m104`, and a roster's row `m104`.
fn two_rows(book: &mut Book) -> (String, String) {
    let keep = keys::machine_pk("m36");
    let gone = keys::machine_pk("m104");
    machine_row(
        book,
        &keep,
        "m36",
        vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
            put(&MACHINE, &keep, "status", json!("active")),
        ],
    );
    machine_row(
        book,
        &gone,
        "m104",
        vec![put(&MACHINE, &gone, "status", json!("active"))],
    );
    (keep, gone)
}

fn preference(book: &mut Book, machine: &str, rank: Option<u32>) -> String {
    let account = account();
    let pk = keys::preference_pk(machine, &account).unwrap();
    let mut changes = vec![
        put(&PREFERENCE, &pk, "machine", json!(machine)),
        put(&PREFERENCE, &pk, "account", json!(account)),
        put(&PREFERENCE, &pk, "why", json!("import")),
        put(&PREFERENCE, &pk, "row", json!("live")),
    ];
    if let Some(rank) = rank {
        changes.push(put(&PREFERENCE, &pk, "rank", json!(rank)));
    }
    book.write(2, &[], changes);
    pk
}

fn sheets_of_the_live_mac(computers: &Value) -> Vec<Sheet> {
    let mut sheets = import::dump_sheets(
        "fleet-db-m104.json",
        &aterm_json::to_vec(&json!({"computers": computers})).unwrap(),
        Mode::Authoritative,
        &KEY,
    )
    .unwrap();
    sheets.push(vault(&[",ann@example.invalid,m104,m36,1,,,,,,,,"]));
    sheets
}

fn import_input<'a>(sheets: &'a [Sheet], macs: &'a [KnownMac]) -> Input<'a> {
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

/// The live shape, built as the importer built it: the roster births `m104` with no node and no
/// alias, the vault sheet's second computer column births `m36` and gives each row a preference
/// for one account, and the replica's import writes `m36`'s node and the alias `m104` under its
/// own source. Answers the sheets, the roster, and `(row that stays, duplicate)`.
fn live_shape(book: &mut Book) -> (Vec<Sheet>, [KnownMac; 1], String, String) {
    let computers = json!([{"label": "m36", "hostname": "mbp.lan", "node_id": NODE,
        "aliases": ["m104"], "make": "Apple", "nickname": "Quiet Otter", "status": "active"}]);
    let macs = [KnownMac {
        label: Some(Label::plain(104)),
        aliases: Vec::new(),
        node: None,
        fleet_node: None,
        hosts: Vec::new(),
    }];
    let born = import::plan(&import_input(&[], &macs), &book.cells()).unwrap();
    let cells = apply_rows(book, &born.rows);
    let vault_alone = [vault(&[",ann@example.invalid,m104,m36,1,,,,,,,,"])];
    let placed = import::plan(&import_input(&vault_alone, &macs), &cells).unwrap();
    apply_rows(book, &placed.rows);
    let sheets = sheets_of_the_live_mac(&computers);
    let source = sheets[0].source.clone();
    let keep = keys::machine_pk("m36");
    let gone = keys::machine_pk("m104");
    let imported = |col: &str, elem: Option<&str>, x: Value| {
        cell::change(
            &MACHINE,
            CellKey {
                pk: &keep,
                col,
                elem,
            },
            &CellValue::imported(MACHINE.version, x, &source),
        )
        .unwrap()
    };
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![
            put(&SOURCE, &source, "kind", json!("fleet-db")),
            imported("aliases", Some("m104"), json!(true)),
            imported("fleet_node", None, json!(NODE)),
            imported("hosts", None, json!(["mbp.lan"])),
            imported("make", None, json!("Apple")),
            imported("nickname", None, json!("Quiet Otter")),
            imported("status", None, json!("active")),
        ],
    );
    (sheets, macs, keep, gone)
}

/// What the live shape must end as, whichever ran first: `m104` is the node-bound row's label, `m36`
/// its alias, the duplicate is `merged` pointing at it, the preference's rank moved, the whole
/// import plans nothing and finds no name on two rows, and the merge has nothing left to do.
fn assert_settled(cells: &Cells, sheets: &[Sheet], macs: &[KnownMac], keep: &str, gone: &str) {
    let kept = row(cells, keep);
    assert_eq!(kept.get("label"), Some(&json!("m104")));
    assert_eq!(kept.fields["aliases"].members(), BTreeSet::from(["m36"]));
    let duplicate = row(cells, gone);
    assert_eq!(duplicate.get("merged_into"), Some(&json!(keep)));
    assert_eq!(duplicate.get(LIFECYCLE), Some(&json!("merged")));
    assert!(!duplicate.live());
    let preferences = cell::project(&PREFERENCE, cells);
    let account = account();
    let on_keep = &preferences.rows[&keys::preference_pk(keep, &account).unwrap()];
    assert_eq!(on_keep.get("rank"), Some(&json!(1)));
    assert!(on_keep.live());
    assert!(!preferences.rows[&keys::preference_pk(gone, &account).unwrap()].live());

    let again = import::plan(&import_input(sheets, macs), cells).unwrap();
    let kinds: Vec<&str> = again.findings.iter().map(|f| f.kind.word()).collect();
    assert!(!kinds.contains(&"label-ambiguous"), "{:?}", again.findings);
    assert!(!kinds.contains(&"differs"), "{:?}", again.findings);
    assert_eq!(again.counts.get(MACHINE.name).map(|c| c.new_rows), Some(0));
    assert!(again.rows.is_empty(), "{:?}", again.planned);

    let settled = run(cells, &[], &["m104"]);
    assert!(settled.rows.is_empty() && settled.merges.is_empty());
    assert!(settled.held.is_empty());
    let (found, _) = crate::alab::checks::lab(cells);
    assert!(
        !found
            .iter()
            .any(|finding| finding.kind == "label-ambiguous"),
        "{found:?}"
    );
}

/// The merge first, the order the live registry takes: its dry run names the two rows, their labels
/// and the columns, never a value, and the preference moves whole.
/// NEGATIVE CONTROL: with the merge keeping the old label (`label` always the node-bound row's), the
/// report says so: `left: Object({"from": String("m36"), "to": String("m36")})`. With
/// `move_preferences` a no-op: `left: Null right: String("filled")`.
#[test]
fn the_live_shape_settles_after_the_merge() {
    let mut book = Book::new();
    let (sheets, macs, keep, gone) = live_shape(&mut book);
    let cells = book.cells();
    let held = cell::project(&MACHINE, &cells);
    assert!(held.rows[&keep].fields["aliases"].has_member("m104"));
    assert_eq!(held.rows[&gone].get("label"), Some(&json!("m104")));
    let merged = run(&cells, &[], &["m104"]);
    let reported = merged.report();
    let report = aterm_json::to_string(&reported).unwrap();
    assert!(!report.contains("Quiet Otter"), "{report}");
    assert!(!report.contains(NODE), "{report}");
    assert_eq!(merged.merges.len(), 1, "{report}");
    assert_eq!(reported["merges"][0]["keep"], json!(keep), "{report}");
    assert_eq!(reported["merges"][0]["gone"], json!([gone]), "{report}");
    assert_eq!(
        reported["merges"][0]["label"],
        json!({"from": "m36", "to": "m104"}),
        "{report}"
    );
    assert_eq!(reported["merges"][0]["aliases_added"], json!(["m36"]));
    assert_eq!(reported["merges"][0]["aliases_removed"], json!(["m104"]));
    assert_eq!(
        reported["merges"][0]["preferences"][0]["how"],
        json!("filled"),
        "{report}"
    );
    assert_eq!(
        reported["merges"][0]["preferences"][0]["columns"],
        json!(["rank"]),
        "{report}"
    );
    assert_eq!(reported["held"], json!([]), "{report}");
    let cells = apply(&mut book, &merged);
    assert_settled(&cells, &sheets, &macs, &keep, &gone);
}

/// An import run before the merge is safe: it births nothing, keeps the alias that shows the two
/// rows are one Mac, and says `label-ambiguous` on every run. The replica's node finds the Mac's row
/// for the name, so the sheet's preference moves there and ends on the duplicate. It settles, and
/// the merge then ends in the same state.
/// NEGATIVE CONTROL: with `alias_element` returning at every clash (no `held` branch), the import
/// retracts the alias: `left: Some(Bool(false)) right: Some(Bool(true))`.
#[test]
fn an_import_before_the_merge_births_nothing_and_settles() {
    let mut book = Book::new();
    let (sheets, macs, keep, gone) = live_shape(&mut book);
    let before = import::plan(&import_input(&sheets, &macs), &book.cells()).unwrap();
    assert_eq!(before.counts.get(MACHINE.name).map(|c| c.new_rows), Some(0));
    let ambiguous = |plan: &import::Plan| {
        plan.findings
            .iter()
            .any(|f| f.kind == import::Kind::LabelAmbiguous)
    };
    assert!(ambiguous(&before));
    let link = before
        .planned
        .iter()
        .find(|p| p.column == "aliases.m104")
        .map(|p| p.x.clone());
    assert_eq!(link, Some(json!(true)), "the link is not retracted");
    let duplicate_preference = keys::preference_pk(&gone, &account()).unwrap();
    for ended in before.planned.iter().filter(|p| p.x == json!("ended")) {
        assert_eq!(ended.pk, duplicate_preference, "{ended:?}");
    }
    let cells = apply_rows(&mut book, &before.rows);
    let repeat = import::plan(&import_input(&sheets, &macs), &cells).unwrap();
    assert!(repeat.rows.is_empty(), "{:?}", repeat.planned);
    assert_eq!(repeat.counts.get(MACHINE.name).map(|c| c.new_rows), Some(0));
    assert!(ambiguous(&repeat));
    let merged = run(&cells, &[], &["m104"]);
    assert_eq!(merged.merges.len(), 1, "{:?}", merged.held);
    let cells = apply(&mut book, &merged);
    assert_settled(&cells, &sheets, &macs, &keep, &gone);
}

/// A cell only the duplicate holds moves with its provenance, a set's new elements with it, and a
/// cell both hold differently stays as the node-bound row has it, listed by column; a `status` that
/// says something moves off `planned`. Nothing of a value reaches the report.
/// NEGATIVE CONTROL: with `carried` writing `s: None`, a later import could never tell the cell
/// from the owner's edit: `left: None right: Some("sr-…")`.
#[test]
fn cells_only_the_duplicate_holds_move_with_their_provenance() {
    let mut book = Book::new();
    let keep = keys::machine_pk("m36");
    let gone = keys::machine_pk("m104");
    machine_row(
        &mut book,
        &keep,
        "m36",
        vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
            put(&MACHINE, &keep, "status", json!("planned")),
            put(&MACHINE, &keep, "chip", json!("M3 Max")),
            elem(&MACHINE, &keep, "clis", "claude"),
        ],
    );
    let from_sheet = |col: &str, elem: Option<&str>, x: Value| {
        cell::change(
            &MACHINE,
            CellKey {
                pk: &gone,
                col,
                elem,
            },
            &CellValue::imported(MACHINE.version, x, "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    machine_row(
        &mut book,
        &gone,
        "m104",
        vec![
            put(&MACHINE, &gone, "status", json!("parked")),
            from_sheet("location", None, json!("Quiet Harbor")),
            from_sheet("chip", None, json!("M1")),
            from_sheet("clis", Some("codex"), json!(true)),
            elem(&MACHINE, &gone, "clis", "claude"),
        ],
    );
    let cells = book.cells();
    let plan = run(&cells, &[], &["m104"]);
    let merge = &plan.merges[0];
    let columns = |list: &[(String, String)]| -> Vec<String> {
        list.iter().map(|(_, column)| column.clone()).collect()
    };
    let mut carried = columns(&merge.carried);
    carried.sort();
    assert_eq!(carried, ["clis.codex", "location", "status"]);
    assert_eq!(columns(&merge.kept), ["chip"]);
    let report = aterm_json::to_string(&plan.report()).unwrap();
    assert!(!report.contains("Quiet Harbor"), "{report}");
    assert!(!report.contains("M3 Max"), "{report}");
    assert!(!report.contains("M1"), "{report}");
    let cells = apply(&mut book, &plan);
    let kept = row(&cells, &keep);
    let Some(Field::Set(clis)) = kept.fields.get("clis") else {
        panic!("clis is a set");
    };
    let Some(Field::Value(codex)) = clis.get("codex") else {
        panic!("clis.codex is one value");
    };
    assert_eq!(codex.value.s.as_deref(), Some("sr-0123456789abcdef"));
    assert_eq!(kept.get("location"), Some(&json!("Quiet Harbor")));
    assert_eq!(kept.get("chip"), Some(&json!("M3 Max")));
    assert_eq!(kept.get("status"), Some(&json!("parked")));
    assert_eq!(
        kept.fields["clis"].members(),
        BTreeSet::from(["claude", "codex"])
    );
    let Some(Field::Value(location)) = kept.fields.get("location") else {
        panic!("location is one value");
    };
    assert_eq!(location.value.s.as_deref(), Some("sr-0123456789abcdef"));
}

/// What the owner said stands through a merge: a cell or an alias the owner cleared on the row that
/// stays is not refilled, an owner's cell on a duplicate moves over an import's, and an element the
/// row removed stays removed. A value this build cannot read stays where it is, listed, on the
/// duplicate or on the row that stays, and the row answers every host its duplicate answered.
/// NEGATIVE CONTROL: with `lacks` answering `true` for any cleared cell, the owner's clear is
/// overwritten: `left: Some(String("Quiet Harbor")) right: Some(Null)`. With `carry_cells` not reading
/// the row that stays' `raw`, the newer build's word is written over: the `kept` assertion prints the
/// merge. With `move_preferences` not reading the target's, its rank is: `left: ["rank"] right: []`.
#[test]
fn the_owners_word_and_what_this_build_cannot_read_stand() {
    let keep = keys::machine_pk("m36");
    let gone = keys::machine_pk("m104");
    let imported = |pk: &str, col: &str, elem: Option<&str>, x: Value| {
        cell::change(
            &MACHINE,
            CellKey { pk, col, elem },
            &CellValue::imported(MACHINE.version, x, "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    let owner_false = |pk: &str, col: &str, elem: &str| {
        cell::change(
            &MACHINE,
            CellKey {
                pk,
                col,
                elem: Some(elem),
            },
            &CellValue::owner(MACHINE.version, json!(false)),
        )
        .unwrap()
    };
    let mut book = Book::new();
    machine_row(
        &mut book,
        &keep,
        "m36",
        vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            owner_false(&keep, "aliases", "m7"),
            owner_false(&keep, "clis", "codex"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
            put(&MACHINE, &keep, "status", json!("active")),
            put(&MACHINE, &keep, "location", Value::Null),
            imported(&keep, "chip", None, json!("M3 Max")),
            put(&MACHINE, &keep, "hosts", json!(["a.lan"])),
        ],
    );
    machine_row(
        &mut book,
        &gone,
        "m104",
        vec![
            elem(&MACHINE, &gone, "aliases", "m7"),
            elem(&MACHINE, &gone, "clis", "codex"),
            put(&MACHINE, &gone, "status", json!("active")),
            imported(&gone, "location", None, json!("Quiet Harbor")),
            put(&MACHINE, &gone, "chip", json!("M1")),
            imported(&gone, "hosts", None, json!(["b.lan", "a.lan.local"])),
        ],
    );
    let plan = run(&book.cells(), &[], &["m104"]);
    let merge = &plan.merges[0];
    assert!(
        merge
            .kept
            .contains(&(gone.clone(), "aliases.m7".to_owned()))
    );
    assert!(merge.carried.contains(&(gone.clone(), "hosts".to_owned())));
    let cells = apply(&mut book, &plan);
    let kept = row(&cells, &keep);
    assert_eq!(kept.get("location"), Some(&Value::Null));
    assert_eq!(kept.get("chip"), Some(&json!("M1")));
    assert!(!kept.fields["aliases"].has_member("m7"));
    assert!(!kept.fields["clis"].has_member("codex"));
    assert_eq!(kept.get("hosts"), Some(&json!(["a.lan", "b.lan"])));
    assert!(run(&cells, &[], &["m104"]).rows.is_empty());

    // A word this build does not know stays on the duplicate; the other rows still merge.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let raw = Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{gone}.role"),
        value: Some(json!({"r": MACHINE.version, "x": "gpu-lab"})),
    };
    let seen = book.all();
    book.write(2, &seen, vec![raw]);
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges[0].keep, keep);
    assert!(
        plan.merges[0]
            .kept
            .contains(&(gone.clone(), "role".to_owned())),
        "{:?}",
        plan.merges[0]
    );
    assert!(
        !plan.merges[0]
            .carried
            .iter()
            .any(|(_, column)| column == "role")
    );

    // A cell no reader here can decode, on the row that stays, is no absent cell: what the duplicate
    // holds does not overwrite it, on the row or on a preference moving onto one.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let target = preference(&mut book, &keep, None);
    let raw = Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{keep}.role"),
        value: Some(json!("not a cell")),
    };
    let raw_rank = Change {
        collection: PREFERENCE.name.to_owned(),
        key: format!("{target}.rank"),
        value: Some(json!("not a cell")),
    };
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![raw, raw_rank, put(&MACHINE, &gone, "role", json!("spare"))],
    );
    preference(&mut book, &gone, Some(1));
    let plan = run(&book.cells(), &[], &["m104"]);
    let merge = &plan.merges[0];
    assert!(
        merge.kept.contains(&(gone.clone(), "role".to_owned())),
        "{merge:?}"
    );
    assert_eq!(merge.preferences[0].columns, [""; 0]);
    assert_eq!(merge.preferences[0].kept, ["rank"]);
}

/// The label a merge writes keeps its provenance, so the roster that wrote it can rename the Mac
/// later; the alias it takes back keeps its source too.
/// NEGATIVE CONTROL: with the label written as an owner cell, a later import reports `owner-kept`
/// for the roster's new name: `left: None right: Some("sr-…")`.
#[test]
fn the_label_keeps_its_provenance() {
    let keep = keys::machine_pk("m36");
    let gone = keys::machine_pk("m104");
    let imported = |pk: &str, col: &str, elem: Option<&str>, x: Value| {
        cell::change(
            &MACHINE,
            CellKey { pk, col, elem },
            &CellValue::imported(MACHINE.version, x, "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            imported(&keep, "label", None, json!("m36")),
            imported(&keep, "aliases", Some("m104"), json!(true)),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
            put(&MACHINE, &keep, "row", json!("live")),
            imported(&gone, "label", None, json!("m104")),
            put(&MACHINE, &gone, "row", json!("live")),
        ],
    );
    let plan = run(&book.cells(), &[], &["m104"]);
    let cells = apply(&mut book, &plan);
    let kept = row(&cells, &keep);
    let Some(Field::Value(label)) = kept.fields.get("label") else {
        panic!("label is one value");
    };
    assert_eq!(label.value.s.as_deref(), Some("sr-0123456789abcdef"));
    let Some(Field::Set(aliases)) = kept.fields.get("aliases") else {
        panic!("aliases is a set");
    };
    let Some(Field::Value(old)) = aliases.get("m104") else {
        panic!("aliases.m104 is one value");
    };
    assert_eq!(old.value.x, json!(false));
    assert_eq!(old.value.s.as_deref(), Some("sr-0123456789abcdef"));
}

/// Rows that share a name transitively fold into the one node-bound row: the first duplicate's
/// value and preference are taken, the second's are listed, and nothing is written twice, a set's
/// element included.
/// NEGATIVE CONTROL: with `taken` always false, the second duplicate's cell is written over the
/// first's and not listed: `` assertion failed: merge.kept.contains(&(two.clone(),
/// "location".to_owned())) ``. With a set's `taken` always false, the element is carried twice:
/// `left: 2 right: 1`.
#[test]
fn three_rows_fold_into_one() {
    let keep = keys::machine_pk("m36");
    let middle = keys::machine_pk("m104");
    let last = keys::machine_pk("m200");
    let mut book = Book::new();
    machine_row(
        &mut book,
        &keep,
        "m36",
        vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
        ],
    );
    // The key order is the order the duplicates are read in.
    let (one, two) = if middle < last {
        (middle.clone(), last.clone())
    } else {
        (last.clone(), middle.clone())
    };
    machine_row(
        &mut book,
        &middle,
        "m104",
        vec![
            elem(&MACHINE, &middle, "aliases", "m200"),
            elem(&MACHINE, &middle, "clis", "codex"),
            put(
                &MACHINE,
                &middle,
                "location",
                json!(if middle == one { "One" } else { "Two" }),
            ),
        ],
    );
    machine_row(
        &mut book,
        &last,
        "m200",
        vec![
            elem(&MACHINE, &last, "clis", "codex"),
            put(
                &MACHINE,
                &last,
                "location",
                json!(if last == one { "One" } else { "Two" }),
            ),
        ],
    );
    let first = preference(&mut book, &one, Some(1));
    let second = preference(&mut book, &two, Some(2));
    let plan = run(&book.cells(), &[], &["m200"]);
    assert_eq!(plan.merges.len(), 1, "{:?}", plan.held);
    let merge = &plan.merges[0];
    assert_eq!(merge.gone.len(), 2);
    assert_eq!(merge.label_to, "m200");
    assert_eq!(merge.preferences[0].how, How::Moved);
    assert_eq!(merge.preferences[1].how, How::Filled);
    assert!(merge.kept.contains(&(two.clone(), "location".to_owned())));
    let codex = merge.carried.iter().filter(|(_, c)| c == "clis.codex");
    assert_eq!(codex.count(), 1, "{:?}", merge.carried);
    let cells = apply(&mut book, &plan);
    let kept = row(&cells, &keep);
    assert_eq!(kept.get("location"), Some(&json!("One")));
    assert_eq!(kept.get("label"), Some(&json!("m200")));
    for name in ["m36", "m104"] {
        assert!(kept.fields["aliases"].has_member(name), "{name}");
    }
    let table = cell::project(&PREFERENCE, &cells);
    let target = &table.rows[&keys::preference_pk(&keep, &account()).unwrap()];
    assert_eq!(target.get("rank"), Some(&json!(1)));
    for from in [&first, &second] {
        assert!(!table.rows[from].live());
    }
    assert_eq!(row(&cells, &one).get("merged_into"), Some(&json!(keep)));
    assert_eq!(row(&cells, &two).get("merged_into"), Some(&json!(keep)));
    assert!(run(&cells, &[], &["m200"]).rows.is_empty());
}

/// A duplicate's live preference moves: to a new row when the row that stays has none, into the row
/// that stays' when it has one, taking only what it lacks. One the owner ended there stays ended;
/// one an import ended is live again, as the duplicate's was. The duplicate's ends either way. A
/// preference that had already ended stays where it is. The owner's word on a duplicate's
/// preference moves over an import's, and what stays against it is listed.
/// NEGATIVE CONTROL: with `how` answering `Revived` for any non-live row, the owner's end is undone:
/// `left: Revived right: OwnerEnded`. With an owner-ended target still taking columns, it gains the
/// duplicate's rank: `left: ["rank"] right: []`.
#[test]
fn preferences_move_and_an_owners_end_stands() {
    // Moved: only the duplicate prefers the account.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let from = preference(&mut book, &gone, Some(1));
    let plan = run(&book.cells(), &[], &["m104"]);
    let moved = &plan.merges[0].preferences[0];
    assert_eq!(moved.how, How::Moved);
    assert_eq!(moved.from, from);
    let cells = apply(&mut book, &plan);
    let table = cell::project(&PREFERENCE, &cells);
    let on_keep = &table.rows[&keys::preference_pk(&keep, &account()).unwrap()];
    assert!(on_keep.live());
    assert_eq!(on_keep.get("machine"), Some(&json!(keep)));
    assert_eq!(on_keep.get("account"), Some(&json!(account())));
    assert_eq!(on_keep.get("rank"), Some(&json!(1)));
    assert_eq!(table.rows[&from].get(LIFECYCLE), Some(&json!("ended")));

    // A preference an import wrote stays an import's where it moved.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let pk = keys::preference_pk(&gone, &account()).unwrap();
    let imported = |col: &str, x: Value| {
        cell::change(
            &PREFERENCE,
            CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &CellValue::imported(PREFERENCE.version, x, "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    book.write(
        2,
        &[],
        vec![
            imported("machine", json!(gone)),
            imported("account", json!(account())),
            imported("row", json!("live")),
            imported("rank", json!(3)),
        ],
    );
    let plan = run(&book.cells(), &[], &["m104"]);
    let cells = apply(&mut book, &plan);
    let table = cell::project(&PREFERENCE, &cells);
    let on_keep = &table.rows[&keys::preference_pk(&keep, &account()).unwrap()];
    for column in ["machine", "account", "row", "rank"] {
        let Some(Field::Value(cell)) = on_keep.fields.get(column) else {
            panic!("{column} is one value");
        };
        assert_eq!(
            cell.value.s.as_deref(),
            Some("sr-0123456789abcdef"),
            "{column}"
        );
    }

    // The owner's rank on the duplicate beats an import's on the row that stays; an owner's word on
    // both stays as the row that stays has it, listed by column.
    for (ours, moves) in [(true, true), (false, false)] {
        let mut book = Book::new();
        let (keep, gone) = two_rows(&mut book);
        let target = preference(&mut book, &keep, None);
        let rank = if ours {
            cell::change(
                &PREFERENCE,
                CellKey {
                    pk: &target,
                    col: "rank",
                    elem: None,
                },
                &CellValue::imported(PREFERENCE.version, json!(5), "sr-0123456789abcdef"),
            )
            .unwrap()
        } else {
            put(&PREFERENCE, &target, "rank", json!(5))
        };
        let seen = book.all();
        book.write(2, &seen, vec![rank]);
        preference(&mut book, &gone, Some(1));
        let plan = run(&book.cells(), &[], &["m104"]);
        let moved = &plan.merges[0].preferences[0];
        assert_eq!(moved.columns.contains(&"rank".to_owned()), moves);
        assert_eq!(moved.kept.contains(&"rank".to_owned()), !moves);
        let cells = apply(&mut book, &plan);
        let table = cell::project(&PREFERENCE, &cells);
        let rank = if moves { 1 } else { 5 };
        assert_eq!(table.rows[&target].get("rank"), Some(&json!(rank)));
    }

    // Filled: the row that stays has one with no rank, and keeps its own `why`.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let target = preference(&mut book, &keep, None);
    preference(&mut book, &gone, Some(1));
    let plan = run(&book.cells(), &[], &["m104"]);
    let moved = &plan.merges[0].preferences[0];
    assert_eq!(moved.how, How::Filled);
    assert_eq!(moved.columns, ["rank"]);
    let cells = apply(&mut book, &plan);
    let table = cell::project(&PREFERENCE, &cells);
    assert_eq!(table.rows[&target].get("rank"), Some(&json!(1)));

    // The owner ended the row that stays' preference: it stays ended.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let target = preference(&mut book, &keep, None);
    book.write(
        2,
        &book.all(),
        vec![put(&PREFERENCE, &target, "row", json!("ended"))],
    );
    let from = preference(&mut book, &gone, Some(1));
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges[0].preferences[0].how, How::OwnerEnded);
    assert_eq!(plan.merges[0].preferences[0].columns, [""; 0]);
    let seen = book.all();
    for changes in &plan.rows {
        book.write(2, &seen, changes.clone());
    }
    let table = cell::project(&PREFERENCE, &book.cells());
    assert!(!table.rows[&target].live());
    assert!(!table.rows[&from].live());

    // An import ended it: it is live again.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let target = preference(&mut book, &keep, None);
    let ended = cell::change(
        &PREFERENCE,
        CellKey {
            pk: &target,
            col: "row",
            elem: None,
        },
        &CellValue::imported(PREFERENCE.version, json!("ended"), "sr-0123456789abcdef"),
    )
    .unwrap();
    book.write(2, &book.all(), vec![ended]);
    preference(&mut book, &gone, Some(1));
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges[0].preferences[0].how, How::Revived);
    let seen = book.all();
    for changes in &plan.rows {
        book.write(2, &seen, changes.clone());
    }
    let table = cell::project(&PREFERENCE, &book.cells());
    assert!(table.rows[&target].live());
    let from = keys::preference_pk(&gone, &account()).unwrap();
    assert!(!table.rows[&from].live());
}

/// What the merge cannot decide is held and listed, every row untouched: two nodes are two Macs,
/// and so is a node the roster states of a name against the one the row holds; no node gives no row
/// to keep; one node on two rows is the owner's call; a duplicate a Mac's own facts row is bound to
/// moves only by that Mac; two words of `status` are not plainly one Mac; a conflicted cell, a set's
/// element or a moving preference's is settled first; a cell it decides by that this build cannot
/// read is a newer build's, and nothing unreadable reaches the report; and two of the roster's names
/// on one Mac leave its current name open. A conflict elsewhere holds nothing here.
/// NEGATIVE CONTROL: with `choose` not reading `bound`, a row a Mac is bound to is merged under it,
/// and the `plan.merges.is_empty()` assertion prints that merge. With the disputed target's hold
/// dropped, an ended preference with a conflicted rank is moved onto: the same assertion prints it.
/// With the roster's other labels not read, a row the roster names otherwise is relabelled: it prints
/// that too. With `names_of` not reading `admissible`, the address reaches the report, which the
/// `contains` assertion prints.
#[test]
fn what_it_cannot_decide_is_held() {
    let held_for = |book: &Book, bound: &[&str], roster: &[&str]| -> Vec<&'static str> {
        let plan = run(&book.cells(), bound, roster);
        assert!(
            plan.rows.is_empty() && plan.merges.is_empty(),
            "{:?}",
            plan.merges
        );
        plan.held.iter().map(|held| held.why).collect()
    };
    let (keep, gone) = (keys::machine_pk("m36"), keys::machine_pk("m104"));

    let mut book = Book::new();
    machine_row(
        &mut book,
        &keep,
        "m36",
        vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
        ],
    );
    machine_row(
        &mut book,
        &gone,
        "m104",
        vec![put(&MACHINE, &gone, "fleet_node", json!(OTHER_NODE))],
    );
    assert_eq!(held_for(&book, &[], &["m104"]), ["two-macs"]);

    let mut book = Book::new();
    machine_row(
        &mut book,
        &keep,
        "m36",
        vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
        ],
    );
    machine_row(
        &mut book,
        &gone,
        "m104",
        vec![put(&MACHINE, &gone, "fleet_node", json!(NODE))],
    );
    assert_eq!(held_for(&book, &[], &["m104"]), ["one-node-twice"]);

    // A row the Mac published its own node on is as bound as a replica's, and nothing says the
    // principal and the transport id are one Mac's.
    let mut book = Book::new();
    machine_row(
        &mut book,
        &keep,
        "m36",
        vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
        ],
    );
    machine_row(
        &mut book,
        &gone,
        "m104",
        vec![put(&MACHINE, &gone, "node", json!("0011223344556677"))],
    );
    assert_eq!(held_for(&book, &[], &["m104"]), ["two-macs"]);

    let mut book = Book::new();
    machine_row(
        &mut book,
        &keep,
        "m36",
        vec![elem(&MACHINE, &keep, "aliases", "m104")],
    );
    machine_row(&mut book, &gone, "m104", Vec::new());
    assert_eq!(held_for(&book, &[], &["m104"]), ["no-node"]);

    let mut book = Book::new();
    two_rows(&mut book);
    assert_eq!(held_for(&book, &[&gone], &["m104"]), ["bound-by-its-mac"]);
    assert_eq!(held_for(&book, &[], &["m104", "m36"]), ["two-roster-names"]);

    let mut book = Book::new();
    two_rows(&mut book);
    // Two writers set one cell with neither seeing the other.
    book.write(1, &[], vec![put(&MACHINE, &gone, "nickname", json!("one"))]);
    book.write(3, &[], vec![put(&MACHINE, &gone, "nickname", json!("two"))]);
    assert_eq!(held_for(&book, &[], &["m104"]), ["conflicted"]);

    // The row a Mac's facts name is the one that stays: that holds nothing.
    let mut book = Book::new();
    two_rows(&mut book);
    assert_eq!(run(&book.cells(), &[&keep], &["m104"]).merges.len(), 1);

    let mut book = Book::new();
    two_rows(&mut book);
    let stated: Roster = [(
        "m104".to_owned(),
        BTreeMap::from([("fleet_node", OTHER_NODE.to_owned())]),
    )]
    .into();
    let plan = plan_with(&book.cells(), &stated);
    assert!(plan.merges.is_empty(), "{:?}", plan.merges);
    assert_eq!(plan.held[0].why, "two-macs");
    // The report says why, by rows and names.
    let report = plan.report();
    assert_eq!(report["held"][0]["why"], json!("two-macs"), "{report}");
    assert_eq!(
        report["held"][0]["names"],
        json!(["m104", "m36"]),
        "{report}"
    );
    let stated: Roster = [(
        "m104".to_owned(),
        BTreeMap::from([("fleet_node", NODE.to_owned())]),
    )]
    .into();
    assert_eq!(plan_with(&book.cells(), &stated).merges.len(), 1);
    // The roster states the row's node of another name it does not go by.
    let stated: Roster = [
        ("m104".to_owned(), BTreeMap::new()),
        (
            "m55".to_owned(),
            BTreeMap::from([("fleet_node", NODE.to_owned())]),
        ),
    ]
    .into();
    let plan = plan_with(&book.cells(), &stated);
    assert!(plan.merges.is_empty(), "{:?}", plan.merges);
    assert!(
        plan.held.is_empty(),
        "the signed node now names m55, so the rows share no name"
    );

    // A node, a status or a label this build cannot read holds the rows, and what is not a name is
    // not reported as one.
    for (pk, column, x) in [
        (&gone, "node", "not-a-principal"),
        (&keep, "status", "orbiting"),
        (&keep, "label", "ann@example.invalid"),
    ] {
        let mut book = Book::new();
        two_rows(&mut book);
        let raw = Change {
            collection: MACHINE.name.to_owned(),
            key: format!("{pk}.{column}"),
            value: Some(json!({"r": MACHINE.version, "x": x})),
        };
        let seen = book.all();
        book.write(2, &seen, vec![raw]);
        let plan = run(&book.cells(), &[], &["m104"]);
        assert!(plan.merges.is_empty(), "{column} {:?}", plan.merges);
        assert_eq!(plan.held[0].why, "unreadable", "{column}");
        let report = aterm_json::to_string(&plan.report()).unwrap();
        assert!(!report.contains(x), "{report}");
    }

    // Nor is a node no reader here can decode an absent one.
    let mut book = Book::new();
    let (_, gone) = two_rows(&mut book);
    let raw = Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{gone}.node"),
        value: Some(json!("not a cell")),
    };
    let seen = book.all();
    book.write(2, &seen, vec![raw]);
    let plan = run(&book.cells(), &[], &["m104"]);
    assert!(plan.merges.is_empty(), "{:?}", plan.merges);
    assert_eq!(plan.held[0].why, "unreadable");

    // A preference on the rows that says which it is or whether it is live only in a newer build's
    // words holds them too.
    for column in ["account", "row"] {
        let mut book = Book::new();
        let (_, gone) = two_rows(&mut book);
        let on_gone = preference(&mut book, &gone, Some(1));
        let raw = Change {
            collection: PREFERENCE.name.to_owned(),
            key: format!("{on_gone}.{column}"),
            value: Some(json!({"r": PREFERENCE.version, "x": "not-a-word-this-build-reads"})),
        };
        let seen = book.all();
        book.write(2, &seen, vec![raw]);
        let plan = run(&book.cells(), &[], &["m104"]);
        assert!(plan.merges.is_empty(), "{column} {:?}", plan.merges);
        assert_eq!(plan.held[0].why, "unreadable", "{column}");
    }

    let mut book = Book::new();
    let (keep, _) = two_rows(&mut book);
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&MACHINE, &keep, "status", json!("lost"))],
    );
    assert_eq!(held_for(&book, &[], &["m104"]), ["status-differs"]);

    // A set's element in dispute.
    let mut book = Book::new();
    let (_, gone) = two_rows(&mut book);
    book.write(1, &[], vec![elem(&MACHINE, &gone, "clis", "codex")]);
    let removed = cell::change(
        &MACHINE,
        CellKey {
            pk: &gone,
            col: "clis",
            elem: Some("codex"),
        },
        &CellValue::owner(MACHINE.version, json!(false)),
    )
    .unwrap();
    book.write(3, &[], vec![removed]);
    assert_eq!(held_for(&book, &[], &["m104"]), ["conflicted"]);

    // A moving preference's cell, or its end, in dispute; or the preference it would move onto.
    for column in ["rank", "row"] {
        let values = if column == "rank" {
            [json!(1), json!(2)]
        } else {
            [json!("live"), json!("ended")]
        };
        for on_keep in [false, true] {
            let mut book = Book::new();
            let (keep, gone) = two_rows(&mut book);
            preference(&mut book, &gone, Some(9));
            let disputed = preference(&mut book, if on_keep { &keep } else { &gone }, None);
            let [one, two] = values.clone();
            book.write(1, &[], vec![put(&PREFERENCE, &disputed, column, one)]);
            book.write(3, &[], vec![put(&PREFERENCE, &disputed, column, two)]);
            assert_eq!(
                held_for(&book, &[], &["m104"]),
                ["conflicted"],
                "{column} {on_keep}"
            );
        }
    }

    // An ended preference a duplicate's would move onto, with a cell in dispute.
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    preference(&mut book, &gone, Some(9));
    let ended = preference(&mut book, &keep, None);
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&PREFERENCE, &ended, "row", json!("ended"))],
    );
    book.write(1, &[], vec![put(&PREFERENCE, &ended, "rank", json!(1))]);
    book.write(3, &[], vec![put(&PREFERENCE, &ended, "rank", json!(2))]);
    assert_eq!(held_for(&book, &[], &["m104"]), ["conflicted"]);

    // A preference in dispute on another machine holds nothing here.
    let mut book = Book::new();
    two_rows(&mut book);
    let elsewhere = keys::machine_pk("m7");
    machine_row(&mut book, &elsewhere, "m7", Vec::new());
    let disputed = preference(&mut book, &elsewhere, None);
    book.write(1, &[], vec![put(&PREFERENCE, &disputed, "rank", json!(1))]);
    book.write(3, &[], vec![put(&PREFERENCE, &disputed, "rank", json!(2))]);
    assert_eq!(run(&book.cells(), &[], &["m104"]).merges.len(), 1);
}

fn plan_with(cells: &Cells, roster: &Roster) -> Plan {
    plan(
        &cell::project(&MACHINE, cells),
        &cell::project(&PREFERENCE, cells),
        &Facts::default(),
        roster,
    )
    .unwrap()
}

/// `register::pick` adopts by name only from `planned`, so a merge keeps that way in for a Mac that
/// has yet to bind: the row that stays takes `planned` over an import's `active`, keeping that
/// import's provenance, and never takes `active` off `planned`. Where another word stands, or a Mac
/// is already bound to the row, there is nothing to keep open or the rows are held.
/// NEGATIVE CONTROL: with `status_moves` taking `active` off `planned`, the `kept` assertion prints a
/// merge that carried `status`. With `planned` never moving onto a row that waits, it can no longer
/// be adopted: `left: Some(String("active")) right: Some(String("planned"))`. With that word keeping
/// no provenance, it is an owner's cell: `left: None right: Some("sr-…")`. With `choose` not reading
/// `can_wait`, an owner's `active` is overruled, and the hold's assertion prints the merge. With the
/// hold ignoring whether a Mac is bound, a row already in is held: `left: 0 right: 1`.
#[test]
fn a_row_a_mac_could_adopt_stays_adoptable() {
    let imported = |pk: &str, x: &str| {
        cell::change(
            &MACHINE,
            CellKey {
                pk,
                col: "status",
                elem: None,
            },
            &CellValue::imported(MACHINE.version, json!(x), "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    let shape = |book: &mut Book, keep_status: Change, gone_status: &str, more: Vec<Change>| {
        let keep = keys::machine_pk("m36");
        let gone = keys::machine_pk("m104");
        let mut changes = vec![
            elem(&MACHINE, &keep, "aliases", "m104"),
            put(&MACHINE, &keep, "fleet_node", json!(NODE)),
            keep_status,
        ];
        changes.extend(more);
        machine_row(book, &keep, "m36", changes);
        machine_row(
            book,
            &gone,
            "m104",
            vec![put(&MACHINE, &gone, "status", json!(gone_status))],
        );
        (keep, gone)
    };
    let keep = keys::machine_pk("m36");

    // An import's `active` beside a duplicate waiting by name: the row waits, and it settles.
    let mut book = Book::new();
    shape(&mut book, imported(&keep, "active"), "planned", Vec::new());
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges.len(), 1, "{:?}", plan.held);
    let cells = apply(&mut book, &plan);
    let kept = row(&cells, &keep);
    assert_eq!(kept.get("status"), Some(&json!("planned")));
    let Some(Field::Value(status)) = kept.fields.get("status") else {
        panic!("status is one value");
    };
    assert_eq!(status.value.s.as_deref(), Some("sr-0123456789abcdef"));
    assert!(run(&cells, &[], &["m104"]).rows.is_empty());

    // `active` on a duplicate says nothing of a row that is `planned`.
    let mut book = Book::new();
    let (_, gone) = shape(&mut book, imported(&keep, "planned"), "active", Vec::new());
    let plan = run(&book.cells(), &[], &["m104"]);
    assert!(
        plan.merges[0]
            .kept
            .contains(&(gone.clone(), "status".to_owned())),
        "{:?}",
        plan.merges[0]
    );
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("planned")));

    // A Mac already bound to the row needs no way in.
    let mut book = Book::new();
    let principal = put(&MACHINE, &keep, "node", json!("0011223344556677"));
    shape(
        &mut book,
        imported(&keep, "active"),
        "planned",
        vec![principal],
    );
    let plan = run(&book.cells(), &[], &["m104"]);
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("active")));
    let mut book = Book::new();
    shape(&mut book, imported(&keep, "active"), "planned", Vec::new());
    let plan = run(&book.cells(), &[&keep], &["m104"]);
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("active")));

    // Nor does the word it holds stop the merge, whoever wrote it: nothing waits.
    let mut book = Book::new();
    let principal = put(&MACHINE, &keep, "node", json!("0011223344556677"));
    let status = put(&MACHINE, &keep, "status", json!("active"));
    shape(&mut book, status, "planned", vec![principal]);
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges.len(), 1, "{:?}", plan.held);

    // The owner's word, or any other word, is not this merge's to overrule.
    for (word, owner) in [("active", true), ("parked", false), ("parked", true)] {
        let mut book = Book::new();
        let status = if owner {
            put(&MACHINE, &keep, "status", json!(word))
        } else {
            imported(&keep, word)
        };
        shape(&mut book, status, "planned", Vec::new());
        let plan = run(&book.cells(), &[], &["m104"]);
        assert!(plan.merges.is_empty(), "{word} {:?}", plan.merges);
        assert_eq!(plan.held[0].why, "status-differs", "{word}");
    }
}

/// With no roster name among a Mac's names, the node-bound row keeps its own label and every other
/// name stays an alias: nothing is forgotten and the owner can `relabel` it later.
/// NEGATIVE CONTROL: with `choose` taking the first name, the label moves to a name nobody chose:
/// `left: "m104" right: "m36"`.
#[test]
fn with_no_roster_name_the_node_bound_row_keeps_its_label() {
    let mut book = Book::new();
    let (keep, _) = two_rows(&mut book);
    let plan = run(&book.cells(), &[], &[]);
    assert_eq!(plan.merges[0].label_to, "m36");
    let cells = apply(&mut book, &plan);
    let kept = row(&cells, &keep);
    assert_eq!(kept.get("label"), Some(&json!("m36")));
    assert_eq!(kept.fields["aliases"].members(), BTreeSet::from(["m104"]));
}

/// The authenticated node-to-name binding joins every nodeless sheet row answering that name.
/// The node-bound row stays and keeps its earlier nonclashing number as an alias; another node
/// on the target row or two rows carrying the node prevents the merge. Without applying the
/// node-to-name binding before grouping, the first merge is absent.
#[test]
fn a_mac_the_roster_names_otherwise_folds_its_two_rows() {
    let principal = "0011223344556677";
    let roster: Roster = BTreeMap::from([(
        "m164".to_owned(),
        BTreeMap::from([("node", principal.to_owned())]),
    )]);
    let shape = |book: &mut Book| {
        let (own, named) = (keys::machine_pk("m91"), keys::machine_pk("m164"));
        machine_row(
            book,
            &own,
            "m91",
            vec![
                put(&MACHINE, &own, "node", json!(principal)),
                put(&MACHINE, &own, "status", json!("active")),
            ],
        );
        machine_row(
            book,
            &named,
            "m164",
            vec![put(&MACHINE, &named, "status", json!("planned"))],
        );
        (own, named)
    };
    let mut book = Book::new();
    let (own, named) = shape(&mut book);
    let first = plan_with(&book.cells(), &roster);
    assert_eq!(first.merges.len(), 1, "{first:?}");
    assert_eq!(first.merges[0].keep, own);
    assert_eq!(first.merges[0].gone, std::slice::from_ref(&named));
    assert_eq!(first.merges[0].label_to, "m164");
    let cells = apply(&mut book, &first);
    let kept = row(&cells, &own);
    assert_eq!(kept.get("label"), Some(&json!("m164")));
    assert_eq!(kept.get("status"), Some(&json!("active")));
    assert_eq!(kept.fields["aliases"].members(), BTreeSet::from(["m91"]));
    assert!(!row(&cells, &named).live());
    let again = plan_with(&cells, &roster);
    assert!(again.rows.is_empty(), "{again:?}");

    let mut book = Book::new();
    let (_, named) = shape(&mut book);
    book.write(
        2,
        &[],
        vec![put(&MACHINE, &named, "fleet_node", json!(OTHER_NODE))],
    );
    let apart = plan_with(&book.cells(), &roster);
    assert!(
        apart.merges.is_empty() && apart.held.iter().all(|held| held.why == "two-macs"),
        "{apart:?}"
    );
    let mut book = Book::new();
    let (own, _) = shape(&mut book);
    let third = keys::machine_pk("m7");
    machine_row(
        &mut book,
        &third,
        "m7",
        vec![elem(&MACHINE, &third, "aliases", "m164")],
    );
    let two = plan_with(&book.cells(), &roster);
    assert_eq!(two.merges.len(), 1, "{two:?}");
    assert_eq!(two.merges[0].keep, own);
    assert_eq!(two.merges[0].gone.len(), 2);
    let mut book = Book::new();
    shape(&mut book);
    let twin = keys::machine_pk("m8");
    machine_row(
        &mut book,
        &twin,
        "m8",
        vec![put(&MACHINE, &twin, "node", json!(principal))],
    );
    let twice = plan_with(&book.cells(), &roster);
    assert!(twice.merges.is_empty(), "{twice:?}");
}

/// A run cut short after any record finishes on the next: each preference moves whole with its end,
/// a carried cell moves before the row it came from ends, and the names change with the duplicate's
/// end, so a reader never sees a name on none of the rows, and the second run writes only what the
/// first did not.
/// NEGATIVE CONTROL: with the machine entry written first (`plan.rows.insert(0, last)`), a run cut
/// there strands what had yet to move on a `merged` row the next plan no longer reads:
/// `left: None right: Some(String("Quiet Harbor"))`.
#[test]
fn a_run_cut_short_finishes_on_the_next() {
    let shape = |book: &mut Book| {
        let (keep, gone) = two_rows(book);
        let seen = book.all();
        book.write(
            2,
            &seen,
            vec![put(&MACHINE, &gone, "location", json!("Quiet Harbor"))],
        );
        preference(book, &gone, Some(1));
        (keep, gone)
    };
    let mut book = Book::new();
    shape(&mut book);
    let whole = run(&book.cells(), &[], &["m104"]);
    assert!(whole.rows.len() >= 3, "{:?}", whole.rows);
    for cut in 1..=whole.rows.len() {
        let mut book = Book::new();
        let (keep, gone) = shape(&mut book);
        let cells = apply_rows(&mut book, &whole.rows[..cut]);
        // At every cut each name still answers, and the duplicate is either whole or gone.
        let table = cell::project(&MACHINE, &cells);
        for name in ["m36", "m104"] {
            assert!(
                table
                    .rows
                    .values()
                    .any(|row| row.live() && names_of(row).contains(name)),
                "cut {cut}: {name}"
            );
        }
        let plan = run(&cells, &[], &["m104"]);
        let cells = apply(&mut book, &plan);
        let kept = row(&cells, &keep);
        assert_eq!(kept.get("label"), Some(&json!("m104")), "cut {cut}");
        assert_eq!(
            kept.get("location"),
            Some(&json!("Quiet Harbor")),
            "cut {cut}"
        );
        assert!(!row(&cells, &gone).live(), "cut {cut}");
        let preferences = cell::project(&PREFERENCE, &cells);
        let on_keep = &preferences.rows[&keys::preference_pk(&keep, &account()).unwrap()];
        assert!(on_keep.live(), "cut {cut}");
        assert_eq!(on_keep.get("rank"), Some(&json!(1)), "cut {cut}");
        assert!(run(&cells, &[], &["m104"]).rows.is_empty(), "cut {cut}");
    }
}

/// A row an import wrote, with a word of `status`.
fn imported_row(pk: &str, label: &str, status: &str, more: Vec<Change>) -> Vec<Change> {
    let cell = |col: &str, x: Value| {
        cell::change(
            &MACHINE,
            CellKey {
                pk,
                col,
                elem: None,
            },
            &CellValue::imported(MACHINE.version, x, "sr-0123456789abcdef"),
        )
        .unwrap()
    };
    let mut changes = vec![
        cell("label", json!(label)),
        cell("row", json!("live")),
        cell("status", json!(status)),
    ];
    changes.extend(more);
    changes
}

/// A live preference a Mac wrote on a row since merged moves along its `merged_into`, also along a
/// chain of them and onto the row a merge in this very plan keeps, and a plan over the result is
/// empty. One that reaches no live row, or that a merge would hold for, is held and left.
/// NEGATIVE CONTROL: with no `move_strays`: `left: 0 right: 1` at `strays.len()`. With `merge_target`
/// ignoring this plan's merges, or a stray listed apart from the merge that keeps its row, the move
/// lands in `strays`: `Plan { .. strays: [Moved { from: ..`. With one hop of `merged_into`:
/// `left: 0 right: 1` at `moved.len()`. With a merged row holding no live preference counted:
/// `left: 2 right: 1` at `held.len()`. With any row counted as merged, a plan over the result writes:
/// `assertion failed: run(&cells, &[], &["m104"]).rows.is_empty()`. With each hold off (a preference or
/// the one it would move onto that this build cannot read or that is in dispute, a `merged_into` this
/// build cannot read, a row a Mac's facts name, a chain through a row not merged), the move goes on:
/// `Plan { merges: [], held: [], strays: [Moved { ..`. With a disputed `merged_into` followed, it
/// reads as no target: `left: "no-merge-target" right: "conflicted"`. With only the stray's own row
/// held to `bound`, a chain through a bound merged row moves on: `Plan { merges: [], held: [],
/// strays: [Moved { ..`. With a bound live row at its end held too: `Plan { merges: [], held:
/// [Held { rows: ["mc-2042463f4ef5fb03"], names: [], why: "bound-by-its-mac" }], ..`. With no Mac's
/// facts read as rebound, a Mac gone along the merge still holds it: that same `held: [Held { ..`.
/// With every Mac read as rebound, or only the first row the chain passes checked, a chain through
/// a bound row moves on: `Plan { merges: [], held: [], strays: [Moved { ..`; with a Mac that
/// rebound anywhere read as gone along the merge: `left: 0 right: 1` at `held.len()`.
#[test]
fn a_preference_on_a_merged_row_moves_along_its_merge() {
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let merged = run(&book.cells(), &[], &["m104"]);
    apply(&mut book, &merged);
    let stray = preference(&mut book, &gone, Some(3));
    let target = keys::preference_pk(&keep, &account()).unwrap();
    let plan = run(&book.cells(), &[], &["m104"]);
    assert!(plan.merges.is_empty(), "{:?}", plan.merges);
    assert!(plan.held.is_empty(), "{:?}", plan.held);
    assert_eq!(plan.strays.len(), 1, "{plan:?}");
    assert_eq!(plan.strays[0].from, stray);
    assert_eq!(plan.strays[0].to, target);
    assert_eq!(plan.strays[0].how, How::Moved);
    assert_eq!(plan.report()["strays"][0]["to"], json!(target));
    let cells = apply(&mut book, &plan);
    let preferences = cell::project(&PREFERENCE, &cells);
    assert!(!preferences.rows[&stray].live());
    assert!(preferences.rows[&target].live());
    assert_eq!(preferences.rows[&target].get("rank"), Some(&json!(3)));
    assert!(run(&cells, &[], &["m104"]).rows.is_empty());

    // Along a chain, and onto what a merge in this plan keeps.
    let merged_row = |pk: &str, label: &str, into: &str| {
        vec![
            put(&MACHINE, pk, "label", json!(label)),
            put(&MACHINE, pk, "merged_into", json!(into)),
            put(&MACHINE, pk, "row", json!("merged")),
        ]
    };
    let mut book = Book::new();
    let (keep, gone) = two_rows(&mut book);
    let (first, second) = (keys::machine_pk("m1"), keys::machine_pk("m2"));
    book.write(2, &[], merged_row(&first, "m1", &second));
    book.write(2, &[], merged_row(&second, "m2", &gone));
    let stray = preference(&mut book, &first, Some(1));
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges.len(), 1, "{:?}", plan.held);
    assert!(plan.strays.is_empty(), "{plan:?}");
    let moved = &plan.merges[0].preferences;
    assert_eq!(moved.len(), 1, "{plan:?}");
    assert_eq!(moved[0].from, stray);
    assert_eq!(moved[0].to, keys::preference_pk(&keep, &account()).unwrap());
    assert!(
        run(&apply(&mut book, &plan), &[], &["m104"])
            .rows
            .is_empty()
    );

    // A `merged_into` that reaches nothing live: a cycle, then a key no row has.
    for into in ["m2", "m9"] {
        let mut book = Book::new();
        let (first, second) = (keys::machine_pk("m1"), keys::machine_pk(into));
        book.write(2, &[], merged_row(&first, "m1", &second));
        if into == "m2" {
            book.write(2, &[], merged_row(&second, "m2", &first));
        }
        let stray = preference(&mut book, &first, Some(1));
        let plan = run(&book.cells(), &[], &[]);
        assert!(plan.rows.is_empty(), "{plan:?}");
        assert_eq!(plan.held.len(), 1, "{plan:?}");
        assert_eq!(plan.held[0].why, "no-merge-target");
        assert!(!stray.is_empty());
    }

    // A `merged_into` through a row that is not merged ends there.
    let mut book = Book::new();
    let (first, second, third) = (
        keys::machine_pk("m1"),
        keys::machine_pk("m2"),
        keys::machine_pk("m3"),
    );
    book.write(2, &[], merged_row(&first, "m1", &second));
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &second, "label", json!("m2")),
            put(&MACHINE, &second, "merged_into", json!(third)),
            put(&MACHINE, &second, "row", json!("retired")),
        ],
    );
    machine_row(&mut book, &third, "m3", Vec::new());
    preference(&mut book, &first, Some(1));
    let plan = run(&book.cells(), &[], &[]);
    assert!(plan.rows.is_empty(), "{plan:?}");
    assert_eq!(plan.held.len(), 1, "{plan:?}");
    assert_eq!(plan.held[0].why, "no-merge-target");

    // A `merged_into` through a merged row a Mac's facts name stops there, the live row it ends at
    // bound or not; a bound live row at its end alone takes the move.
    let mut book = Book::new();
    let (first, second, third) = (
        keys::machine_pk("m1"),
        keys::machine_pk("m2"),
        keys::machine_pk("m3"),
    );
    book.write(2, &[], merged_row(&first, "m1", &second));
    book.write(2, &[], merged_row(&second, "m2", &third));
    machine_row(&mut book, &third, "m3", Vec::new());
    preference(&mut book, &first, Some(1));
    for bound in [vec![second.as_str()], vec![second.as_str(), third.as_str()]] {
        let plan = run(&book.cells(), &bound, &[]);
        assert!(plan.rows.is_empty(), "{plan:?}");
        assert_eq!(plan.held.len(), 1, "{plan:?}");
        assert_eq!(plan.held[0].why, "bound-by-its-mac");
        assert_eq!(plan.held[0].rows, std::slice::from_ref(&first));
    }
    let plan = run(&book.cells(), &[third.as_str()], &[]);
    assert!(plan.held.is_empty(), "{plan:?}");
    assert_eq!(plan.strays.len(), 1, "{plan:?}");
    // A Mac whose facts show it rebound from a row to one further along the chain has moved off it,
    // at either hop; a Mac still on it, one that rebound onto it, or one that went elsewhere holds
    // the move.
    let facts = |rows: &[(&str, Option<&str>)]| {
        rows.iter()
            .map(|(mac, into)| (Some((*mac).to_owned()), into.map(str::to_owned)))
            .collect::<Vec<_>>()
    };
    for rebound in [
        facts(&[(&second, Some(&third))]),
        facts(&[(&first, Some(&third))]),
        facts(&[(&first, Some(&third)), (&second, Some(&third))]),
    ] {
        let plan = run_facts(&book.cells(), rebound, &[]);
        assert!(plan.held.is_empty(), "{plan:?}");
        assert_eq!(plan.strays.len(), 1, "{plan:?}");
    }
    let other = keys::machine_pk("m9");
    for holding in [
        facts(&[(&second, Some(&third)), (&second, None)]),
        facts(&[(&second, Some(&third)), (&other, Some(&second))]),
        facts(&[(&first, Some(&second))]),
        facts(&[(&second, Some(&first))]),
        facts(&[(&first, Some(&other))]),
    ] {
        let plan = run_facts(&book.cells(), holding, &[]);
        assert_eq!(plan.held.len(), 1, "{plan:?}");
        assert_eq!(plan.held[0].why, "bound-by-its-mac");
    }

    // What a merge holds for holds the move.
    let shapes: [Shape; 4] = [
        ("conflicted", |book, stray, _| {
            book.write(3, &[], vec![put(&PREFERENCE, stray, "rank", json!(9))]);
        }),
        ("conflicted", |book, _, target| {
            for (replica, rank) in [(3, 7), (4, 8)] {
                book.write(
                    replica,
                    &[],
                    vec![put(&PREFERENCE, target, "rank", json!(rank))],
                );
            }
        }),
        ("unreadable", |book, stray, _| {
            book.write(
                3,
                &[],
                vec![Change {
                    collection: PREFERENCE.name.to_owned(),
                    key: format!("{stray}.machine"),
                    value: Some(json!("not a cell")),
                }],
            );
        }),
        ("unreadable", |book, _, target| {
            book.write(
                3,
                &[],
                vec![Change {
                    collection: PREFERENCE.name.to_owned(),
                    key: format!("{target}.machine"),
                    value: Some(json!("not a cell")),
                }],
            );
        }),
    ];
    for (why, shape) in shapes {
        let mut book = Book::new();
        let (keep, gone) = two_rows(&mut book);
        let merged = run(&book.cells(), &[], &["m104"]);
        apply(&mut book, &merged);
        let stray = preference(&mut book, &gone, Some(3));
        let target = keys::preference_pk(&keep, &account()).unwrap();
        shape(&mut book, &stray, &target);
        let plan = run(&book.cells(), &[], &["m104"]);
        assert!(plan.strays.is_empty(), "{why} {plan:?}");
        assert!(plan.rows.is_empty(), "{why} {plan:?}");
        assert_eq!(plan.held.len(), 1, "{plan:?}");
        assert_eq!(plan.held[0].why, why);
        assert_eq!(plan.held[0].rows, [gone]);
    }

    // A `merged_into` that cannot be told holds the move, as does a row a Mac's facts still name.
    let shapes: [Shape; 3] = [
        ("unreadable", |book, gone, _| {
            book.write(
                3,
                &[],
                vec![Change {
                    collection: MACHINE.name.to_owned(),
                    key: format!("{gone}.merged_into"),
                    value: Some(json!("not a cell")),
                }],
            );
        }),
        ("conflicted", |book, gone, keep| {
            let other = keys::machine_pk("m9");
            machine_row(book, &other, "m9", Vec::new());
            for (replica, into) in [(3, keep), (4, other.as_str())] {
                book.write(
                    replica,
                    &[],
                    vec![put(&MACHINE, gone, "merged_into", json!(into))],
                );
            }
        }),
        ("bound-by-its-mac", |_, _, _| {}),
    ];
    for (why, shape) in shapes {
        let mut book = Book::new();
        let (keep, gone) = two_rows(&mut book);
        let merged = run(&book.cells(), &[], &["m104"]);
        apply(&mut book, &merged);
        preference(&mut book, &gone, Some(3));
        shape(&mut book, &gone, &keep);
        let bound: &[&str] = if why == "bound-by-its-mac" {
            &[&gone]
        } else {
            &[]
        };
        let plan = run(&book.cells(), bound, &["m104"]);
        assert!(plan.rows.is_empty(), "{why} {plan:?}");
        assert_eq!(plan.held.len(), 1, "{why} {plan:?}");
        assert_eq!(plan.held[0].why, why);
        assert_eq!(plan.held[0].rows, [gone]);
    }

    // A preference on a row retired, not merged, is no stray: it stays where the owner left it.
    let mut book = Book::new();
    let retired = keys::machine_pk("m1");
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &retired, "label", json!("m1")),
            put(&MACHINE, &retired, "row", json!("retired")),
        ],
    );
    preference(&mut book, &retired, Some(1));
    let plan = run(&book.cells(), &[], &[]);
    assert!(plan.rows.is_empty(), "{plan:?}");
    assert!(plan.held.is_empty(), "{plan:?}");
}

/// An import's `active` on a row no Mac has joined goes back to `planned` with its provenance, the
/// word `register::pick` adopts a Mac's label by, and nothing else does: not the owner's word, nor a
/// row with a principal, a host or a Mac's facts, a row merged or held, nor any row it cannot read. A
/// plan over the result is empty, and a replica still saying `active` writes no status.
/// NEGATIVE CONTROL: with each condition of `wait_for_macs` dropped, the row it spares joins the list:
/// `left: [("mc-2042463f4ef5fb03", "m1"), ("mc-76c301532f85bec9", "m12"), ("mc-962e227ed7abc82b", "m4")]`
/// for a host, and likewise for another word, the owner's word (`m2`), a principal or a Mac's facts, a
/// conflict, a held row, and a label, status, node or host it cannot read. With no provenance on the
/// write: `left: None right: Some("sr-0123456789abcdef")`.
#[test]
fn an_imports_active_on_a_row_no_mac_joined_goes_back_to_planned() {
    let mut book = Book::new();
    let pk = |label: &str| keys::machine_pk(label);
    let raw = |pk: &str, col: &str| Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{pk}.{col}"),
        value: Some(json!("not a cell")),
    };
    let (m1, m2, m3, m4, m5, m6, m7) = (
        pk("m1"),
        pk("m2"),
        pk("m3"),
        pk("m4"),
        pk("m5"),
        pk("m6"),
        pk("m7"),
    );
    let (m8, m9, m10, m11, m12, m13) = (
        pk("m8"),
        pk("m9"),
        pk("m10"),
        pk("m11"),
        pk("m12"),
        pk("m13"),
    );
    let held = pk("m14");
    let held_too = pk("m14/2");
    book.write(2, &[], imported_row(&m1, "m1", "active", Vec::new()));
    machine_row(
        &mut book,
        &m2,
        "m2",
        vec![put(&MACHINE, &m2, "status", json!("active"))],
    );
    book.write(
        2,
        &[],
        imported_row(
            &m3,
            "m3",
            "active",
            vec![put(&MACHINE, &m3, "node", json!("0011223344556677"))],
        ),
    );
    book.write(
        2,
        &[],
        imported_row(
            &m4,
            "m4",
            "active",
            vec![put(&MACHINE, &m4, "hosts", json!(["m4.local"]))],
        ),
    );
    book.write(2, &[], imported_row(&m5, "m5", "active", Vec::new()));
    book.write(2, &[], imported_row(&m6, "m6", "planned", Vec::new()));
    book.write(2, &[], imported_row(&m7, "m7", "parked", Vec::new()));
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE, &m8, "label", json!("m8")),
            put(&MACHINE, &m8, "row", json!("retired")),
            put(&MACHINE, &m8, "status", json!("active")),
        ],
    );
    book.write(
        2,
        &[],
        imported_row(&m9, "m9", "active", vec![raw(&m9, "node")]),
    );
    book.write(2, &[], imported_row(&m10, "m10", "active", Vec::new()));
    book.write(3, &[], vec![put(&MACHINE, &m10, "status", json!("parked"))]);
    book.write(2, &[], imported_row(&m11, "m11", "active", Vec::new()));
    book.write(3, &[], vec![put(&MACHINE, &m11, "note", json!("a"))]);
    book.write(4, &[], vec![put(&MACHINE, &m11, "note", json!("b"))]);
    book.write(
        2,
        &[],
        imported_row(
            &m12,
            "m12",
            "active",
            vec![put(&MACHINE, &m12, "fleet_node", json!(NODE))],
        ),
    );
    // Each column that says whether a row waits, as a newer build wrote it: a label this build would
    // not take for one, then a status and a host beside a word it reads.
    let mut unread = imported_row(&m13, "m13", "active", Vec::new());
    unread.retain(|change| change.key != format!("{m13}.label"));
    unread.push(Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{m13}.label"),
        value: Some(json!({"r": MACHINE.version, "x": "not a label!", "s": "sr-0123456789abcdef"})),
    });
    book.write(2, &[], unread);
    let (m15, m16) = (pk("m15"), pk("m16"));
    book.write(2, &[], imported_row(&m15, "m15", "active", Vec::new()));
    book.write(3, &[], vec![raw(&m15, "status")]);
    book.write(2, &[], imported_row(&m16, "m16", "active", Vec::new()));
    book.write(3, &[], vec![raw(&m16, "hosts")]);
    book.write(2, &[], imported_row(&held, "m14", "active", Vec::new()));
    book.write(2, &[], imported_row(&held_too, "m14", "active", Vec::new()));
    let plan = run(&book.cells(), &[&m5], &[]);
    let waiting: Vec<(&str, &str)> = plan
        .waiting
        .iter()
        .map(|row| (row.row.as_str(), row.label.as_str()))
        .collect();
    assert_eq!(
        waiting,
        BTreeSet::from([(m1.as_str(), "m1"), (m12.as_str(), "m12")])
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(plan.held.len(), 1, "{:?}", plan.held);
    assert_eq!(plan.held[0].why, "no-node");
    assert_eq!(plan.report()["waiting"][0]["label"], json!(waiting[0].1));
    let cells = apply(&mut book, &plan);
    let Some(Field::Value(word)) = row(&cells, &m1).fields.get("status").cloned() else {
        panic!("no status");
    };
    assert_eq!(word.value.x, json!("planned"));
    assert_eq!(word.value.s.as_deref(), Some("sr-0123456789abcdef"));
    assert!(run(&cells, &[&m5], &[]).rows.is_empty());
}

/// A row a merge decides waits as the merge left it: the row that stays, with the label it takes,
/// unless it took `planned` or a host from a duplicate, and never the duplicate. A replica still
/// saying `active` of a row set back writes no status.
/// NEGATIVE CONTROL: with no `wait_for_macs`: `left: 0 right: 1`. With the duplicate counted:
/// `left: 2 right: 1`. With the old label listed: `left: "m36" right: "m104"`. With a row that took
/// `planned` or a host still listed: `Plan { .. waiting: [Waiting { row: "mc-0ee010d923d4a1ac", ..`. With
/// `machine_statuses` taking `active` over `planned`, the last assertion prints the `status` it plans:
/// `[Planned { collection: "machine", pk: "mc-71781bc08893dc43", column: "status", x: String("active"),
/// step: Write }, ..`.
#[test]
fn a_merged_row_waits_as_the_merge_left_it() {
    let shape = |gone_status: &str, gone_more: fn(&str) -> Vec<Change>| {
        let keep = keys::machine_pk("m36");
        let gone = keys::machine_pk("m104");
        let mut book = Book::new();
        book.write(
            2,
            &[],
            imported_row(
                &keep,
                "m36",
                "active",
                vec![
                    elem(&MACHINE, &keep, "aliases", "m104"),
                    put(&MACHINE, &keep, "fleet_node", json!(NODE)),
                ],
            ),
        );
        book.write(
            2,
            &[],
            imported_row(&gone, "m104", gone_status, gone_more(&gone)),
        );
        (book, keep, gone)
    };
    let (mut book, keep, gone) = shape("active", |_| Vec::new());
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges.len(), 1, "{:?}", plan.held);
    assert_eq!(plan.waiting.len(), 1, "{plan:?}");
    assert_eq!(plan.waiting[0].row, keep);
    assert_eq!(plan.waiting[0].label, "m104");
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("planned")));
    assert_eq!(row(&cells, &gone).get("status"), Some(&json!("active")));
    assert!(run(&cells, &[], &["m104"]).rows.is_empty());

    let (book, keep, _) = shape("planned", |_| Vec::new());
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges[0].carried[0].1, "status", "{plan:?}");
    assert!(plan.waiting.is_empty(), "{plan:?}");
    assert_eq!(plan.merges[0].keep, keep);

    let (book, _, _) = shape("active", |gone| {
        vec![put(&MACHINE, gone, "hosts", json!(["m104.local"]))]
    });
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges[0].carried[0].1, "hosts", "{plan:?}");
    assert!(plan.waiting.is_empty(), "{plan:?}");

    // A replica still saying `active`, whose import wrote the word: its status stays `planned`.
    let sheets = import::dump_sheets(
        "fleet-db-m104.json",
        &aterm_json::to_vec(&json!({"computers": [
            {"label": "m50", "hostname": "m50.local", "status": "active"}
        ]}))
        .unwrap(),
        Mode::Authoritative,
        &KEY,
    )
    .unwrap();
    let pk = keys::machine_pk("m50");
    let source = sheets[0].source.clone();
    let mut book = Book::new();
    let cell = |col: &str, x: Value| {
        cell::change(
            &MACHINE,
            CellKey {
                pk: &pk,
                col,
                elem: None,
            },
            &CellValue::imported(MACHINE.version, x, &source),
        )
        .unwrap()
    };
    book.write(
        2,
        &[],
        vec![
            cell("label", json!("m50")),
            cell("row", json!("live")),
            cell("status", json!("active")),
        ],
    );
    let plan = run(&book.cells(), &[], &[]);
    assert_eq!(plan.waiting.len(), 1, "{plan:?}");
    let cells = apply(&mut book, &plan);
    let import = import::plan(&import_input(&sheets, &[]), &cells).unwrap();
    assert!(
        import
            .planned
            .iter()
            .all(|planned| planned.column != "status"),
        "{:?}",
        import.planned
    );
}

/// A row a merge keeps waits from the merge's own plan, also with no `status` of its own, taking the
/// word an import's `active` means there, and with no label, taking the one the merge gives it: a
/// plan over the result is empty. A row bound to its Mac takes the word as it stands.
/// NEGATIVE CONTROL: with `carry_cells` moving an import's `active` as it stands, the row
/// that stays keeps it: `left: Some(String("active")) right: Some(String("planned"))`. Turning it into
/// `planned` on a row bound to its Mac, or for the owner's word: `left: Some(String("planned"))
/// right: Some(String("active"))`. With `wait_for_macs` reading only the row's own label, the row
/// with none waits a plan late: `left: 0 right: 1`.
#[test]
fn a_row_a_merge_keeps_waits_from_the_merges_own_plan() {
    let keep = keys::machine_pk("m36");
    let gone = keys::machine_pk("m104");
    let names = vec![
        elem(&MACHINE, &keep, "aliases", "m104"),
        put(&MACHINE, &keep, "fleet_node", json!(NODE)),
    ];
    let mut book = Book::new();
    machine_row(&mut book, &keep, "m36", names.clone());
    book.write(2, &[], imported_row(&gone, "m104", "active", Vec::new()));
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges.len(), 1, "{:?}", plan.held);
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("planned")));
    assert!(run(&cells, &[], &["m104"]).rows.is_empty());

    let mut book = Book::new();
    machine_row(&mut book, &keep, "m36", names.clone());
    book.write(2, &[], imported_row(&gone, "m104", "active", Vec::new()));
    let plan = run(&book.cells(), &[&keep], &["m104"]);
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("active")));

    // The owner's `active` is a statement, and moves as it stands.
    let mut book = Book::new();
    machine_row(&mut book, &keep, "m36", names.clone());
    machine_row(
        &mut book,
        &gone,
        "m104",
        vec![put(&MACHINE, &gone, "status", json!("active"))],
    );
    let plan = run(&book.cells(), &[], &["m104"]);
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("active")));
    assert!(run(&cells, &[], &["m104"]).rows.is_empty());

    let mut book = Book::new();
    let mut labelless = imported_row(&keep, "m36", "active", names);
    labelless.retain(|change| change.key != format!("{keep}.label"));
    book.write(2, &[], labelless);
    book.write(2, &[], imported_row(&gone, "m104", "active", Vec::new()));
    let plan = run(&book.cells(), &[], &["m104"]);
    assert_eq!(plan.merges.len(), 1, "{:?}", plan.held);
    assert_eq!(plan.waiting.len(), 1, "{plan:?}");
    assert_eq!(plan.waiting[0].label, "m104");
    let cells = apply(&mut book, &plan);
    assert_eq!(row(&cells, &keep).get("status"), Some(&json!("planned")));
    assert!(run(&cells, &[], &["m104"]).rows.is_empty());
}

/// A row no Mac joined that the roster gives another Mac, or two of its names, is held, not set
/// waiting: `register::pick` would adopt it for the wrong Mac. A row the roster's node for its name
/// agrees with waits.
/// NEGATIVE CONTROL: with either hold off, or `other_mac` reading only the node the roster
/// states for the row's name or only the nodes of its other names, the row waits: `Plan { merges:
/// [], held: [], strays: [], waiting: [Waiting { row: "mc-2042463f4ef5fb03", label: "m1" }], ..`.
#[test]
fn a_row_the_roster_gives_another_mac_does_not_wait() {
    let stated = |node: &str| BTreeMap::from([("fleet_node", node.to_owned())]);
    let run_on = |cells: &Cells, roster: &Roster| {
        plan(
            &cell::project(&MACHINE, cells),
            &cell::project(&PREFERENCE, cells),
            &Facts::default(),
            roster,
        )
        .unwrap()
    };
    let pk = keys::machine_pk("m1");
    let row_with_node = || {
        let mut book = Book::new();
        book.write(
            2,
            &[],
            imported_row(
                &pk,
                "m1",
                "active",
                vec![put(&MACHINE, &pk, "fleet_node", json!(NODE))],
            ),
        );
        book
    };
    let book = row_with_node();
    for roster in [
        Roster::from([("m1".to_owned(), stated(OTHER_NODE))]),
        Roster::from([
            ("m1".to_owned(), BTreeMap::new()),
            ("m9".to_owned(), stated(NODE)),
        ]),
    ] {
        let plan = run_on(&book.cells(), &roster);
        assert!(plan.waiting.is_empty(), "{plan:?}");
        assert!(plan.rows.is_empty(), "{plan:?}");
        assert_eq!(plan.held.len(), 1, "{plan:?}");
        assert_eq!(plan.held[0].why, "two-macs");
    }
    let agrees = Roster::from([("m1".to_owned(), stated(NODE))]);
    assert_eq!(run_on(&book.cells(), &agrees).waiting.len(), 1);

    let mut book = Book::new();
    book.write(
        2,
        &[],
        imported_row(
            &pk,
            "m1",
            "active",
            vec![elem(&MACHINE, &pk, "aliases", "m2")],
        ),
    );
    let plan = run(&book.cells(), &[], &["m1", "m2"]);
    assert!(plan.waiting.is_empty(), "{plan:?}");
    assert_eq!(plan.held.len(), 1, "{plan:?}");
    assert_eq!(plan.held[0].why, "two-roster-names");
    assert_eq!(plan.held[0].names, ["m1", "m2"]);
}

/// A row `merge` set waiting goes back to `active` once its Mac publishes its principal: the roster's
/// word ends the wait whichever import wrote `planned`, and the plans settle. The owner's `planned`
/// stands against it, and so does an import's against `adopt`, which finds the row by its label.
/// NEGATIVE CONTROL: with the roster's `active` deciding as any import's, or the row it found by the
/// principal not recorded as joined, it is a finding: `[Finding { kind: AdoptedByNode, .. },
/// Finding { kind: Differs, collection: Some("machine"), pk: Some("mc-71781bc08893dc43"), column:
/// Some("status"), ..`. With any word ending the wait: `left: Some(String("parked")) right:
/// Some(String("planned"))`; with `active` ending any word: `left: Some(String("active")) right:
/// Some(String("parked"))`. With any row the roster found ending it: `left: Some(String("active"))
/// right: Some(String("planned"))`.
#[test]
fn a_mac_joining_a_row_merge_set_waiting_makes_it_active() {
    const PRINCIPAL: &str = "0011223344556677";
    let pk = keys::machine_pk("m50");
    let mac = KnownMac {
        label: Some(Label::plain(50)),
        aliases: Vec::new(),
        node: Some(PRINCIPAL.to_owned()),
        fleet_node: None,
        hosts: Vec::new(),
    };
    let macs = [mac];
    let mut book = Book::new();
    book.write(2, &[], imported_row(&pk, "m50", "active", Vec::new()));
    let plan = run(&book.cells(), &[], &[]);
    assert_eq!(plan.waiting.len(), 1, "{plan:?}");
    apply(&mut book, &plan);
    book.write(3, &[], vec![put(&MACHINE, &pk, "node", json!(PRINCIPAL))]);
    let joined = import::plan(&import_input(&[], &macs), &book.cells()).unwrap();
    assert!(
        joined
            .findings
            .iter()
            .all(|found| found.kind != import::Kind::Differs),
        "{:?}",
        joined.findings
    );
    let cells = apply_rows(&mut book, &joined.rows);
    assert_eq!(row(&cells, &pk).get("status"), Some(&json!("active")));
    let again = import::plan(&import_input(&[], &macs), &cells).unwrap();
    assert!(again.rows.is_empty(), "{:?}", again.planned);
    assert!(run(&cells, &[&pk], &[]).rows.is_empty());

    let mut book = Book::new();
    machine_row(
        &mut book,
        &pk,
        "m50",
        vec![
            put(&MACHINE, &pk, "status", json!("planned")),
            put(&MACHINE, &pk, "node", json!(PRINCIPAL)),
        ],
    );
    let owner = import::plan(&import_input(&[], &macs), &book.cells()).unwrap();
    let cells = apply_rows(&mut book, &owner.rows);
    assert_eq!(row(&cells, &pk).get("status"), Some(&json!("planned")));

    // Only `active` ends the wait, and it ends only `planned`: another import's `parked` stands
    // against the roster, and a replica's `parked` against `planned`, under fill-only.
    let mut book = Book::new();
    book.write(
        2,
        &[],
        imported_row(
            &pk,
            "m50",
            "parked",
            vec![put(&MACHINE, &pk, "node", json!(PRINCIPAL))],
        ),
    );
    let parked = import::plan(&import_input(&[], &macs), &book.cells()).unwrap();
    let cells = apply_rows(&mut book, &parked.rows);
    assert_eq!(row(&cells, &pk).get("status"), Some(&json!("parked")));

    let mut book = Book::new();
    book.write(2, &[], imported_row(&pk, "m50", "planned", Vec::new()));
    let sheets = import::dump_sheets(
        "fleet-db-m104.json",
        &aterm_json::to_vec(&json!({"computers": [
            {"label": "m50", "hostname": "m50.local", "status": "parked"}
        ]}))
        .unwrap(),
        Mode::FillOnly,
        &KEY,
    )
    .unwrap();
    let replica = import::plan(&import_input(&sheets, &[]), &book.cells()).unwrap();
    let cells = apply_rows(&mut book, &replica.rows);
    assert_eq!(row(&cells, &pk).get("status"), Some(&json!("planned")));

    // `adopt` finds the row by its label, which says nothing of the Mac having joined: an import's
    // `planned` stands until the Mac publishes its principal.
    let mut book = Book::new();
    book.write(2, &[], imported_row(&pk, "m50", "planned", Vec::new()));
    let adopt = Input {
        births: false,
        ..import_input(&[], &macs)
    };
    let adopted = import::plan(&adopt, &book.cells()).unwrap();
    let cells = apply_rows(&mut book, &adopted.rows);
    assert_eq!(row(&cells, &pk).get("status"), Some(&json!("planned")));
    // Nor over the roster's own `planned`, on the row it birthed under the Mac's key: `adopt` and
    // `merge` then plan nothing over each other.
    let mut book = Book::new();
    let born = import::plan(&import_input(&[], &macs), &book.cells()).unwrap();
    apply_rows(&mut book, &born.rows);
    let keyed = keys::machine_pk(&format!("n-{PRINCIPAL}"));
    let adopted = import::plan(&adopt, &book.cells()).unwrap();
    let cells = apply_rows(&mut book, &adopted.rows);
    assert_eq!(row(&cells, &keyed).get("status"), Some(&json!("planned")));
    assert!(run(&cells, &[], &[]).rows.is_empty());
}

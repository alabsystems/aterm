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

fn roster_of(labels: &[&str]) -> Roster {
    labels
        .iter()
        .map(|label| ((*label).to_owned(), BTreeMap::new()))
        .collect()
}

fn run(cells: &Cells, bound: &[&str], roster: &[&str]) -> Plan {
    let bound = bound.iter().map(|pk| (*pk).to_owned()).collect();
    plan(
        &cell::project(&MACHINE, cells),
        &cell::project(&PREFERENCE, cells),
        &bound,
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
    assert_eq!(plan.held[0].why, "two-macs");

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
        &BTreeSet::new(),
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

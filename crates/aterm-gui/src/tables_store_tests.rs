// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the Tables page's read of the registry says of the values aterm sets, on a scratch
//! registry (`aterm_db::table::fixture::Lab`: an isolated home, no live vault, fleet or
//! Commons). Every value is synthetic.

use crate::App;
use crate::native_app::TextInputEvent;
use crate::native_tables::native_tables_tests::{
    act, cell_says, key_in, put_in, render, said, settle, settle_write,
};
use aterm_db::alab::Registry;
use aterm_db::alab::registry::{PLAN, PREFERENCE, SOURCE};
use aterm_db::table::fixture::Lab;
use aterm_json::json;
use aterm_secrets::commons::roster::Role;

/// When the synthetic import ran (2026-11-16, Unix milliseconds).
const AT_MS: u64 = 1_794_829_979_346;

fn opened(lab: &Lab) -> App {
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    app
}

#[test]
fn machines_publish_before_unrelated_accounts_and_phones_are_laid_out() {
    use std::cell::Cell;
    use std::ops::ControlFlow;

    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    Registry::at(paths.clone()).open().unwrap();
    let before = aterm_db::typed::claim::projection_count("machine");
    let read = super::Read::open(paths, &aterm_db::att::REGISTRY, None, false).unwrap();
    let mut family = super::OpeningFamily::new(read);
    let published = Cell::new(false);
    let mut opening = super::Opening {
        wanted: &|| Some("machine"),
        publish: &mut |sheet, _| {
            assert_eq!(sheet.name, "machine");
            published.set(true);
        },
    };
    // Each turn can lay out one required sheet or publish Machines.
    for _ in 0..=family.read.listed.len() {
        let ControlFlow::Continue(next) = family
            .step(paths, Some("machine"), None, &mut opening)
            .unwrap()
        else {
            panic!("the read ended before Machines reached the page");
        };
        family = next;
        if published.get() {
            break;
        }
    }
    assert!(published.get());
    assert_eq!(
        aterm_db::typed::claim::projection_count("machine") - before,
        1
    );
    assert!(matches!(family.sheets.get("machine"), Some(None)));
    for unrelated in ["account", "phone"] {
        assert!(
            family
                .read
                .listed
                .iter()
                .any(|entry| entry.name == unrelated)
        );
        assert!(
            !family.sheets.contains_key(unrelated),
            "{unrelated} was laid out"
        );
    }
}

#[test]
fn sheet_histories_keep_interleaved_changes_in_one_operation() {
    use crate::native_tables::Written;
    use aterm_db::alab::registry::MACHINE;

    fn fields(record: &Written) -> Vec<(&str, &str)> {
        record
            .cells
            .iter()
            .map(|c| (c.column.as_str(), c.now.as_str()))
            .collect()
    }

    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let here = Registry::at(paths.clone());
    here.open().unwrap();
    let machine = "mc-0000000000000001";
    let source = aterm_db::typed::keys::source_pk(b"inventory");
    here.write(vec![
        vec![
            put_in(&MACHINE, machine, "label", json!("m1")),
            put_in(&SOURCE, &source, "name", json!("inventory")),
            put_in(&MACHINE, machine, "location", json!("rack a")),
        ],
        vec![put_in(&MACHINE, machine, "location", json!("rack b"))],
    ])
    .unwrap();

    let (snapshot, _) = super::read(paths, super::FAMILIES, None, false).unwrap();
    let sheet = |name| {
        snapshot
            .sheets
            .iter()
            .find(|sheet| sheet.name == name)
            .unwrap()
    };
    let [first, second] = sheet("machine").written.as_slice() else {
        panic!("expected both machine operations once each");
    };
    let [source] = sheet("source").written.as_slice() else {
        panic!("expected only the shared operation on the source sheet");
    };
    assert_eq!(source.operation, first.operation);
    assert_eq!(fields(first), [("label", "m1"), ("location", "rack a")]);
    assert_eq!(fields(source), [("name", "inventory")]);
    assert_eq!(fields(second), [("location", "rack b")]);
}

#[test]
fn a_past_machine_cell_keeps_its_value_while_its_writer_and_references_use_the_head() {
    use aterm_db::alab::registry::{MACHINE, MACHINE_FACTS};
    use aterm_db::att::{Handle, REGISTRY};
    use aterm_db::database::Change;
    use aterm_db::typed::keys;

    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let here = Registry::at(paths.clone());
    here.open().unwrap();
    let machine = keys::machine_pk("m1");
    let signer = keys::uuid32(&lab.machine(0));
    here.write(vec![
        vec![put_in(&MACHINE, &machine, "label", json!("m1"))],
        vec![Change {
            collection: MACHINE_FACTS.name.into(),
            key: signer.clone(),
            value: Some(json!({"r": 1, "n": 1, "mac": machine, "label": "m1"})),
        }],
    ])
    .unwrap();
    let point = Handle::at(paths.clone(), &REGISTRY)
        .history()
        .unwrap()
        .into_iter()
        .find(|entry| {
            entry
                .changes
                .iter()
                .any(|c| c.collection == MACHINE_FACTS.name)
        })
        .unwrap()
        .operation;
    here.write(vec![vec![put_in(&MACHINE, &machine, "label", json!("m2"))]])
        .unwrap();

    let (past, _) = super::read(paths, super::FAMILIES, Some(point), false).unwrap();
    let row = |collection, pk: &str| {
        past.sheets
            .iter()
            .find(|s| s.name == collection)
            .unwrap()
            .rows
            .iter()
            .find(|r| r.pk == pk)
            .unwrap()
    };
    let label = &row(MACHINE.name, &machine).cells["label"];
    assert_eq!(label.text, "m1");
    assert_eq!(label.by.as_ref().unwrap().label.as_deref(), Some("m2"));
    let reference = &row(MACHINE_FACTS.name, &signer).cells["mac"];
    assert_eq!(reference.text, "m2");
    assert_eq!(reference.refers.as_deref(), Some(machine.as_str()));
}

/// A time aterm sets on a sheet of its own (an import's `at`, SET BY ATERM) reads as its date,
/// as an owned row's time does, in its cell and in its history; a cell the page edits still holds
/// the number the grid writes, which an open cell and a Write take, and the line under the grid
/// names no key for it.
/// NEGATIVE CONTROL, observed: with `cells_sheet` showing the grid's text (its `words` never
/// taken), the import's time reads as its number: `` left: "1794829979346" right: "2026-11-16
/// 03:52" ``; with the history's line read by `shown_value` alone, `` its history says the date
/// too left: ["1794829979346"] right: ["2026-11-16 03:52"] ``; with `provenance` naming
/// `Shown::holds` as it names a reference's key, `` At (1794829979346): set at df5abbdb just now
/// ``.
#[test]
fn a_time_aterm_sets_reads_as_its_date() {
    let lab = Lab::new(&[Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    let pk = aterm_db::typed::keys::source_pk(b"a synthetic import");
    here.write(vec![vec![
        put_in(&SOURCE, &pk, "kind", json!("swarm-roster")),
        put_in(&SOURCE, &pk, "name", json!("synthetic roster")),
        put_in(&SOURCE, &pk, "sha256", json!("0".repeat(64))),
        put_in(&SOURCE, &pk, "rows", json!(2)),
        put_in(&SOURCE, &pk, "at", json!(AT_MS)),
        put_in(&SOURCE, &pk, "row", json!("live")),
    ]])
    .unwrap();
    let mut app = opened(&lab);
    act(&mut app, "tables/sheet/source", None);
    let date = crate::native_tables::stamp_words(AT_MS);
    assert!(date.starts_with("2026-11-1"), "{date}");
    assert_eq!(cell_says(&render(&app), &pk, "at"), date);
    let tables = app.native_runtime.tables_mut().unwrap();
    let sheet = tables.sheet_named("source").unwrap();
    let column = sheet.columns.iter().find(|c| c.name == "at").unwrap();
    let cell = &sheet.rows.iter().find(|r| r.pk == pk).unwrap().cells["at"];
    assert_eq!(cell.text, date);
    let holds = if column.editable {
        AT_MS.to_string()
    } else {
        date.clone()
    };
    assert_eq!(
        cell.value(),
        holds,
        "an open cell and a Write take the grid's text"
    );
    let written: Vec<&str> = sheet
        .written
        .iter()
        .flat_map(|w| &w.cells)
        .filter(|c| c.pk == pk && c.column == "at")
        .map(|c| c.now.as_str())
        .collect();
    assert_eq!(written, [date.as_str()], "its history says the date too");
    // The line under the grid names no key for it: the number is what it holds, not a row.
    act(&mut app, &format!("tables/cell/{pk}/at"), None);
    let line = said(&render(&app), "tables/said/words").expect("the cell says where it is from");
    assert!(
        line.starts_with("At: ") && !line.contains(&AT_MS.to_string()),
        "{line}"
    );
}

/// A time the owner stores (a link's `since`, STORED) reads as its date too, opens on the number
/// the grid writes, and a time typed into it and staged is said as a date beside the date the
/// registry holds, never one form beside the other.
/// NEGATIVE CONTROL, observed: with the words kept to the columns aterm sets (`set_words` taken
/// only for SET BY ATERM and REFERENCE), the link's time reads as its number: `` left:
/// "1794829979346" right: "2026-11-16 03:52" ``; with `provenance` saying a staged value by
/// `value_words`, `` left: "Since: 1794829999346 set here, not written; the registry holds
/// 2026-11-16 03:52" ``.
#[test]
fn a_time_the_owner_stores_reads_as_its_date() {
    let lab = Lab::new(&[Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    let (machine, account) = ("mc-0000000000000009", "ac-000000000000000a");
    let pk = aterm_db::typed::keys::preference_pk(machine, account).unwrap();
    here.write(vec![vec![
        put_in(&PREFERENCE, &pk, "machine", json!(machine)),
        put_in(&PREFERENCE, &pk, "account", json!(account)),
        put_in(&PREFERENCE, &pk, "why", json!("owner")),
        put_in(&PREFERENCE, &pk, "since", json!(AT_MS)),
        put_in(&PREFERENCE, &pk, "row", json!("live")),
    ]])
    .unwrap();
    let mut app = opened(&lab);
    act(&mut app, "tables/sheet/preference", None);
    let date = crate::native_tables::stamp_words(AT_MS);
    assert_eq!(cell_says(&render(&app), &pk, "since"), date);
    let cell = format!("tables/cell/{pk}/since");
    act(&mut app, &cell, None);
    let line = said(&render(&app), "tables/said/words").expect("the cell says where it is from");
    assert!(!line.contains(&AT_MS.to_string()), "{line}");
    act(&mut app, &cell, None);
    let ui = render(&app);
    let field = ui
        .semantic(&crate::native_ui::UiKey::new("tables/edit".to_owned()))
        .map(|node| node.value.clone());
    assert_eq!(
        field,
        Some(crate::native_ui::SemanticValue::Text(AT_MS.to_string())),
        "the cell opens on the number the grid writes"
    );
    let typed = AT_MS + 20_000;
    key_in(&mut app, TextInputEvent::SelectAll);
    key_in(&mut app, TextInputEvent::Commit(typed.to_string()));
    key_in(&mut app, TextInputEvent::Submit);
    let line = said(&render(&app), "tables/said/words").expect("the staged cell says so");
    assert_eq!(
        line,
        format!(
            "Since: {} set here, not written; the registry holds {date}",
            crate::native_tables::stamp_words(typed)
        )
    );
}

/// A plan's limits read in words, each window as the rest of aterm names it (`5-hour`,
/// `weekly`) and what it counts, a window alone where the plan states no amount, and a plan that
/// names no window reads empty; no JSON shows, and one limit this build cannot read says only
/// itself.
/// NEGATIVE CONTROL, observed: with `set_words` leaving out `limits_words`, Claude Max's limits
/// read as the grid writes them: `` [{"unit":"pct","w":"5h"},{"unit":"pct","w":"7d"}], set by
/// aterm ``. With one limit this build cannot read making the whole list `None` (each limit's
/// words collected with `?`, its `unit` read with `?`), `` left: None right: Some("5-hour ·
/// weekly: \"lots\" messages · {\"unit\":\"pct\"}") ``.
#[test]
fn a_plans_limits_read_in_words() {
    let lab = Lab::new(&[Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    here.seed().unwrap();
    let mut app = opened(&lab);
    act(&mut app, "tables/sheet/plan", None);
    let max = "5-hour \u{b7} weekly";
    let said = cell_says(&render(&app), "anthropic-max", "limits");
    assert!(said.starts_with(max), "{said}");
    let tables = app.native_runtime.tables_mut().unwrap();
    let sheet = tables.sheet_named("plan").unwrap();
    let limits = |pk: &str| {
        sheet.rows.iter().find(|r| r.pk == pk).unwrap().cells["limits"]
            .text
            .clone()
    };
    assert_eq!(limits("anthropic-max"), max);
    assert_eq!(limits("anthropic-team"), "", "no window reads empty");

    let ty = &PLAN.column("limits").unwrap().ty;
    let limits = json!([
        {"w": "5h", "unit": "messages", "nominal": 45},
        {"w": "7d", "unit": "messages", "nominal": 225},
        {"w": "7d-model", "model": "Opus", "unit": "pct"},
        {"w": "7d-model", "unit": "tokens"},
        {"w": "month", "unit": "usd-cents", "nominal": 2000},
        {"w": "day", "unit": "pct", "nominal": 80},
        {"w": "credits", "unit": "usd-cents"},
    ]);
    assert_eq!(
        super::limits_words(ty, &limits).as_deref(),
        Some(
            "5-hour: 45 messages \u{b7} weekly: 225 messages \u{b7} Opus weekly \u{b7} \
             one model's weekly: in tokens \u{b7} monthly: $20.00 \u{b7} daily: 80% \u{b7} \
             credits: in dollars"
        )
    );
    // A newer build's shape: one limit with no unit, one whose amount is no whole number, one
    // with no window; each reads as far as this build can, and the rest in words.
    let newer = json!([
        {"w": "5h"},
        {"w": "7d", "unit": "messages", "nominal": "lots"},
        {"unit": "pct"},
    ]);
    assert_eq!(
        super::limits_words(ty, &newer).as_deref(),
        Some("5-hour \u{b7} weekly: \"lots\" messages \u{b7} {\"unit\":\"pct\"}")
    );
    assert_eq!(super::limits_words(ty, &json!([])).as_deref(), Some(""));
    assert_eq!(
        super::limits_words(&aterm_db::typed::Ty::Json(1024), &limits),
        None,
        "only a plan's limits read so"
    );
}

/// An owned or log row's whole value says, opened, how many fields it holds, and written over
/// one row, which fields changed, in words; the counter that orders a Mac's statements is none
/// of them, and no value is said.
/// NEGATIVE CONTROL: with the counter kept among the fields (`!= COUNTER` filter taken out), ``
/// left: "Location, N changed" right: "Location changed" ``.
#[test]
fn a_whole_rows_change_names_the_fields_it_changed() {
    let now = aterm_json::json!({"n": 3, "location": "rack b", "ram_gb": 8});
    let was = aterm_json::json!({"n": 2, "location": "rack a", "ram_gb": 8});
    assert_eq!(super::whole_row_words(&now, Some(&was)), "Location changed");
    assert_eq!(super::whole_row_words(&now, None), "a row of 2 fields");
    assert_eq!(super::whole_row_words(&now, Some(&now)), "no field changed");
    assert_eq!(
        super::whole_row_words(&aterm_json::json!("x"), None),
        "a row"
    );
}

/// History can list records of every collection of a generation, the rail's first in its
/// order, then those the rail does not list (a carried face) by their faces' titles: what its
/// table filter offers.
/// NEGATIVE CONTROL: with only the rail's listed (the `extend` taken out), `` left: 17 right: 19
/// ``.
#[test]
fn history_lists_every_table_of_a_generation_the_rails_first() {
    let table = &aterm_db::alab::registry::REGISTRY_4;
    let rail: Vec<String> = super::listed(table)
        .iter()
        .map(|listed| listed.name.to_owned())
        .collect();
    let tables = super::history_tables(table);
    assert_eq!(tables.len(), table.collections.len());
    let names: Vec<&str> = tables.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names[..rail.len()], rail);
    assert!(
        tables[rail.len()..]
            .iter()
            .any(|(name, title)| name == "archived" && title == "Archived rows"),
        "{tables:?}"
    );
}

/// History's reads keep the registry's order between them (tracker LT-25B): the first read
/// keeps the order at the registry's revision, Read older at that revision is cut from it,
/// reading the registry no more (`Kept::reads`; that such a page decodes no record is the
/// store's own test,
/// `aterm_db::att::tests::a_history_page_at_a_held_revision_reads_no_record_again`), and lists
/// what a reader that keeps nothing lists; once a record is written the next read reads the new
/// revision, says the registry moved, and lists the new record at its top.
/// NEGATIVE CONTROL, observed: with `history` reading each store page into a `Kept` of its own
/// (the reader's never filled), `` the first read keeps the registry's order left: None right:
/// Some(..) ``. With the kept order never matched (`aterm_db::att`'s `Order::of` answering
/// `false`), Read older reads the registry again: `Read older at the held revision read the
/// registry again left: 2 right: 1`.
///
/// The change check's mark shares that `Kept` (tracker LT-25D): asked by a reader that never
/// reads History it keeps what its open read of the registry's files and no History, and once a
/// record is written it reads the new revision. That the page's reader hands each check what
/// it keeps is `native_tables`' test
/// (`the_tables_reader_asks_each_change_check_with_the_sighting_it_keeps`).
/// NEGATIVE CONTROL, observed: with `mark` reading the plain revision
/// (`aterm_db::att::Handle::revision`, `kept` left alone), `` the mark keeps what its open read
/// left: None right: Some(..) ``.
#[test]
fn historys_reads_keep_the_registrys_order_until_it_moves() {
    use crate::native_tables::Asked;
    use aterm_db::alab::registry::MACHINE;
    let lab = Lab::new(&[Role::Writer]);
    let here = Registry::at(lab.paths[0].clone());
    here.open().unwrap();
    let label = |pk: &str, label: &str| vec![put_in(&MACHINE, pk, "label", json!(label))];
    here.write(
        (1..=5)
            .map(|n| label(&format!("mc-000000000000000{n}"), &format!("m{n}")))
            .collect(),
    )
    .unwrap();
    let paths = &lab.paths[0];
    let asked = Asked {
        seq: 1,
        filter: Default::default(),
        names: std::collections::BTreeMap::new(),
        before: None,
        revision: None,
        through: None,
        limit: 2,
    };
    let mut kept = super::Kept::default();
    let first = super::history(paths, super::FAMILIES, &asked, &mut kept).unwrap();
    assert_eq!(
        kept.revision().map(|revision| revision.0),
        Some(first.revision),
        "the first read keeps the registry's order"
    );
    assert_eq!(
        super::mark(paths, super::FAMILIES, &mut kept)
            .unwrap()
            .digest(),
        first.revision
    );
    // A reader that never reads History keeps of its mark what the registry's open read of
    // its files, and no History (tracker LT-25D; that a mark asked again then opens no store
    // is the store's own test,
    // `aterm_db::att::tests::a_revision_asked_again_opens_no_store_while_it_stands`).
    let mut marked = super::Kept::default();
    assert_eq!(
        super::mark(paths, super::FAMILIES, &mut marked)
            .unwrap()
            .digest(),
        first.revision,
        "a mark asked with nothing kept is the registry's"
    );
    assert_eq!(
        marked.sighted().map(|revision| revision.0),
        Some(first.revision),
        "the mark keeps what its open read"
    );
    assert_eq!(
        (marked.revision(), marked.reads()),
        (None, 0),
        "the mark keeps no History"
    );
    assert_eq!(
        super::mark(paths, super::FAMILIES, &mut marked)
            .unwrap()
            .digest(),
        first.revision,
        "a mark asked again, standing on what it kept, is the registry's"
    );
    assert_eq!(kept.reads(), 1);
    let older = Asked {
        before: first.next,
        revision: Some(first.revision),
        ..asked.clone()
    };
    let read_older = super::history(paths, super::FAMILIES, &older, &mut kept).unwrap();
    assert_eq!(
        kept.reads(),
        1,
        "Read older at the held revision read the registry again"
    );
    assert!(!read_older.moved);
    assert_eq!(read_older.records.len(), 2);
    assert_eq!(
        read_older,
        super::history(paths, super::FAMILIES, &older, &mut super::Kept::default()).unwrap(),
        "kept or not, one page"
    );

    here.write(vec![label("mc-0000000000000006", "m6")])
        .unwrap();
    let moved = super::history(paths, super::FAMILIES, &older, &mut kept).unwrap();
    assert!(moved.moved, "asked at the old revision");
    assert_ne!(moved.revision, first.revision);
    assert_eq!(
        kept.revision().map(|revision| revision.0),
        Some(moved.revision)
    );
    assert_eq!(kept.reads(), 2);
    assert_eq!(moved.records[0].cells[0].pk, "mc-0000000000000006");
    // The mark that kept the old revision's open reads the new one.
    assert_eq!(
        super::mark(paths, super::FAMILIES, &mut marked)
            .unwrap()
            .digest(),
        moved.revision,
        "a mark after a record was written is the registry's"
    );
    assert_eq!(
        super::mark(paths, super::FAMILIES, &mut kept)
            .unwrap()
            .digest(),
        moved.revision
    );
    assert_eq!(kept.reads(), 2);
}

/// A cells collection of the second family [`WORKSHOP`] whose name the registry's own `plan`
/// also has: a plan of the workshop's, keyed by its name.
const WORKSHOP_PLAN: aterm_db::typed::Collection = aterm_db::typed::Collection {
    name: "plan",
    version: 1,
    kind: aterm_db::typed::Kind::Cells,
    writers: aterm_db::typed::Writers::Owner,
    prefix: "",
    derive: aterm_db::typed::Derive::Word,
    columns: &[
        aterm_db::typed::Column::new("name", aterm_db::typed::Ty::Text(32)).fixed(),
        aterm_db::typed::Column::new("row", aterm_db::typed::Ty::Enum(&["live", "retired"])),
    ],
    unique: &[],
    max_row: 4096,
    authority: None,
};

/// A workshop machine has an inventory key, a model title and a physical label.
const WORKSHOP_MACHINE: aterm_db::typed::Collection = aterm_db::typed::Collection {
    name: "machine",
    columns: &[
        aterm_db::typed::Column::new("name", aterm_db::typed::Ty::Text(32)).fixed(),
        aterm_db::typed::Column::new("title", aterm_db::typed::Ty::Text(32)),
        aterm_db::typed::Column::new("label", aterm_db::typed::Ty::Text(32)),
        aterm_db::typed::Column::new("row", aterm_db::typed::Ty::Enum(&["live", "retired"])),
    ],
    ..WORKSHOP_PLAN
};

/// A gadget of [`WORKSHOP`], keyed by its name, made to a workshop plan on a workshop machine.
const GADGET: aterm_db::typed::Collection = aterm_db::typed::Collection {
    name: "gadget",
    version: 1,
    kind: aterm_db::typed::Kind::Cells,
    writers: aterm_db::typed::Writers::Owner,
    prefix: "",
    derive: aterm_db::typed::Derive::Word,
    columns: &[
        aterm_db::typed::Column::new("name", aterm_db::typed::Ty::Text(32)).fixed(),
        aterm_db::typed::Column::new("plan", aterm_db::typed::Ty::Ref("plan")).null(),
        aterm_db::typed::Column::new("tool", aterm_db::typed::Ty::Ref("machine")).null(),
        aterm_db::typed::Column::new("row", aterm_db::typed::Ty::Enum(&["live", "retired"])),
    ],
    unique: &[],
    max_row: 4096,
    authority: None,
};

/// `att-test/workshop`: a table family no build registers, whose tables the rail lists as
/// Plans, Gadgets and Tools, the owner's.
const WORKSHOP_TABLE: aterm_db::typed::Table = aterm_db::typed::Table {
    application: "att-test",
    role: "workshop",
    collections: &[&WORKSHOP_PLAN, &GADGET, &WORKSHOP_MACHINE],
    faces: &[
        aterm_db::typed::Faced {
            collection: "plan",
            title: "Plans",
            singular: "Plan",
            face: aterm_db::typed::Face::Owner,
            order: &["name"],
        },
        aterm_db::typed::Faced {
            collection: "gadget",
            title: "Gadgets",
            singular: "Gadget",
            face: aterm_db::typed::Face::Owner,
            order: &["name", "plan", "tool"],
        },
        aterm_db::typed::Faced {
            collection: "machine",
            title: "Tools",
            singular: "Tool",
            face: aterm_db::typed::Face::Owner,
            order: &["title", "label"],
        },
    ],
    self_quota: usize::MAX,
    ends: &[],
};

/// The words the workshop's own rule refuses a gadget named [`FORBIDDEN`] in.
const WORKSHOP_RULE: &str = "the workshop keeps no gadget of that name";

/// The name the workshop's own rule refuses.
const FORBIDDEN: &str = "forbidden";

/// The workshop's key for a new row: its name.
fn named_birth(
    _: &aterm_db::typed::Collection,
    values: &std::collections::BTreeMap<String, aterm_json::Value>,
    _: &aterm_db::typed::cell::Cells,
) -> aterm_db::Result<String> {
    Ok(values
        .get("name")
        .and_then(aterm_json::Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

/// The workshop's own rule on the owner's writes: no gadget is named [`FORBIDDEN`].
fn workshop_guard(
    rows: &mut [Vec<aterm_db::database::Change>],
    _: &aterm_db::typed::cell::Cells,
) -> aterm_db::Result<Option<aterm_db::typed::Refused>> {
    use aterm_db::typed::cell::{CellKey, CellValue};
    for change in rows.iter().flatten() {
        let Ok(key) = CellKey::parse(&change.key) else {
            continue;
        };
        let named = change
            .value
            .as_ref()
            .and_then(|value| CellValue::decode(value).ok())
            .is_some_and(|cell| cell.x.as_str() == Some(FORBIDDEN));
        if change.collection == "gadget" && key.col == "name" && named {
            return Ok(Some(aterm_db::typed::Refused {
                collection: "gadget",
                pk: key.pk.to_owned(),
                column: "name".to_owned(),
                rule: WORKSHOP_RULE.to_owned(),
            }));
        }
    }
    Ok(None)
}

/// [`WORKSHOP_TABLE`], compiled under its own title, birth and rules.
const WORKSHOP: aterm_db::att::Compiled = aterm_db::att::Compiled {
    table: &WORKSHOP_TABLE,
    title: "Workshop",
    birth: named_birth,
    guard: workshop_guard,
    generation: 1,
    successor: None,
    ..aterm_db::att::REGISTRY
};

/// Open row `pk`'s `column` on the sheet shown: a press selects the cell, a second opens it.
fn open(app: &mut App, pk: &str, column: &str) {
    let cell = format!("tables/cell/{pk}/{column}");
    act(app, &cell, None);
    if said(&render(app), "tables/edit").is_none() {
        act(app, &cell, None);
    }
    assert!(
        said(&render(app), "tables/edit").is_some(),
        "{pk} {column} opened"
    );
}

/// Type `text` into row `pk`'s `column` of the sheet shown, and stage it with Return.
fn type_in(app: &mut App, pk: &str, column: &str, text: &str) {
    open(app, pk, column);
    act(
        app,
        "tables/edit",
        Some(crate::native_app::SemanticInput::Text(text.to_owned())),
    );
    key_in(app, TextInputEvent::Submit);
}

/// The page over the registry and the workshop, the registry first.
static TWO_FAMILIES: [&aterm_db::att::Compiled; 2] = [&aterm_db::att::REGISTRY, &WORKSHOP];

/// TK-06. A second compiled family, one no build registers, lists on the rail under its own
/// title after the registry's tables, each of its sheets named apart from the registry's: its
/// `plan` is not the registry's `plan`, and a reference to it offers its own rows. A row added
/// on it is written to that family, not the registry, by one save its own rules judge: a gadget
/// its rule refuses is refused in its words, and nothing of it is written.
/// NEGATIVE CONTROLS, run once each and observed: with `tables_store::write` saving every
/// staged table through the registry (`Handle::at(paths.clone(), &aterm_db::att::REGISTRY)` in
/// place of the staged generation), the plan is never written: `` the workshop holds the plan
/// left: None right: Some("assembly") ``; with `refers_to` (native_tables_rows.rs) reading the
/// compiled `Ty::Ref` and not the column's link, the gadget's chosen plan is checked against the
/// registry's plans, refused, and the Write held back: `` a Write is out: Gadgets held back: 1
/// refused cell ``.
#[test]
fn a_second_compiled_family_lists_on_the_rail_and_saves() {
    use crate::native_tables::{WRITE, rows};
    use aterm_db::att::Handle;
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    aterm_db::file::fixture::declare(paths, &WORKSHOP);
    let registry_writes = || {
        Handle::at(paths.clone(), &aterm_db::att::REGISTRY)
            .history()
            .unwrap()
            .len()
    };
    let before = registry_writes();
    let mut app = App::headless_for_test();
    app.open_tables_tab_over(Some(paths.clone()), &TWO_FAMILIES)
        .unwrap();
    let (plans, gadgets) = ("workshop.plan", "workshop.gadget");
    let pending = render(&app);
    let gadget_key = format!("tables/sheet/{gadgets}");
    assert!(said(&pending, &gadget_key).is_some_and(|label| label.contains("rows unread")));
    act(&mut app, &gadget_key, None);
    assert_eq!(
        said(&render(&app), "tables/title").as_deref(),
        Some("Gadgets")
    );
    settle(&mut app);

    // The rail: each family under its title, the workshop's after every registry table, and
    // its sheets named apart from the registry's, its `plan` among them.
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/title").as_deref(), Some("Gadgets"));
    let y = |key: &str| {
        ui.semantic(&crate::native_ui::UiKey::new(key.to_owned()))
            .map(|node| node.rect.y)
            .unwrap_or_else(|| panic!("the rail shows {key}"))
    };
    let tables = app.native_runtime.tables_mut().unwrap();
    let names = tables.listed_names();
    assert_eq!(
        names[names.len() - 3..],
        [plans, gadgets, "workshop.machine"],
        "{names:?}"
    );
    assert_eq!(tables.title_of(gadgets), Some("Gadgets"));
    let registry_tables = &names[..names.len() - 3];
    assert!(registry_tables.contains(&"plan"), "{registry_tables:?}");
    let workshop = y("tables/rail/family/Workshop");
    assert!(y("tables/rail/family/Registry") < y("tables/sheet/plan"));
    for name in registry_tables {
        assert!(y(&format!("tables/sheet/{name}")) < workshop, "{name}");
    }
    assert!(workshop < y("tables/rail/group/Workshop/STORED"));
    assert!(y("tables/rail/group/Workshop/STORED") < y(&format!("tables/sheet/{plans}")));
    assert!(
        said(&ui, &format!("tables/sheet/{gadgets}"))
            .is_some_and(|said| said.starts_with("Gadgets")),
        "the rail names the workshop's table"
    );

    // A plan added on the workshop's sheet is written to the workshop, by its own birth rule.
    act(&mut app, &format!("tables/sheet/{plans}"), None);
    act(&mut app, rows::ADD, None);
    type_in(&mut app, &format!("{}1", rows::NEW), "name", "assembly");
    assert_eq!(
        said(&render(&app), "tables/bar/count").as_deref(),
        Some("1 change not written")
    );
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    let workshop = Handle::at(paths.clone(), &WORKSHOP);
    let held = |collection: &'static aterm_db::typed::Collection, pk: &str, column: &str| {
        workshop
            .view()
            .unwrap()
            .project(collection)
            .rows
            .get(pk)
            .and_then(|row| row.fields.get(column))
            .and_then(|field| field.value())
            .and_then(aterm_json::Value::as_str)
            .map(str::to_owned)
    };
    assert_eq!(
        held(&WORKSHOP_PLAN, "assembly", "name").as_deref(),
        Some("assembly"),
        "the workshop holds the plan"
    );

    // A gadget's plan offers the workshop's plans, never the registry's, and is written there.
    act(&mut app, &format!("tables/sheet/{gadgets}"), None);
    act(&mut app, rows::ADD, None);
    let new = format!("{}2", rows::NEW);
    type_in(&mut app, &new, "name", "sprocket");
    open(&mut app, &new, "plan");
    let ui = render(&app);
    let offered: Vec<String> = (0..)
        .map_while(|n| said(&ui, &format!("{}{n}", crate::native_tables_choices::CHOICE)))
        .collect();
    // The workshop's one plan, then empty (the reference may be), marked as the cell holds.
    assert_eq!(
        offered,
        ["assembly", "empty, marked"],
        "the workshop's plans"
    );
    act(
        &mut app,
        &format!("{}0", crate::native_tables_choices::CHOICE),
        None,
    );
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    assert_eq!(
        held(&GADGET, "sprocket", "plan").as_deref(),
        Some("assembly"),
        "the workshop holds the gadget, made to its plan"
    );
    assert_eq!(registry_writes(), before, "the registry wrote nothing");
    assert_eq!(cell_says(&render(&app), "sprocket", "plan"), "assembly");

    // A save the workshop's own rules refuse is refused in their words, and writes nothing.
    act(&mut app, rows::ADD, None);
    let new = format!("{}3", rows::NEW);
    type_in(&mut app, &new, "name", FORBIDDEN);
    let writes = workshop.history().unwrap().len();
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    let rule = app
        .native_runtime
        .tables_mut()
        .unwrap()
        .refusal(&(gadgets.to_owned(), new.clone(), "name".to_owned()))
        .cloned()
        .unwrap_or_default();
    assert!(rule.contains(WORKSHOP_RULE), "{rule}");
    assert_eq!(
        workshop.history().unwrap().len(),
        writes,
        "nothing was written"
    );
    assert_eq!(held(&GADGET, FORBIDDEN, "name"), None);
    assert_opening_workshop(paths);
}

/// A reference to a workshop machine uses its catalog title. Two tools of the same model
/// remain distinct by inventory key in the chooser, grid and history.
#[test]
fn workshop_machine_references_use_the_workshops_row_names() {
    use crate::native_tables::{Sheet, rows::Targets};
    use aterm_db::att::{Handle, Saved};
    use aterm_db::typed::grid;
    use std::collections::BTreeMap;

    fn row<const N: usize>(fields: [(&str, &str); N]) -> BTreeMap<String, String> {
        fields.map(|(k, v)| (k.to_owned(), v.to_owned())).into()
    }

    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    Registry::at(paths.clone()).open().unwrap();
    aterm_db::file::fixture::declare(paths, &WORKSHOP);
    let workshop = Handle::at(paths.clone(), &WORKSHOP);
    let write = |collection, rows: &[BTreeMap<String, String>]| {
        let base = grid::export(
            &WORKSHOP_TABLE,
            collection,
            &workshop.view().unwrap().cells,
            &[],
        )
        .unwrap()
        .base;
        let text = grid::staged_text(&base, &BTreeMap::new(), rows);
        let saved = workshop.save(collection, &base, &text).unwrap();
        let Saved::Written { applied, .. } = saved else {
            panic!("{saved:?}");
        };
        applied.operations
    };
    write(
        &WORKSHOP_MACHINE,
        &[
            row([
                ("name", "tool-east"),
                ("title", "Lathe"),
                ("label", "Bay A"),
            ]),
            row([
                ("name", "tool-west"),
                ("title", "Lathe"),
                ("label", "Bay B"),
            ]),
        ],
    );
    let operations = write(
        &GADGET,
        &[row([("name", "spindle"), ("tool", "tool-east")])],
    );
    let [operation] = operations.as_slice() else {
        panic!("expected one signed gadget write");
    };
    let check = |sheet: &Sheet, targets: &Targets| {
        let tools = &targets["workshop.machine"];
        assert_eq!(tools.order, ["tool-east", "tool-west"]);
        for (key, ending) in [("tool-east", "east"), ("tool-west", "west")] {
            let named = &tools.rows[key];
            assert_eq!(named.words, "Lathe");
            assert_eq!(named.short, format!("Lathe · …{ending}"));
            assert_eq!(named.said, format!("{} · live", named.short));
        }
        let [gadget] = sheet.rows.as_slice() else {
            panic!("expected the spindle");
        };
        let tool = &gadget.cells["tool"];
        assert_eq!(tool.refers.as_deref(), Some("tool-east"));
        assert_eq!(tool.text, tools.rows["tool-east"].said);
        let [record] = sheet.written.as_slice() else {
            panic!("expected the gadget's signed operation");
        };
        assert_eq!(&record.operation, operation);
        assert_eq!(tool.by.as_ref(), Some(&record.by));
        let change = record.cells.iter().find(|c| c.column == "tool").unwrap();
        assert_eq!(change.now, tools.rows["tool-east"].short);
    };

    let (snapshot, _) = super::read(paths, &TWO_FAMILIES, None, false).unwrap();
    check(
        snapshot
            .sheets
            .iter()
            .find(|s| s.name == "workshop.gadget")
            .unwrap(),
        &snapshot.targets,
    );

    let mut published = false;
    super::read_opening(
        paths,
        &TWO_FAMILIES,
        &mut super::Opening {
            wanted: &|| Some("workshop.gadget"),
            publish: &mut |sheet, targets| {
                if sheet.name == "workshop.gadget" {
                    check(&sheet, &targets);
                    published = true;
                }
            },
        },
    )
    .unwrap();
    assert!(published);
}

/// Start on a non-registry family and change the selected collection while the reader is
/// publishing. Both early sheets must contain the same rows and references as a settled read.
fn assert_opening_workshop(paths: &super::Paths) {
    use std::cell::Cell;
    use std::collections::BTreeSet;

    let (expected, whole) = super::read(paths, &TWO_FAMILIES, None, false).unwrap();
    let wanted = Cell::new("workshop.gadget");
    let mut published = Vec::new();
    let (tail, opened) = super::read_opening(
        paths,
        &TWO_FAMILIES,
        &mut super::Opening {
            wanted: &|| Some(wanted.get()),
            publish: &mut |sheet, targets| {
                if sheet.name == "workshop.gadget" {
                    assert_eq!(targets["workshop.plan"].rows["assembly"].short, "assembly");
                    wanted.set("workshop.plan");
                }
                published.push(sheet);
            },
        },
    )
    .unwrap();
    assert_eq!(opened, whole);
    assert_eq!(published.first().unwrap().name, "workshop.gadget");
    assert_eq!(published.get(1).unwrap().name, "workshop.plan");
    assert_eq!(tail.listed, expected.listed);
    assert_eq!(tail.targets, expected.targets);

    let mut seen = BTreeSet::new();
    for sheet in published.iter().chain(&tail.sheets) {
        assert!(seen.insert(sheet.name), "{} arrived twice", sheet.name);
        let other = expected
            .sheets
            .iter()
            .find(|s| s.name == sheet.name)
            .unwrap();
        assert_eq!(
            sheet.rows, other.rows,
            "values and writer names in {}",
            sheet.name
        );
        assert_eq!(sheet.written, other.written, "history of {}", sheet.name);
        assert_eq!(
            sheet.columns, other.columns,
            "reference columns of {}",
            sheet.name
        );
        assert_eq!(
            sheet.observed, other.observed,
            "observations of {}",
            sheet.name
        );
        assert_eq!(sheet.referrers, other.referrers, "links to {}", sheet.name);
        assert_eq!(sheet.base, other.base, "edit baseline of {}", sheet.name);
    }
    assert_eq!(seen.len(), expected.sheets.len());

    // Closing the tab on the first published secondary sheet ends the whole read,
    // including the primary family that still has sheets to lay out.
    let selected = Cell::new(Some("workshop.gadget"));
    let mut before_close = Vec::new();
    let closed = super::read_opening(
        paths,
        &TWO_FAMILIES,
        &mut super::Opening {
            wanted: &|| selected.get(),
            publish: &mut |sheet, _| {
                before_close.push(sheet.name);
                selected.set(None);
            },
        },
    );
    assert!(closed.is_err());
    assert_eq!(before_close, ["workshop.gadget"]);
}

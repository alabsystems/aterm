// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the Tables page's read of the registry says of the values aterm sets, on a scratch
//! registry (`aterm_db::table::fixture::Lab`: an isolated home, no live vault, fleet or
//! Commons). Every value is synthetic.

use crate::App;
use crate::native_app::TextInputEvent;
use crate::native_tables::native_tables_tests::{
    act, cell_says, key_in, put_in, render, said, settle,
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
    let first = super::history(paths, &asked, &mut kept).unwrap();
    assert_eq!(
        kept.revision().map(|revision| revision.0),
        Some(first.revision),
        "the first read keeps the registry's order"
    );
    assert_eq!(super::mark(paths).unwrap().digest(), first.revision);
    assert_eq!(kept.reads(), 1);
    let older = Asked {
        before: first.next,
        revision: Some(first.revision),
        ..asked.clone()
    };
    let read_older = super::history(paths, &older, &mut kept).unwrap();
    assert_eq!(
        kept.reads(),
        1,
        "Read older at the held revision read the registry again"
    );
    assert!(!read_older.moved);
    assert_eq!(read_older.records.len(), 2);
    assert_eq!(
        read_older,
        super::history(paths, &older, &mut super::Kept::default()).unwrap(),
        "kept or not, one page"
    );

    here.write(vec![label("mc-0000000000000006", "m6")])
        .unwrap();
    let moved = super::history(paths, &older, &mut kept).unwrap();
    assert!(moved.moved, "asked at the old revision");
    assert_ne!(moved.revision, first.revision);
    assert_eq!(
        kept.revision().map(|revision| revision.0),
        Some(moved.revision)
    );
    assert_eq!(kept.reads(), 2);
    assert_eq!(moved.records[0].cells[0].pk, "mc-0000000000000006");
}

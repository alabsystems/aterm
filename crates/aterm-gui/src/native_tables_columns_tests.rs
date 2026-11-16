// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's columns (docs/PLAN.md LT-40) on hand-made reads and on a scratch registry
//! (`aterm_db::table::fixture::Lab`: an isolated home, no live vault, fleet or Commons): which
//! columns the grid draws, the fold of the empty ones, and each column's fill. Every value is
//! synthetic.

use super::super::native_tables_tests::{
    MC, WID, act, cell_says, key_with, one_sheet, open_with, pk_of, place, point_at, presented,
    press_key, put_in, render, render_wide, said, settle, settle_write, shipped_themes, tall_sheet,
    tree, two_macs, view_of, window_px, with_view_mut,
};
use super::super::*;
use super::*;
use crate::App;
use crate::native_ui::{CompiledUi, LogicalRect};
use aterm_db::alab::Registry;
use aterm_db::alab::registry::{ADDRESS, MACHINE};
use aterm_db::table::fixture::repair_commons;
use aterm_json::json;
use aterm_types::keyboard::{Key, Modifiers, NamedKey};
use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta};

/// The linked count's column of [`machines`].
const LINKED: &str = "~disk.machine";

fn column(name: &str, group: Group, editable: bool) -> SheetColumn {
    SheetColumn {
        name: name.into(),
        label: crate::tables_store::label_of(name),
        group,
        editable,
        kind: ColumnKind::Text,
        links: None,
        compiled: None,
        key: false,
    }
}

fn text(words: &str) -> Shown {
    Shown {
        text: words.into(),
        ..Shown::default()
    }
}

/// Three machines under seven columns: `label` the key; `chip`, which two of them hold;
/// `nickname` (an empty cell in each row) and `notes` (no cell at all) under STORED; `status`,
/// which each holds, and `seen`, which none does, under SET BY ATERM; and a count of the disks
/// that name each, none.
fn machines() -> Snapshot {
    let mut snapshot = one_sheet(Shown::default());
    let sheet = &mut snapshot.sheets[0];
    sheet.key = "label".into();
    let mut linked = column(LINKED, Group::Linked, false);
    linked.label = "DISKS \u{b7} MACHINE".into();
    linked.kind = ColumnKind::Number;
    linked.links = Some("disk".into());
    sheet.columns = vec![
        column("label", Group::Stored, true),
        column("chip", Group::Stored, true),
        column("nickname", Group::Stored, true),
        column("notes", Group::Stored, true),
        column("status", Group::SetByAterm, false),
        column("seen", Group::SetByAterm, false),
        linked,
    ];
    sheet.rows = (0..3)
        .map(|n| {
            let mut cells = BTreeMap::new();
            cells.insert("label".to_owned(), text(&format!("m{n}")));
            cells.insert("chip".to_owned(), text(["M2", "M4", ""][n]));
            cells.insert("nickname".to_owned(), text(""));
            cells.insert("status".to_owned(), text("active"));
            cells.insert(LINKED.to_owned(), text(""));
            SheetRow {
                pk: pk_of(n),
                cells,
            }
        })
        .collect();
    snapshot
}

/// [`machines`] beside a second table of the same columns, `disk`.
fn machines_and_disks() -> Snapshot {
    let mut snapshot = machines();
    let mut disks = snapshot.sheets[0].clone();
    disks.name = "disk";
    disks.title = "Disks";
    disks.singular = "Disk";
    snapshot.sheets.push(disks);
    snapshot.listed.push(crate::tables_store::Listed {
        name: "disk",
        title: "Disks",
        singular: "Disk",
        face: aterm_db::typed::Face::Owner,
        owned: false,
    });
    snapshot
}

/// The fill a column's header says in its name, after what its press does.
fn header_fill(ui: &CompiledUi, column: &str) -> Option<String> {
    let said = said(ui, &format!("{HEAD}{column}"))?;
    let (_, fill) = said.rsplit_once(", ")?;
    Some(fill.to_owned())
}

/// The words node `key` paints, whether or not the tree lists it.
fn painted(ui: &CompiledUi, key: &str) -> Option<String> {
    ui.paint
        .iter()
        .find(|node| node.key.as_str() == key)
        .and_then(|node| match &node.content {
            UiContent::Text(spec) => Some(spec.text.clone()),
            _ => None,
        })
}

/// Whether the grid draws column `name`: its header is listed.
fn drawn(ui: &CompiledUi, name: &str) -> bool {
    said(ui, &format!("{HEAD}{name}")).is_some()
}

/// The selected cell: its row's key and its column, and where the keys are.
fn selected(app: &App) -> (String, String, String) {
    let view = view_of(app);
    (
        view.row.clone().unwrap_or_default(),
        view.column.clone().unwrap_or_default(),
        view.common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_owned())
            .unwrap_or_default(),
    )
}

fn line_of(name: &str) -> String {
    format!("{COLUMN}{name}")
}

/// The empty columns fold into one stub at the end of their group, each header says its fill
/// and wears it as a bar along its foot, and the Columns button counts the columns the grid
/// draws of all the table's. Its list offers every column but the key under its group, each
/// with a mark where the grid draws it and its fill; a press hides a drawn column and shows it
/// again, and an empty one shown so is drawn though the fold is on.
/// NEGATIVE CONTROL: with `fill_bar` drawing each bar the header's whole width (its `share`
/// taken as at least 1), the chip's bar runs across: `` two thirds across: LogicalRect { x:
/// 311.0, y: 182.0, width: 63.0, height: 2.0 } in LogicalRect { x: 311.0, y: 157.0, width:
/// 64.0, height: 28.0 } ``.
#[test]
fn a_column_is_hidden_and_shown_and_empty_ones_fold_and_each_says_its_fill() {
    let mut app = open_with(machines());
    let ui = render(&app);
    for name in ["label", "chip", "status"] {
        assert!(drawn(&ui, name), "{name} is drawn");
    }
    for name in ["nickname", "notes", "seen", LINKED] {
        assert!(!drawn(&ui, name), "{name} folds");
        assert!(
            said(&ui, &format!("{CELL}{}/{name}", pk_of(0))).is_none(),
            "no cell of {name} is listed"
        );
    }
    assert_eq!(
        said(&ui, "tables/fold/stored").as_deref(),
        Some("2 empty columns folded under Stored; show them")
    );
    assert_eq!(
        said(&ui, "tables/fold/aterm").as_deref(),
        Some("1 empty column folded under Set by aterm; show it")
    );
    assert_eq!(
        said(&ui, "tables/fold/linked").as_deref(),
        Some("1 empty column folded under Linked rows; show it")
    );
    assert_eq!(
        painted(&ui, "tables/fold/stored/word").as_deref(),
        Some("2 EMPTY")
    );
    // The stub stands after its group's last drawn column, before the next group's first.
    let x = |key: &str| place(&ui, key).x;
    assert!(x("tables/head/chip/seat") < x("tables/fold/stored/seat"));
    assert!(x("tables/fold/stored/seat") < x("tables/head/status/seat"));
    // Each header says its fill in its name, after what its press does, and wears it as a bar
    // along its foot.
    assert_eq!(
        said(&ui, "tables/head/chip").as_deref(),
        Some("Sort by Chip, 2 of 3 rows hold a value")
    );
    assert_eq!(
        said(&ui, "tables/head/status").as_deref(),
        Some("Sort by Status, 3 of 3 rows hold a value")
    );
    let seat = place(&ui, "tables/head/chip/seat");
    let bar = place(&ui, "tables/head/chip/fill/bar");
    assert!(
        (bar.width - (seat.width - 1.0) * 2.0 / 3.0).abs() < 0.5,
        "two thirds across: {bar:?} in {seat:?}"
    );
    assert!(
        (bar.height - FILL_BAR).abs() < 0.01 && bar.y + bar.height <= seat.y + seat.height,
        "along the header's foot: {bar:?} in {seat:?}"
    );
    assert_eq!(
        said(&ui, COLUMNS).as_deref(),
        Some("Columns 3 of 7 shown; choose the columns shown")
    );

    // The list: every column but the key, under its group, with its mark and its fill.
    act(&mut app, COLUMNS, None);
    let ui = render(&app);
    assert!(
        said(&ui, &line_of("label")).is_none(),
        "the key column is not offered"
    );
    assert_eq!(
        said(&ui, &line_of("chip")).as_deref(),
        Some("Chip, shown, 2 of 3 rows hold a value")
    );
    assert_eq!(
        said(&ui, &line_of("nickname")).as_deref(),
        Some("Nickname, folded, 0 of 3 rows hold a value")
    );
    assert_eq!(
        painted(&ui, &format!("{}/fill", line_of("chip"))).as_deref(),
        Some("2 of 3")
    );
    assert_eq!(
        painted(&ui, &format!("{}/mark", line_of("chip"))).as_deref(),
        Some("\u{2713}")
    );
    assert_ne!(
        painted(&ui, &format!("{}/mark", line_of("seen"))).as_deref(),
        Some("\u{2713}")
    );
    assert_eq!(
        painted(&ui, "tables/columns/head/count").as_deref(),
        Some("3 of 7 shown")
    );
    assert_eq!(
        painted(&ui, "tables/columns/group/stored/word").as_deref(),
        Some("STORED")
    );
    assert_eq!(
        painted(&ui, "tables/columns/group/stored/note").as_deref(),
        Some("rows with a value")
    );
    let order: Vec<f32> = ["chip", "nickname", "notes", "status", "seen", LINKED]
        .iter()
        .map(|name| place(&ui, &line_of(name)).y)
        .collect();
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "in the grid's order: {order:?}"
    );

    // A press hides a drawn column, and shows it again.
    act(&mut app, &line_of("chip"), None);
    let ui = render(&app);
    assert!(!drawn(&ui, "chip"), "the chip is hidden");
    assert_eq!(
        said(&ui, &line_of("chip")).as_deref(),
        Some("Chip, hidden, 2 of 3 rows hold a value")
    );
    assert_eq!(
        said(&ui, COLUMNS).as_deref(),
        Some("Columns 2 of 7 shown; close the list of columns")
    );
    act(&mut app, &line_of("chip"), None);
    assert!(drawn(&render(&app), "chip"), "the chip is shown again");
    // An empty column shown from the list is drawn though the fold is on.
    act(&mut app, &line_of("seen"), None);
    let ui = render(&app);
    assert!(drawn(&ui, "seen"));
    assert!(
        said(&ui, "tables/fold/aterm").is_none(),
        "its group folds nothing now"
    );
    assert!(!drawn(&ui, "nickname"), "the other groups still fold");
}

/// A column's fill counts the rows the lifecycle filter shows of the whole table: the find
/// narrows the rows shown and leaves every fill as it was, and each read counts again. Under the
/// live rows a retired row counts nowhere, and the columns only it fills fold; once the filter
/// shows the retired rows the count reads against them, and against every row once it shows all.
/// NEGATIVE CONTROLS: with `TablesViewState::fills` keeping its counts across reads (its cache
/// keyed by the sheet's name alone, `app.reads` made `0`), the read that fills the chip is not
/// counted: `` a read counts again left: Some("2 of 3 rows hold a value") right: Some("3 of 3
/// rows hold a value") ``; with it counting every row of the sheet (its `life_shows` filter
/// removed), the retired machine's nickname keeps the column open under the live rows: ``
/// no live row fills the nickname: it folds ``.
#[test]
fn a_columns_fill_counts_the_rows_the_filter_shows_whatever_the_find_shows() {
    let mut app = open_with(machines());
    act(&mut app, FIND, Some(SemanticInput::Text("m1".into())));
    let ui = render(&app);
    assert!(
        said(&ui, &format!("{CELL}{}/label", pk_of(0))).is_none(),
        "the find leaves the first row out"
    );
    assert_eq!(
        header_fill(&ui, "chip").as_deref(),
        Some("2 of 3 rows hold a value")
    );
    // A read that fills the third machine's chip counts it at once.
    let mut filled = machines();
    filled.sheets[0].rows[2]
        .cells
        .insert("chip".to_owned(), text("M1"));
    app.native_runtime.tables_mut().unwrap().show(filled);
    assert_eq!(
        header_fill(&render(&app), "chip").as_deref(),
        Some("3 of 3 rows hold a value"),
        "a read counts again"
    );

    // On a scratch registry, a second machine's nickname, then the machine retired.
    let lab = two_macs();
    let here = Registry::at(lab.paths[0].clone());
    const OTHER: &str = "mc-000000000000000b";
    here.write(vec![vec![
        put_in(&MACHINE, OTHER, "label", json!("m55")),
        put_in(&MACHINE, OTHER, "status", json!("active")),
        put_in(&MACHINE, OTHER, "nickname", json!("Spare")),
        put_in(&MACHINE, OTHER, "row", json!("live")),
    ]])
    .unwrap();
    repair_commons(&lab, 4);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let ui = render(&app);
    assert_eq!(
        header_fill(&ui, "nickname").as_deref(),
        Some("1 of 2 rows holds a value")
    );
    here.write(vec![vec![put_in(&MACHINE, OTHER, "row", json!("retired"))]])
        .unwrap();
    repair_commons(&lab, 4);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    let ui = render(&app);
    assert!(
        !drawn(&ui, "nickname"),
        "no live row fills the nickname: it folds"
    );
    act(&mut app, COLUMNS, None);
    assert_eq!(
        said(&render(&app), &line_of("nickname")).as_deref(),
        Some("Nickname, folded, 0 of 1 row holds a value")
    );
    // The retired rows: the retired machine alone shows, and it fills the nickname.
    act(&mut app, rows::FILTER, None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, rows::FILTER).as_deref(),
        Some("Retired rows shown; press for all rows")
    );
    assert!(drawn(&ui, "nickname"), "the retired row fills it");
    assert_eq!(
        header_fill(&ui, "nickname").as_deref(),
        Some("1 of 1 row holds a value")
    );
    act(&mut app, rows::FILTER, None);
    assert_eq!(
        header_fill(&render(&app), "nickname").as_deref(),
        Some("1 of 2 rows holds a value"),
        "all rows"
    );
}

/// A table with no rows folds nothing: every column stands open, saying what a row would hold,
/// and the switch counts none folded.
/// NEGATIVE CONTROL: with `TablesViewState::left_out` folding a table with no rows (its
/// `self.fill_rows(app, sheet) > 0` clause removed), every column folds: `` chip is drawn ``.
#[test]
fn a_table_with_no_rows_folds_nothing() {
    let mut snapshot = machines();
    snapshot.sheets[0].rows.clear();
    let mut app = open_with(snapshot);
    let ui = render(&app);
    for name in ["chip", "nickname", "notes", "status", "seen", LINKED] {
        assert!(drawn(&ui, name), "{name} is drawn");
    }
    assert!(said(&ui, "tables/fold/stored").is_none(), "no stub stands");
    act(&mut app, COLUMNS, None);
    assert_eq!(
        said(&render(&app), FOLD).as_deref(),
        Some("Fold columns no row fills, on, 0 folded")
    );
}

/// The fold is each group's: a press on a group's stub unfolds that group and only it, the
/// list's switch turns the fold off for every group, and turned on again it folds every group's
/// empty columns again. Unfolded, an empty column is drawn as any other.
/// NEGATIVE CONTROL: with a stub's press unfolding every group (`spread` set in place of its
/// group inserted), the SET BY ATERM stub is gone too: `` the other groups stay folded ``.
#[test]
fn the_fold_is_per_group_and_the_switch_turns_it_off() {
    let mut app = open_with(machines());
    act(&mut app, "tables/fold/stored", None);
    let ui = render(&app);
    assert!(drawn(&ui, "nickname") && drawn(&ui, "notes"));
    assert!(said(&ui, "tables/fold/stored").is_none());
    assert_eq!(
        said(&ui, &format!("{CELL}{}/nickname", pk_of(1))).as_deref(),
        Some(format!("NICKNAME of {}: empty", pk_of(1)).as_str()),
        "an unfolded empty column's cells are ordinary"
    );
    assert!(
        said(&ui, "tables/fold/aterm").is_some() && !drawn(&ui, "seen"),
        "the other groups stay folded"
    );
    // The switch: off, every column is drawn and no stub stands.
    act(&mut app, COLUMNS, None);
    assert_eq!(
        said(&render(&app), FOLD).as_deref(),
        Some("Fold columns no row fills, on, 2 folded")
    );
    act(&mut app, FOLD, None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, FOLD).as_deref(),
        Some("Fold columns no row fills, off")
    );
    for name in ["nickname", "notes", "seen", LINKED] {
        assert!(drawn(&ui, name), "{name} is drawn");
    }
    assert!(said(&ui, "tables/fold/aterm").is_none() && said(&ui, "tables/fold/linked").is_none());
    // On again, every group folds again.
    act(&mut app, FOLD, None);
    let ui = render(&app);
    assert!(
        !drawn(&ui, "nickname") && said(&ui, "tables/fold/stored").is_some(),
        "on again, every group folds again"
    );
    assert_eq!(
        said(&ui, FOLD).as_deref(),
        Some("Fold columns no row fills, on, 4 folded")
    );
    // Show all shows every column and turns the fold off.
    act(&mut app, &line_of("chip"), None);
    act(&mut app, ALL, None);
    let ui = render(&app);
    for name in ["chip", "nickname", "notes", "seen", LINKED] {
        assert!(drawn(&ui, name), "{name} is drawn");
    }
}

/// Copy, a range and Tab's path take the columns the grid draws alone: a folded or hidden column
/// is passed over by Right, by Tab and by a range, and copies nothing.
/// NEGATIVE CONTROL: with `TablesApp::selection` naming the range's columns among every column
/// (`ordered` in place of `shown_columns`), the copy takes the folded nickname's empty cells in
/// the status's place: `` left: Some("m0\tM2\t\nm1\tM4\t") right:
/// Some("m0\tM2\tactive\nm1\tM4\tactive") ``.
#[test]
fn hidden_and_folded_columns_leave_copy_a_range_and_tabs_path() {
    let none = Modifiers::empty();
    let mut app = open_with(machines());
    act(&mut app, &format!("{CELL}{}/label", pk_of(0)), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), none);
    assert_eq!(selected(&app).1, "chip");
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), none);
    assert_eq!(
        selected(&app).1,
        "status",
        "Right passes the folded columns over"
    );
    // Tab from the last column drawn goes on to the next row's first.
    key_with(&mut app, Key::Named(NamedKey::Tab), none);
    assert_eq!(
        (selected(&app).0, selected(&app).1),
        (pk_of(1), "label".to_owned())
    );
    key_with(&mut app, Key::Named(NamedKey::Tab), Modifiers::SHIFT);
    assert_eq!(
        (selected(&app).0, selected(&app).1),
        (pk_of(0), "status".to_owned())
    );
    // A range from the label two columns across and a row down holds the drawn columns alone.
    act(&mut app, &format!("{CELL}{}/label", pk_of(0)), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some("m0\tM2\tactive\nm1\tM4\tactive")
    );
    // Hidden, the chip leaves the range, the copy and the arrows' path too.
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("chip"), None);
    act(&mut app, COLUMNS, None);
    act(&mut app, &format!("{CELL}{}/label", pk_of(0)), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), Modifiers::SHIFT);
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    assert_eq!(
        app.native_selection_text(WID).as_deref(),
        Some("m0\tactive\nm1\tactive")
    );
    act(&mut app, &format!("{CELL}{}/label", pk_of(2)), None);
    key_with(&mut app, Key::Named(NamedKey::ArrowRight), none);
    assert_eq!(selected(&app).1, "status");
}

/// A column holding a change not written is never folded or hidden while it is: an empty
/// column with a staged cell is drawn though the fold is on, and its list line says it has a
/// change not written and takes no press. Once the change is undone it folds again.
/// NEGATIVE CONTROL: with `TablesViewState::kept` blind to staged cells (its `app.staged` clause
/// answering `false`), the staged nickname folds: `` the staged column is drawn ``.
#[test]
fn a_staged_cells_column_is_never_folded_or_hidden() {
    let mut app = open_with(machines());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(1), "nickname", "Desk".into());
    let ui = render(&app);
    assert!(drawn(&ui, "nickname"), "the staged column is drawn");
    assert_eq!(
        painted(&ui, "tables/fold/stored/word").as_deref(),
        Some("1 EMPTY"),
        "the notes alone fold"
    );
    act(&mut app, COLUMNS, None);
    let ui = render(&app);
    let line = ui
        .semantic(&UiKey::new(line_of("nickname")))
        .expect("the nickname's line is listed");
    assert_eq!(
        line.label,
        "Nickname, shown, has a change not written, 0 of 3 rows hold a value"
    );
    assert_eq!(line.state.map(|state| state.enabled), Some(false));
    assert_eq!(
        painted(&ui, &format!("{}/note", line_of("nickname"))).as_deref(),
        Some("has a change not written")
    );
    act(&mut app, &line_of("nickname"), None);
    act(&mut app, FOLD, None);
    act(&mut app, FOLD, None);
    assert!(
        drawn(&render(&app), "nickname"),
        "neither the hide nor the fold takes it"
    );
    app.native_runtime.tables_mut().unwrap().unstage(&(
        "machine".into(),
        pk_of(1),
        "nickname".into(),
    ));
    // A press elsewhere closes the list; the keys leave the staged column's line.
    act(&mut app, &format!("{CELL}{}/chip", pk_of(0)), None);
    assert!(
        !drawn(&render(&app), "nickname"),
        "undone, the empty column folds again"
    );
}

/// While a new row of the table stands, each column it is typed into is drawn though no written
/// row fills it, so the person sees where its values go, and one it has a value typed in holds a
/// change not written, which is never hidden. A column no new row is typed into stays folded.
/// The new row, not written, is no row a fill counts, and its typed cells fill nothing. Taken
/// back, its columns fold again.
/// NEGATIVE CONTROL: with `TablesViewState::kept` not keeping a new row's columns from the fold
/// (its `app.born` clause removed), the empty nickname stays folded: `` the new row's nickname
/// is drawn ``.
#[test]
fn a_new_rows_columns_are_drawn_while_it_stands() {
    let mut app = open_with(with_life(machines(), None));
    let ui = render(&app);
    assert!(
        !drawn(&ui, "nickname") && !drawn(&ui, "notes"),
        "the empty columns fold"
    );
    act(&mut app, rows::ADD, None);
    let first = format!("{}1", rows::NEW);
    let ui = render(&app);
    assert!(
        said(&ui, &format!("{CELL}{first}/label")).is_some(),
        "the new row stands"
    );
    for name in ["chip", "nickname", "notes"] {
        assert!(drawn(&ui, name), "the new row's {name} is drawn");
    }
    assert!(
        !drawn(&ui, "seen"),
        "a column no new row is typed into stays folded"
    );
    assert_eq!(
        header_fill(&ui, "chip").as_deref(),
        Some("2 of 3 rows hold a value"),
        "a new row, not written, is no row a fill counts"
    );
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &first, "nickname", "Desk".into());
    act(&mut app, COLUMNS, None);
    assert_eq!(
        said(&render(&app), &line_of("nickname")).as_deref(),
        Some("Nickname, shown, has a change not written, 0 of 3 rows hold a value")
    );
    act(&mut app, COLUMNS, None);
    act(&mut app, DISCARD, None);
    let ui = render(&app);
    assert!(
        said(&ui, &format!("{CELL}{first}/label")).is_none(),
        "the new row is taken back"
    );
    assert!(
        !drawn(&ui, "nickname") && !drawn(&ui, "notes"),
        "its columns fold again"
    );
}

/// A reference's fill counts the rows it is set in, each of which shows the row it names by its
/// words: an address that is no alias is empty in `alias_of`. A reference no row sets folds as any
/// empty column does.
/// NEGATIVE CONTROL: with the fill counting each row whose cell the read lists (`is_some()`, its
/// value aside), the address that is no alias counts: `` left: Some("2 of 2 rows hold a value")
/// right: Some("1 of 2 rows holds a value") ``.
#[test]
fn a_references_fill_counts_the_rows_it_is_set_in() {
    let lab = two_macs();
    let here = Registry::at(lab.paths[0].clone());
    let (owner, agents) = ("ad-0000000000000001", "ad-0000000000000002");
    here.write(vec![vec![
        put_in(&ADDRESS, owner, "addr", json!("owner@example.invalid")),
        put_in(&ADDRESS, owner, "domain", json!("example.invalid")),
        put_in(&ADDRESS, owner, "kind", json!("standalone")),
        put_in(&ADDRESS, owner, "row", json!("live")),
        put_in(&ADDRESS, agents, "addr", json!("agents@example.invalid")),
        put_in(&ADDRESS, agents, "domain", json!("example.invalid")),
        put_in(&ADDRESS, agents, "kind", json!("alias")),
        put_in(&ADDRESS, agents, "alias_of", json!(owner)),
        put_in(&ADDRESS, agents, "row", json!("live")),
    ]])
    .unwrap();
    repair_commons(&lab, 4);
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/address", None);
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, agents, "alias_of"),
        "owner@example.invalid",
        "the reference shows the row it names"
    );
    assert_eq!(
        header_fill(&ui, "alias_of").as_deref(),
        Some("1 of 2 rows holds a value")
    );
    assert!(!drawn(&ui, "phone"), "no address names a phone: it folds");
    act(&mut app, COLUMNS, None);
    assert_eq!(
        said(&render(&app), &line_of("phone")).as_deref(),
        Some("Phone, folded, 0 of 2 rows hold a value")
    );
}

/// A selected cell whose column a person hides moves to the next column the grid draws in its
/// row, else the one before; where the keys were on the cell they go with it. A press naming a
/// hidden column's cell (a control client's) selects the column the grid draws in its place.
/// NEGATIVE CONTROL: with `TablesApp::update` not settling the columns (its `settle_columns`
/// call removed), the selection stays on the hidden chip: `` left: ("mc-0000000000000001",
/// "chip") right: ("mc-0000000000000001", "status") ``.
#[test]
fn a_selected_cell_moves_off_a_column_a_person_hides() {
    let mut app = open_with(machines());
    let cell = |n: usize, column: &str| format!("{CELL}{}/{column}", pk_of(n));
    act(&mut app, &cell(1, "chip"), None);
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("chip"), None);
    let (pk, column, focus) = selected(&app);
    assert_eq!(
        (pk.as_str(), column.as_str()),
        (pk_of(1).as_str(), "status")
    );
    assert_eq!(focus, line_of("chip"), "the keys stay on the list");
    // The last column drawn hidden, the selection moves to the one before it.
    act(&mut app, &cell(1, "label"), None);
    act(&mut app, &cell(1, "status"), None);
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("status"), None);
    assert_eq!(selected(&app).1, "label");
    // With the keys on the cell, they go with the selection.
    act(&mut app, COLUMNS, None);
    act(&mut app, &cell(0, "status"), None);
    let (pk, column, focus) = selected(&app);
    assert_eq!((pk, column), (pk_of(0), "label".to_owned()));
    assert_eq!(focus, cell(0, "label"));
}

/// Each table keeps its own choice while the tab lives: a column hidden on one is drawn on
/// another of the same columns, and is still hidden on the first once it is chosen again.
/// NEGATIVE CONTROL: with `TablesViewState::left_out` reading the first table's choice for
/// every table (`values().next()` in place of `get(sheet.name)`), the disks lose their chip
/// too: `` another table's chip is drawn ``.
#[test]
fn each_table_keeps_its_own_choice_while_the_tab_lives() {
    let mut app = open_with(machines_and_disks());
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("chip"), None);
    act(&mut app, &format!("{SHEET}disk"), None);
    let ui = render(&app);
    assert!(drawn(&ui, "chip"), "another table's chip is drawn");
    assert!(
        said(&ui, &line_of("chip")).is_none(),
        "choosing a table closes the list"
    );
    act(&mut app, &format!("{SHEET}machine"), None);
    assert!(
        !drawn(&render(&app), "chip"),
        "the machines' chip is still hidden"
    );
}

/// The list takes the keys: it opens with them on its first line, Down and Up move among its
/// lines (passing over one that takes no press), Space and Return press the line with them,
/// and Escape closes it with the keys back on the Columns button. A press elsewhere closes it.
/// Every line is a button that says what it holds.
/// NEGATIVE CONTROL: with the list's Escape leaving it open (its `open_columns(false)`
/// removed), `` Escape closes the list ``.
#[test]
fn the_list_takes_the_arrows_space_return_and_escape() {
    let mut app = open_with(machines());
    act(&mut app, COLUMNS, None);
    assert_eq!(selected(&app).2, FOLD);
    press_key(&mut app, NamedKey::ArrowUp);
    assert_eq!(selected(&app).2, FOLD, "the first line stays the first");
    press_key(&mut app, NamedKey::ArrowDown);
    assert_eq!(selected(&app).2, line_of("chip"));
    let ui = render(&app);
    let line = ui.semantic(&UiKey::new(line_of("chip"))).unwrap();
    assert_eq!(line.role, SemanticRole::Button);
    assert_eq!(
        line.action.as_ref().map(ActionId::as_str),
        Some(line_of("chip").as_str())
    );
    assert!(
        tree_has(&tree(&app), &format!("{}/ring", line_of("chip"))),
        "the keys' ring is on the line"
    );
    press_key(&mut app, NamedKey::Space);
    assert!(!drawn(&render(&app), "chip"), "Space hides it");
    press_key(&mut app, NamedKey::Enter);
    assert!(drawn(&render(&app), "chip"), "Return shows it");
    for _ in 0..10 {
        press_key(&mut app, NamedKey::ArrowDown);
    }
    assert_eq!(selected(&app).2, ALL, "Down ends on Show all");
    press_key(&mut app, NamedKey::Escape);
    assert!(!view_of(&app).columns.open, "Escape closes the list");
    assert_eq!(selected(&app).2, COLUMNS);
    assert!(said(&render(&app), FOLD).is_none());
    // A press elsewhere closes it, and the page takes the press.
    act(&mut app, COLUMNS, None);
    act(&mut app, &format!("{CELL}{}/chip", pk_of(2)), None);
    assert!(!view_of(&app).columns.open);
    assert_eq!(selected(&app).1, "chip");
}

/// Whether the tree holds a node keyed `key`.
fn tree_has(tree: &UiTree, key: &str) -> bool {
    fn find(node: &UiNode, key: &str) -> bool {
        node.key.as_str() == key || node.children.iter().any(|child| find(child, key))
    }
    find(&tree.root, key)
}

/// [`machines`] with ten more stored columns every row fills, and thirty machines more like the
/// first, so the grid's header and rows run under the whole of the open list.
fn machines_wide() -> Snapshot {
    let mut snapshot = machines();
    let sheet = &mut snapshot.sheets[0];
    let first = sheet.rows[0].clone();
    sheet.rows.extend((3..33).map(|n| SheetRow {
        pk: pk_of(n),
        ..first.clone()
    }));
    for n in 1..=10 {
        let name = format!("f{n}");
        sheet.columns.insert(4, column(&name, Group::Stored, true));
        for row in &mut sheet.rows {
            row.cells.insert(name.clone(), text("a value"));
        }
    }
    snapshot
}

/// The stored rows the grid shows, top down, read from their numbers.
fn rows_in_order(app: &App, rows: usize) -> Vec<String> {
    let ui = render(app);
    let mut shown: Vec<(usize, String)> = (0..rows)
        .filter_map(|n| {
            let line = said(&ui, &format!("tables/num/{}/n", pk_of(n)))?;
            Some((line.parse().expect("a row number"), pk_of(n)))
        })
        .collect();
    shown.sort_unstable();
    shown.into_iter().map(|(_, pk)| pk).collect()
}

/// Press and release the pointer's left button where it is.
fn click(app: &mut App) {
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
}

/// A folded group's stub goes once a press unfolds its group, and the keys never stay on it:
/// pressed from the keyboard they go to the header of the first column it showed, after a
/// pointer's press to the selected cell. Either way they rest on a control the page draws, so
/// Return with a change waiting presses that control, never the page's default button, Write.
/// NEGATIVE CONTROL: with the stub's press leaving the keys where they were (its `match` on
/// where they go removed), they rest on the stub the page no longer draws: `` left:
/// "tables/fold/stored" right: "tables/head/nickname" ``.
#[test]
fn a_press_on_a_folded_stub_leaves_the_keys_on_a_control_still_drawn() {
    let mut app = open_with(machines());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(1), "chip", "M9".into());
    let writing = |app: &mut App| app.native_runtime.tables_mut().unwrap().writing;
    let staged = |app: &mut App| app.native_runtime.tables_mut().unwrap().staged.len();
    // The keyboard reached STORED's stub, and Return presses it.
    with_view_mut(&mut app, |view| {
        view.common.last_focus = Some(UiKey::new("tables/fold/stored"));
        view.common.focus_visible = true;
    });
    presented(&mut app);
    press_key(&mut app, NamedKey::Enter);
    let ui = presented(&mut app);
    assert!(said(&ui, "tables/fold/stored").is_none(), "STORED unfolded");
    let focus = view_of(&app).common.last_focus.clone().unwrap();
    assert_eq!(focus.as_str(), "tables/head/nickname");
    assert!(
        ui.semantic(&focus).is_some(),
        "the keys rest on a control drawn"
    );
    press_key(&mut app, NamedKey::Enter);
    assert!(!writing(&mut app), "Return writes nothing");
    assert_eq!(staged(&mut app), 1);
    // A pointer's press on SET BY ATERM's stub: the keys go to the selected cell.
    act(&mut app, &format!("{CELL}{}/status", pk_of(0)), None);
    point_at(&mut app, "tables/fold/aterm");
    click(&mut app);
    let ui = presented(&mut app);
    assert!(
        said(&ui, "tables/fold/aterm").is_none(),
        "SET BY ATERM unfolded"
    );
    let focus = view_of(&app).common.last_focus.clone().unwrap();
    assert_eq!(focus.as_str(), format!("{CELL}{}/status", pk_of(0)));
    assert!(ui.semantic(&focus).is_some());
    press_key(&mut app, NamedKey::Enter);
    assert!(!writing(&mut app), "Return writes nothing");
    assert_eq!(staged(&mut app), 1);
}

/// A reply of the reader's that ends the change a hidden column was drawn for (a Write's) moves
/// the selection off that column at once, as a press that hid it does: the grid never selects a
/// cell it does not draw.
/// NEGATIVE CONTROL: with `NativeRuntime::poll_tables` not settling the views' columns (its
/// `settle_columns` call answering `false`), the selection stays on the hidden location: `` the
/// selection left the hidden column ``.
#[test]
fn a_write_that_ends_a_hidden_columns_change_moves_the_selection_off_it() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("location"), None);
    act(&mut app, COLUMNS, None);
    assert!(!drawn(&render(&app), "location"), "the location is hidden");
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "location", "rack z".into());
    assert!(
        drawn(&render(&app), "location"),
        "its change holds the column in"
    );
    act(&mut app, &format!("{CELL}{MC}/location"), None);
    assert_eq!(selected(&app).1, "location");
    act(&mut app, WRITE, None);
    settle_write(&mut app);
    // The runtime takes the reader's replies when it is woken for them.
    app.native_runtime.poll_tables();
    let (pk, column, _) = selected(&app);
    assert_eq!(pk, MC);
    assert_ne!(column, "location", "the selection left the hidden column");
    assert!(drawn(&render(&app), &column), "{column} is drawn");
}

/// Hiding the column the rows are sorted by takes its header, which says so and is where the
/// sort is undone: the rows are in the table's order again. The fold never takes the column the
/// rows are sorted by.
/// NEGATIVE CONTROL: with the hide leaving the sort (its `view.sort.remove` skipped), the rows
/// stay sorted by the hidden chip: `` no sort ``. With `kept` blind to the sort (its `sorted`
/// clause made false), the sorted nickname folds: `` the fold leaves the sorted column drawn ``.
#[test]
fn hiding_the_sorted_column_puts_the_rows_back_and_the_fold_keeps_it() {
    let mut app = open_with(machines());
    act(&mut app, "tables/head/chip", None);
    act(&mut app, "tables/head/chip", None);
    assert_eq!(
        rows_in_order(&app, 3),
        [pk_of(1), pk_of(0), pk_of(2)],
        "from the greatest chip down"
    );
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("chip"), None);
    assert!(!view_of(&app).sort.contains_key("machine"), "no sort");
    assert_eq!(rows_in_order(&app, 3), [pk_of(0), pk_of(1), pk_of(2)]);
    act(&mut app, COLUMNS, None);
    // Sorted by an empty column, the fold turned on again leaves it drawn.
    act(&mut app, "tables/fold/stored", None);
    act(&mut app, "tables/head/nickname", None);
    act(&mut app, COLUMNS, None);
    act(&mut app, FOLD, None);
    act(&mut app, FOLD, None);
    let ui = render(&app);
    assert!(
        drawn(&ui, "nickname"),
        "the fold leaves the sorted column drawn"
    );
    assert!(!drawn(&ui, "notes"), "the rest fold again");
}

/// The open list is a surface over the page: a press on its heading, a group's word, the words
/// over the counts, its closing line or a line that takes no press (a column with a change not
/// written) is the list's. Nothing under it hears the press: no header sorts, no cell is
/// selected, the changes waiting stay as they were, the list stays open, and the keys rest on
/// a control the page draws.
/// NEGATIVE CONTROL: with the list's panel no surface (its `.surface(…)` removed), its heading
/// takes no press and one there reaches whatever lies under it: `` tables/columns/head/title is
/// the list's left: None right: Some("tables/columns/panel") ``.
#[test]
fn a_press_on_the_lists_words_reaches_nothing_under_it() {
    let mut app = open_with(machines_wide());
    act(&mut app, COLUMNS, None);
    // The parts at the list's top, then, the list scrolled to its end, those at its foot; then,
    // a change waiting, the line of its column, which takes no press.
    let top = [
        "tables/columns/head/title".to_owned(),
        "tables/columns/group/stored/word".to_owned(),
        "tables/columns/group/stored/note".to_owned(),
    ];
    let foot = [
        "tables/columns/group/linked/word".to_owned(),
        "tables/columns/note".to_owned(),
    ];
    let waiting = [format!("{}/name", line_of("nickname"))];
    let mut over_the_page = 0;
    for (phase, parts) in [&top[..], &foot[..], &waiting[..]].into_iter().enumerate() {
        if phase == 1 {
            with_view_mut(&mut app, |view| {
                view.columns.scroll = f32::MAX;
                view.common.presentation_revision =
                    view.common.presentation_revision.wrapping_add(1);
            });
        }
        if phase == 2 {
            app.native_runtime.tables_mut().unwrap().stage(
                "machine",
                &pk_of(1),
                "nickname",
                "Desk".into(),
            );
            with_view_mut(&mut app, |view| {
                view.columns.scroll = 0.0;
                view.common.presentation_revision =
                    view.common.presentation_revision.wrapping_add(1);
            });
        }
        let staged = usize::from(phase == 2);
        for part in parts {
            let ui = presented(&mut app);
            let at = ui
                .paint
                .iter()
                .find(|node| node.key.as_str() == part.as_str())
                .map(|node| node.rect)
                .unwrap_or_else(|| panic!("{part} shows"));
            let (x, y) = (at.x + at.width / 2.0, at.y + at.height / 2.0);
            let hit = ui.hit_test(x, y).map(|hit| hit.key.as_str().to_owned());
            assert_eq!(hit.as_deref(), Some(PANEL), "{part} is the list's");
            // What the press would reach were the list no surface.
            over_the_page += usize::from(ui.hits.iter().any(|hit| {
                hit.rect.contains(x, y)
                    && (hit.key.as_str().starts_with(HEAD) || hit.key.as_str().starts_with(CELL))
            }));
            let (px, py) = window_px(&mut app, x, y);
            app.on_cursor_moved(WID, px, py);
            click(&mut app);
            let view = view_of(&app);
            assert!(view.columns.open, "{part}: the list stays open");
            assert_eq!(view.sort.get("machine"), None, "{part}: nothing sorted");
            assert_eq!(
                (view.row.as_deref(), view.column.as_deref()),
                (None, None),
                "{part}: no cell selected"
            );
            let tables = app.native_runtime.tables_mut().unwrap();
            assert_eq!(tables.staged.len(), staged, "{part}");
            assert!(!tables.writing, "{part}");
            let focus = view_of(&app).common.last_focus.clone().unwrap();
            assert!(
                presented(&mut app).semantic(&focus).is_some(),
                "{part}: the keys rest on {focus:?}, drawn"
            );
        }
    }
    assert!(
        over_the_page >= 2,
        "the list lies over the grid's controls: {over_the_page}"
    );
}

/// The wheel over the open list scrolls the list, never the grid under it, whose cells are the
/// least nodes there.
/// NEGATIVE CONTROL: with the wheel's target the least node under the pointer over a surface too
/// (`surface_target` not asked), the grid scrolls and the list does not: `` the grid stays ``.
#[test]
fn the_wheel_over_the_open_list_scrolls_the_list() {
    let mut app = open_with(tall_sheet(60, 40));
    act(&mut app, COLUMNS, None);
    let ui = presented(&mut app);
    // A point of the list's lines, in the room they show in, over the grid's cells.
    let lines = place(&ui, "tables/columns/lines");
    let (x, y) = (lines.x + 40.0, lines.y + lines.height / 2.0);
    assert!(
        ui.hits
            .iter()
            .any(|hit| hit.rect.contains(x, y) && hit.key.as_str().starts_with(CELL)),
        "the list lies over the grid's cells there"
    );
    let (px, py) = window_px(&mut app, x, y);
    app.on_cursor_moved(WID, px, py);
    let mut moved = false;
    for notch in [-1.0, 1.0, 1.0] {
        app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(0.0, notch));
        let view = view_of(&app);
        assert_eq!(view.scroll.get(), 0.0, "the grid stays");
        moved |= view.columns.scroll > 0.0;
        presented(&mut app);
    }
    assert!(moved, "the list scrolls");
}

/// An action no press on the list made (a paste) closes it and leaves the keys on the Columns
/// button, which the page still draws, and repaints the page though the page made nothing of
/// the paste: Return then opens the list again and never presses Write.
/// NEGATIVE CONTROL: with the closing leaving the keys where they were (its `view.focus(COLUMNS)`
/// removed), they rest on the chip's line the page no longer draws: `` left:
/// "tables/columns/column/chip" right: "tables/columns" ``.
#[test]
fn a_paste_on_the_open_list_closes_it_with_the_keys_on_the_button() {
    let mut app = open_with(machines());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(1), "notes", "Desk".into());
    act(&mut app, COLUMNS, None);
    press_key(&mut app, NamedKey::ArrowDown);
    assert_eq!(selected(&app).2, line_of("chip"));
    let revision = view_of(&app).common.presentation_revision;
    act(&mut app, PASTE, Some(SemanticInput::Text("x".into())));
    let view = view_of(&app);
    assert!(!view.columns.open, "the list closes");
    assert_ne!(
        view.common.presentation_revision, revision,
        "the page repaints"
    );
    assert_eq!(selected(&app).2, COLUMNS);
    let ui = presented(&mut app);
    assert!(ui.semantic(&UiKey::new(COLUMNS)).is_some());
    press_key(&mut app, NamedKey::Enter);
    assert!(view_of(&app).columns.open, "Return opens the list again");
    assert!(!app.native_runtime.tables_mut().unwrap().writing);
}

/// A cell of a folded column chosen from elsewhere (Check's Show row, a control client's press)
/// draws its column, which the grid had no place for, and scrolls it into view across.
/// NEGATIVE CONTROL: with `settle_columns` not placing a column the grid did not lay out (its
/// `reveal_laid` call removed), the grid stays at its left: `` scrolled across: 0 ``.
#[test]
fn a_folded_cell_chosen_from_elsewhere_is_scrolled_into_view() {
    let mut snapshot = tall_sheet(3, 12);
    snapshot.sheets[0]
        .columns
        .push(column("spare", Group::Stored, true));
    let mut app = open_with(snapshot);
    let ui = render(&app);
    assert!(!drawn(&ui, "spare"), "no row fills the spare: it folds");
    act(&mut app, &format!("{CELL}{}/spare", pk_of(1)), None);
    let ui = render(&app);
    assert!(drawn(&ui, "spare"), "the selected column is drawn");
    let across = view_of(&app).across;
    assert!(across > 0.0, "scrolled across: {across}");
    let cell = place(&ui, &format!("{CELL}{}/spare/seat", pk_of(1)));
    let grid = place(&ui, "tables/grid");
    assert!(
        cell.x >= grid.x && cell.right() <= grid.right() + 0.5,
        "the cell shows whole: {cell:?} in {grid:?}"
    );
}

/// Every word of the open list reads on its ground on every shipped theme: an empty column's
/// name, a column's count, a group's word and the closing line hold the sheet's word floor.
/// NEGATIVE CONTROL: with an empty column's name in `StyleRef::Faint`, 52 themes fail:
/// `` "Default: an empty column's name reads 2.6" ``.
#[test]
fn the_lists_words_read_on_its_ground() {
    use crate::widget::DrawPrim;
    let mut app = open_with(machines());
    act(&mut app, COLUMNS, None);
    let ui = render(&app);
    let mut under = Vec::new();
    for (name, theme) in shipped_themes() {
        let roles = crate::settings::Roles::from_theme(theme);
        let painted = ui.painted(roles);
        let prims = |key: &str| {
            painted
                .iter()
                .find(|(painted, _)| painted.as_str() == key)
                .map(|(_, prims)| prims.clone())
                .unwrap_or_else(|| panic!("{key} is painted"))
        };
        let ground = prims("tables/columns/panel/ground")
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                _ => None,
            })
            .expect("the list fills its ground");
        let ink = |key: &str| {
            prims(key)
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} draws words"))
        };
        for (what, key) in [
            (
                "an empty column's name",
                format!("{}/name", line_of("nickname")),
            ),
            (
                "a filled column's name",
                format!("{}/name", line_of("chip")),
            ),
            ("a column's count", format!("{}/fill", line_of("chip"))),
            (
                "a group's word",
                "tables/columns/group/stored/word".to_owned(),
            ),
            ("the closing line", "tables/columns/note".to_owned()),
        ] {
            let ratio = crate::native_appearance::contrast_ratio(ink(&key), ground);
            if ratio < crate::native_appearance::SHEET_WORD_FLOOR {
                under.push(format!("{name}: {what} reads {ratio:.1}"));
            }
        }
    }
    assert!(under.is_empty(), "{} themes fail: {under:#?}", under.len());
}

/// Once Show all has shown everything it takes no press, though the keys are on it: Up goes to
/// the line above it, not to the list's top, and Down past the list's end goes nowhere.
/// NEGATIVE CONTROL: with `step_columns` finding the keys only on a line that takes a press, from
/// Show all they go to the top: `` Down past the end goes nowhere left: "tables/columns/fold"
/// right: "tables/columns/all" ``.
#[test]
fn up_from_show_all_once_it_showed_everything_goes_to_the_line_above() {
    let mut app = open_with(machines());
    act(&mut app, COLUMNS, None);
    for _ in 0..10 {
        press_key(&mut app, NamedKey::ArrowDown);
    }
    assert_eq!(selected(&app).2, ALL);
    press_key(&mut app, NamedKey::Enter);
    let ui = render(&app);
    for name in ["nickname", "notes", "seen", LINKED] {
        assert!(drawn(&ui, name), "Show all draws {name}");
    }
    let all = ui.semantic(&UiKey::new(ALL)).unwrap();
    assert_eq!(all.state.map(|state| state.enabled), Some(false));
    press_key(&mut app, NamedKey::ArrowDown);
    assert_eq!(selected(&app).2, ALL, "Down past the end goes nowhere");
    press_key(&mut app, NamedKey::ArrowUp);
    assert_eq!(selected(&app).2, line_of(LINKED));
}

/// [`machines`] as a table with a lifecycle: the tools row then holds its filter, and as it is
/// now the Add, or where `elsewhere` names who adds its rows, those words.
fn with_life(mut snapshot: Snapshot, elsewhere: Option<&'static str>) -> Snapshot {
    snapshot.sheets[0].life = Some(rows::RowLife {
        words: &["live", "retired"],
        ends: "retired",
        born_elsewhere: elsewhere,
        born_columns: ["label", "chip", "nickname", "notes"]
            .map(str::to_owned)
            .to_vec(),
        set_by: Vec::new(),
    });
    snapshot
}

/// `snapshot` read as of an earlier point: the tools row then holds the words naming it and Now.
fn as_of(mut snapshot: Snapshot) -> Snapshot {
    snapshot.at = Some(AsOf {
        operation: [7; 32],
        by: None,
    });
    snapshot
}

/// Where node `key` is laid out, whether the paint or the tree lists it.
fn placed(ui: &CompiledUi, key: &str) -> Option<LogicalRect> {
    ui.paint
        .iter()
        .find(|node| node.key.as_str() == key)
        .map(|node| node.rect)
        .or_else(|| {
            ui.semantic(&UiKey::new(key.to_owned()))
                .map(|node| node.rect)
        })
}

/// On a page of any width, at the text size and at twice it, every tool of the tools row stands
/// in it after the one before, none over its neighbour or past the row's end, with the Columns
/// button at that end: for a table with no lifecycle (the find and the button), one with its
/// filter and Add, one whose rows are added elsewhere (the filter and the words saying who adds
/// them) and a past point (the filter, the words naming it and Now). Where its count does not
/// fit beside the rest the button paints `Columns` alone, its count still in its name; the rest
/// give up room in turn. On a wide page every tool shows whole. The open list's right edge is
/// the button's.
/// NEGATIVE CONTROLS: with the tools row leaving the lifecycle filter out of the room it shares
/// (its want taken as `None` for the gaps and the fit, the filter still laid out whole), the Add
/// runs past the row: `` filter and Add, 150 wide at 1: the tools stand apart in the row:
/// tables/rows/add LogicalRect { x: 136.6, y: 65.0, width: 1.0, height: 28.0 } after 124.6 in
/// LogicalRect { x: 16.0, y: 62.0, width: 118.0, height: 34.0 } ``; with a tool's seat narrowed
/// but not its button (`rows::seat` leaving the button as wide as `button` made it), the Add
/// stands over the filter: `` filter and Add, 150 wide at 1: the tools stand apart in the row:
/// tables/rows/add LogicalRect { x: 42.0, y: 65.0, width: 109.8, height: 28.0 } after
/// 123.200005 ``.
#[test]
fn the_columns_button_fits_the_tools_row_at_any_width() {
    let scale_was = crate::native_appearance::text_scale();
    let set_scale = |scale: f32| {
        crate::native_appearance::install_preferences(
            crate::native_appearance::AppearancePreferences {
                text_scale: scale,
                ..crate::native_appearance::current_preferences()
            },
        );
    };
    let painted_label = |ui: &CompiledUi| {
        ui.paint
            .iter()
            .find(|node| node.key.as_str() == COLUMNS)
            .and_then(|node| match &node.content {
                UiContent::Button(control) => control.spec.visual_label.clone(),
                _ => None,
            })
    };
    let elsewhere = "aterm adds one as a Mac joins";
    let cases = [
        ("no lifecycle", machines(), vec![FIND, COLUMNS]),
        (
            "filter and Add",
            with_life(machines(), None),
            vec![FIND, rows::FILTER, rows::ADD, COLUMNS],
        ),
        (
            "filter and who adds",
            with_life(machines(), Some(elsewhere)),
            vec![FIND, rows::FILTER, "tables/rows/elsewhere", COLUMNS],
        ),
        (
            "a past point",
            as_of(with_life(machines(), None)),
            vec![FIND, rows::FILTER, "tables/asof", NOW, COLUMNS],
        ),
    ];
    let mut short = 0;
    let mut whole = 0;
    for (case, snapshot, tools) in cases {
        let mut app = open_with(snapshot);
        for scale in [1.0, 2.0] {
            set_scale(scale);
            for width in (150..=1600).step_by(50) {
                let at = format!("{case}, {width} wide at {scale}");
                let ui = render_wide(&app, width as f32);
                let row = place(&ui, "tables/tools");
                assert!(
                    row.right() <= width as f32 + 0.5,
                    "{at}: the row ends in the window: {row:?}"
                );
                let mut last = row.x;
                for key in &tools {
                    let tool = placed(&ui, key).unwrap_or_else(|| panic!("{at}: {key} stands"));
                    assert!(
                        tool.x + 0.5 >= last && tool.right() <= row.right() + 0.5,
                        "{at}: the tools stand apart in the row: {key} {tool:?} after {last} \
                         in {row:?}"
                    );
                    last = tool.right();
                }
                let button = place(&ui, COLUMNS);
                assert!(
                    (button.right() - row.right()).abs() < 0.5,
                    "{at}: the button ends where the row does: {button:?} in {row:?}"
                );
                assert_eq!(
                    said(&ui, COLUMNS).as_deref(),
                    Some("Columns 3 of 7 shown; choose the columns shown"),
                    "{at}"
                );
                match painted_label(&ui).as_deref() {
                    Some("Columns") => short += 1,
                    Some("Columns 3 of 7") => whole += 1,
                    other => panic!("{at}: {other:?}"),
                }
                if width == 1600 && scale == 1.0 {
                    // Room for everything: each tool as wide as it is whole.
                    assert_eq!(
                        painted_label(&ui).as_deref(),
                        Some("Columns 3 of 7"),
                        "{at}"
                    );
                    for (key, label) in [
                        (rows::FILTER, "Live rows"),
                        (rows::ADD, "Add machine"),
                        (NOW, "Now"),
                    ] {
                        if let Some(tool) = placed(&ui, key) {
                            assert!(
                                tool.width + 0.5
                                    >= crate::native_ui::native_button_label_width(label) + 24.0,
                                "{at}: {key} shows whole: {tool:?}"
                            );
                        }
                    }
                    for key in ["tables/asof", "tables/rows/elsewhere"] {
                        if let Some(words) = painted(&ui, key) {
                            assert!(!words.ends_with('\u{2026}'), "{at}: {key} whole: {words}");
                        }
                    }
                }
                act(&mut app, COLUMNS, None);
                let ui = render_wide(&app, width as f32);
                let panel = place(&ui, PANEL);
                assert!(
                    (panel.right() - place(&ui, COLUMNS).right()).abs() < 0.5,
                    "{at}: the list's right edge is the button's: {panel:?}"
                );
                act(&mut app, COLUMNS, None);
            }
        }
    }
    set_scale(scale_was);
    assert!(short > 0 && whole > 0, "short {short}, whole {whole}");
}

/// Each line of the open list ends its words where the others do: the switch's state, a
/// column's count, and the count of a column with a change not written, whose line holds a
/// note more, all end at the list's inner edge.
/// NEGATIVE CONTROL: with the words' room on a line taken from the list's width, not the ground's
/// inside the frame's 1 pt left rule (`width - 20.0`), the switch's state ends past the heading's
/// count: `` tables/columns/fold/state: left: 1023.0 right: 1022.0 ``.
#[test]
fn every_line_of_the_list_ends_its_words_at_one_edge() {
    let mut app = open_with(machines());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(1), "nickname", "Desk".into());
    act(&mut app, COLUMNS, None);
    let ui = render(&app);
    let right = |key: &str| place(&ui, key).right();
    let edge = right("tables/columns/head/count");
    for key in [
        "tables/columns/fold/state".to_owned(),
        format!("{}/fill", line_of("chip")),
        format!("{}/fill", line_of("nickname")),
        "tables/columns/group/stored/note".to_owned(),
    ] {
        assert!(
            (right(&key) - edge).abs() < 0.5,
            "{key}: left: {:.1} right: {edge:.1}",
            right(&key)
        );
    }
}

/// A row the filter leaves out shows while a change to it is staged and leaves once the last is
/// taken back, and the heading's count and each column's fill read against the rows the grid
/// shows all the while: a retired machine's nickname typed under the retired rows, the filter
/// turned to the live rows (the machine stands among them, its change staged), then its nickname
/// typed back to what the read holds, which takes the change back.
/// NEGATIVE CONTROL: with the order worked out again only where a row's lifecycle is staged (its
/// key's last part the rows whose `row` cell is staged, as before), the machine stays in the live
/// grid while the fills no longer count it: `` taken back, the retired machine leaves the live
/// rows ``.
#[test]
fn a_row_its_staged_change_shows_leaves_with_it_and_the_fills_follow() {
    let mut snapshot = with_life(machines(), None);
    snapshot.sheets[0].rows[2]
        .cells
        .insert(aterm_db::typed::LIFECYCLE.to_owned(), text("retired"));
    let mut app = open_with(snapshot);
    let shows = |ui: &CompiledUi| said(ui, &format!("{CELL}{}/label", pk_of(2))).is_some();
    let ui = render(&app);
    assert!(!shows(&ui), "the live rows leave the retired machine out");
    assert_eq!(
        header_fill(&ui, "chip").as_deref(),
        Some("2 of 2 rows hold a value")
    );
    act(&mut app, rows::FILTER, None);
    assert!(shows(&render(&app)), "the retired rows show it");
    let stage = |app: &mut App, text: &str| {
        app.native_runtime.tables_mut().unwrap().stage(
            "machine",
            &pk_of(2),
            "nickname",
            text.into(),
        );
    };
    stage(&mut app, "Desk");
    // All rows, then the live rows again.
    act(&mut app, rows::FILTER, None);
    act(&mut app, rows::FILTER, None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, rows::FILTER).as_deref(),
        Some("Live rows shown; press for retired rows")
    );
    assert!(
        shows(&ui),
        "its change staged, the retired machine stands among the live"
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("3 rows"));
    assert_eq!(
        header_fill(&ui, "chip").as_deref(),
        Some("2 of 3 rows hold a value")
    );
    // Typed back to the empty nickname the read holds, the change is taken back.
    stage(&mut app, "");
    assert!(
        app.native_runtime.tables_mut().unwrap().staged.is_empty(),
        "the nickname typed back is no change"
    );
    let ui = render(&app);
    assert!(
        !shows(&ui),
        "taken back, the retired machine leaves the live rows"
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("2 rows"));
    assert_eq!(
        header_fill(&ui, "chip").as_deref(),
        Some("2 of 2 rows hold a value")
    );
}

/// A column a cell of which is refused is never hidden, as one holding a change not written is
/// not, though nothing in it is staged: a new link with its machine chosen and its account left
/// empty is refused at the account, which its key is made from. Hidden before the link was added,
/// the account is drawn again with its refusal, so the person reaches the cell the Write waits
/// on, and its line in the list says why and takes no press.
/// NEGATIVE CONTROL: with `holds` finding no refusal (its `checked`/`refused` clause removed),
/// the hidden account stays hidden: `` the refused account is drawn ``.
#[test]
fn a_column_holding_a_refused_cell_is_never_hidden() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/preference", None);
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("account"), None);
    act(&mut app, COLUMNS, None);
    assert!(
        !drawn(&render(&app), "account"),
        "the person hid the account"
    );
    act(&mut app, rows::ADD, None);
    let first = format!("{}1", rows::NEW);
    let machine = format!("{CELL}{first}/machine");
    act(&mut app, &machine, None);
    if said(&render(&app), EDIT).is_none() {
        act(&mut app, &machine, None);
    }
    act(
        &mut app,
        &format!("{}0", crate::native_tables_choices::CHOICE),
        None,
    );
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written \u{b7} 2 refused")
    );
    assert!(drawn(&ui, "account"), "the refused account is drawn");
    assert_eq!(
        cell_says(&ui, &first, "account"),
        "empty, refused: a new row's key is made from this column; fill it in"
    );
    act(&mut app, COLUMNS, None);
    let ui = render(&app);
    let line = ui
        .semantic(&UiKey::new(line_of("account")))
        .expect("the account's line is listed");
    assert!(
        line.label
            .starts_with("Account, shown, has a refused cell, "),
        "{}",
        line.label
    );
    assert_eq!(line.state.map(|state| state.enabled), Some(false));
    assert_eq!(
        painted(&ui, &format!("{}/note", line_of("account"))).as_deref(),
        Some("has a refused cell")
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's columns (docs/PLAN.md LT-40) on hand-made reads and on a scratch registry
//! (`aterm_db::table::fixture::Lab`: an isolated home, no live vault, fleet or Commons): which
//! columns the grid draws, the fold of the empty ones, and each column's fill. Every value is
//! synthetic.

use super::super::native_tables_tests::{
    MC, VIEWPORT, WID, act, cell_says, key_with, one_sheet, open_with, pk_of, place, point_at,
    presented, press_key, put_in, render, render_wide, said, settle, settle_write, shipped_themes,
    tall_sheet, tree, two_macs, view_of, window_px, with_view_mut,
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

// --- The filters (docs/PLAN.md LT-41) --------------------------------------------------------

use super::super::native_tables_filters::{
    BLANK, CHIP, CLEAR, CLEAR_NONE, EMPTY, HAS, LEAST, MARK, MOST, REMOVE, value_key,
};

/// Six machines under four stored columns: `label` the key; `role`, words (worker three times,
/// desk, build, one empty); `ram`, a whole number headed `RAM GB` (16, 64, 128, 32, one empty,
/// 96); `chip` (one empty, two `M4 Pro`).
fn fleet() -> Snapshot {
    let mut snapshot = one_sheet(Shown::default());
    let sheet = &mut snapshot.sheets[0];
    sheet.key = "label".into();
    let mut ram = column("ram", Group::Stored, true);
    ram.label = "RAM GB".into();
    ram.kind = ColumnKind::Number;
    sheet.columns = vec![
        column("label", Group::Stored, true),
        column("role", Group::Stored, true),
        ram,
        column("chip", Group::Stored, true),
    ];
    let roles = ["worker", "worker", "desk", "", "worker", "build"];
    let rams = ["16", "64", "128", "32", "", "96"];
    let chips = ["M2", "M4 Pro", "", "M4 Pro", "M2", "M1"];
    sheet.rows = (0..6)
        .map(|n| {
            let mut cells = BTreeMap::new();
            cells.insert("label".to_owned(), text(&format!("m{n}")));
            cells.insert("role".to_owned(), text(roles[n]));
            cells.insert("ram".to_owned(), text(rams[n]));
            cells.insert("chip".to_owned(), text(chips[n]));
            SheetRow {
                pk: pk_of(n),
                cells,
            }
        })
        .collect();
    snapshot
}

fn mark_of(name: &str) -> String {
    format!("{MARK}{name}")
}

fn value_of(value: &str) -> String {
    value_key(value)
}

/// What a chip paints, by its column.
fn chip_words(ui: &CompiledUi, name: &str) -> Option<String> {
    painted(ui, &format!("{CHIP}{name}/words"))
}

/// The rows of [`fleet`] the grid shows, by their place in it (`0` for `m0`).
fn fleet_shown(app: &App) -> Vec<usize> {
    rows_in_order(app, 6)
        .iter()
        .filter_map(|pk| (0..6).find(|n| pk_of(*n) == *pk))
        .collect()
}

/// Each column's header carries a mark, a press of its own that opens the list of the column's
/// values (the header's own press still sorts): two one-press choices, then each value with the
/// rows that hold it, the most held first and the empty rows last; a number's column takes `At
/// least` and `At most`. Values chosen keep the rows that hold one of them, a bound the rows
/// within it, `Only rows with a value` those that hold one, and filters join with and. Each
/// filter that stands is a chip, in a line of its own under the tools, that says it in words,
/// and whose close mark takes it away; `Clear filters` ends the line where two stand. The heading
/// counts the rows shown of the rows the lifecycle filter leaves, and where no row passes the
/// grid says so with `Clear filters`.
/// NEGATIVE CONTROL: with `ColumnFilter::passes` taking the values chosen as passing every row
/// (`chosen` made `true || …`), the role's two values keep every row: `` two values keep their
/// rows left: Some("6 rows") right: Some("4 of 6 rows") ``.
#[test]
fn rows_are_filtered_by_the_values_of_a_column_and_each_filter_is_a_chip_in_words() {
    let mut app = open_with(fleet());
    let ui = render(&app);
    assert_eq!(said(&ui, &mark_of("role")).as_deref(), Some("Filter Role"));
    let head = place(&ui, "tables/head/role/seat");
    let mark = place(&ui, &mark_of("role"));
    assert!(
        mark.x > head.x + head.width / 2.0 && mark.right() < head.right(),
        "the mark stands at the header's right end: {mark:?} in {head:?}"
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("6 rows"));
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "no filter, no chips"
    );
    // The header's own press still sorts.
    act(&mut app, "tables/head/role", None);
    assert!(view_of(&app).sort.contains_key("machine"), "a header sorts");
    act(&mut app, "tables/head/role", None);
    act(&mut app, "tables/head/role", None);
    assert!(!view_of(&app).sort.contains_key("machine"));

    // The role's list: the choices, then its values, the most held first, the empty rows last.
    act(&mut app, &mark_of("role"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, HAS).as_deref(),
        Some("Only rows with a value, off, 5 rows")
    );
    assert_eq!(
        said(&ui, EMPTY).as_deref(),
        Some("Only rows that are empty, off, 1 row")
    );
    assert!(said(&ui, LEAST).is_none(), "words take no bounds");
    assert_eq!(
        said(&ui, &value_of("worker")).as_deref(),
        Some("worker, not chosen, 3 rows")
    );
    assert_eq!(
        said(&ui, BLANK).as_deref(),
        Some("(empty), not chosen, 1 row")
    );
    let y = |key: &str| place(&ui, key).y;
    assert!(
        y(&value_of("worker")) < y(&value_of("build"))
            && y(&value_of("build")) < y(&value_of("desk"))
            && y(&value_of("desk")) < y(BLANK),
        "the most held first, then by their words, the empty rows last"
    );

    // Two values keep the rows that hold one of them; their chip says them, as the list orders
    // them.
    act(&mut app, &value_of("worker"), None);
    act(&mut app, &value_of("desk"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/count").as_deref(),
        Some("4 of 6 rows"),
        "two values keep their rows"
    );
    assert_eq!(fleet_shown(&app), [0, 1, 2, 4]);
    assert_eq!(
        chip_words(&ui, "role").as_deref(),
        Some("ROLE is worker, desk")
    );
    assert_eq!(
        said(&ui, &format!("{CHIP}role")).as_deref(),
        Some("ROLE is worker, desk; change this filter")
    );
    assert_eq!(
        said(&ui, &format!("{REMOVE}role")).as_deref(),
        Some("Remove filter, ROLE is worker, desk")
    );
    assert_eq!(
        said(&ui, &mark_of("role")).as_deref(),
        Some("Filter Role, filtered")
    );
    assert!(said(&ui, CLEAR).is_none(), "one filter: no Clear filters");

    // A number's bounds, joined with the role's.
    act(&mut app, &mark_of("ram"), None);
    act(&mut app, LEAST, Some(SemanticInput::Text("64".into())));
    let ui = render(&app);
    assert_eq!(fleet_shown(&app), [1, 2], "worker or desk, and 64 or more");
    assert_eq!(
        chip_words(&ui, "ram").as_deref(),
        Some("RAM GB at least 64")
    );
    act(&mut app, MOST, Some(SemanticInput::Text("100".into())));
    let ui = render(&app);
    assert_eq!(fleet_shown(&app), [1]);
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("1 of 6 rows"));
    assert_eq!(
        chip_words(&ui, "ram").as_deref(),
        Some("RAM GB at least 64 and at most 100")
    );
    assert_eq!(said(&ui, CLEAR).as_deref(), Some("Clear filters, all 2"));
    // The role's chip taken away leaves the bounds: the build machine's 96 is within them.
    act(&mut app, &format!("{REMOVE}role"), None);
    assert_eq!(fleet_shown(&app), [1, 5]);
    // A one-press choice: the chip has a value.
    act(&mut app, &mark_of("chip"), None);
    act(&mut app, HAS, None);
    let ui = render(&app);
    assert_eq!(chip_words(&ui, "chip").as_deref(), Some("CHIP has a value"));
    assert_eq!(fleet_shown(&app), [1, 5]);
    // Only the empty chips: no row passes, and the grid says so with Clear filters.
    act(&mut app, EMPTY, None);
    let ui = render(&app);
    assert_eq!(chip_words(&ui, "chip").as_deref(), Some("CHIP is empty"));
    assert!(fleet_shown(&app).is_empty());
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("0 of 6 rows"));
    assert_eq!(
        said(&ui, "tables/rows/none").as_deref(),
        Some("No row passes these filters")
    );
    assert_eq!(said(&ui, CLEAR_NONE).as_deref(), Some("Clear filters"));
    act(&mut app, CLEAR_NONE, None);
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("6 rows"));
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "every filter cleared"
    );
    // The empty value chosen beside others reads `or empty`; past three values a chip says how
    // many more.
    act(&mut app, &mark_of("role"), None);
    for value in [value_of("worker"), value_of("build"), value_of("desk")] {
        act(&mut app, &value, None);
    }
    act(&mut app, BLANK, None);
    let ui = render(&app);
    assert_eq!(
        chip_words(&ui, "role").as_deref(),
        Some("ROLE is worker, build, desk or empty")
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("6 rows"));
    act(&mut app, &mark_of("ram"), None);
    for value in ["16", "32", "64", "96"] {
        act(&mut app, &value_of(value), None);
    }
    act(&mut app, BLANK, None);
    let ui = render(&app);
    assert_eq!(
        chip_words(&ui, "ram").as_deref(),
        Some("RAM GB is 16, 32, 64 and 1 more, or empty")
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("5 of 6 rows"));
}

/// A filter judges what the read holds, never words staged in a cell, and the rows it leaves are
/// worked out again on a read: a row whose role is typed to another word stays, a row whose
/// change is staged stays through a read that moves its value out of the filter, while a row
/// the read moves out with nothing staged leaves; a new row stays shown, whatever it holds.
/// NEGATIVE CONTROL: with the filters' step not keeping a row with a staged cell (its
/// `staged(app, sheet, row)` clause removed), the read takes the first machine away: `` a row
/// with a change staged stays left: [4] right: [0, 4] ``.
#[test]
fn a_filter_judges_what_the_read_holds_and_no_row_leaves_while_it_is_typed_in() {
    let mut app = open_with(with_life(fleet(), None));
    act(&mut app, &mark_of("role"), None);
    act(&mut app, &value_of("worker"), None);
    act(&mut app, &mark_of("role"), None);
    assert_eq!(fleet_shown(&app), [0, 1, 4]);
    // Typed to another word, the first machine stays: the read holds worker.
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(0), "role", "build".into());
    assert_eq!(fleet_shown(&app), [0, 1, 4], "typing moves no row");
    // A read in which the first two machines are desks: the first, its change to build still
    // staged, stays; the second leaves.
    let mut moved = with_life(fleet(), None);
    for n in [0, 1] {
        moved.sheets[0].rows[n]
            .cells
            .insert("role".to_owned(), text("desk"));
    }
    app.native_runtime.tables_mut().unwrap().show(moved);
    assert_eq!(
        app.native_runtime.tables_mut().unwrap().staged.len(),
        1,
        "the change to build is still staged"
    );
    assert_eq!(
        fleet_shown(&app),
        [0, 4],
        "a row with a change staged stays"
    );
    // A new row stays shown: it holds no role.
    act(&mut app, rows::ADD, None);
    let ui = render(&app);
    let born = format!("{}1", rows::NEW);
    assert!(
        said(&ui, &format!("{CELL}{born}/label")).is_some(),
        "the new row is shown"
    );
}

/// The list of a column's values is a surface over the page that keeps the keys: the arrows
/// move among its lines, Space presses one, and Escape closes it with the keys back on the
/// mark it opened from (or the chip). A press on its heading reaches nothing under it: no
/// header sorts and the list stays open. A value's line is named by a key of one word, its
/// value's spaces written out, which a driver (`aterm ctl act`, whose keys hold no whitespace)
/// can press.
/// NEGATIVE CONTROL: with the list's Escape leaving the keys where they were (its
/// `close_filter(app, true)` made `close_filter(app, false)`), they stay on a line no longer
/// drawn: `` left: "tables/filter/value/worker" right: "tables/mark/role" ``.
#[test]
fn the_list_of_values_takes_the_keys_and_a_press_on_it_reaches_nothing_under_it() {
    let mut app = open_with(fleet());
    act(&mut app, &mark_of("role"), None);
    assert_eq!(selected(&app).2, HAS);
    press_key(&mut app, NamedKey::ArrowUp);
    assert_eq!(selected(&app).2, HAS, "the first line stays the first");
    press_key(&mut app, NamedKey::ArrowDown);
    press_key(&mut app, NamedKey::ArrowDown);
    assert_eq!(selected(&app).2, value_of("worker"));
    assert!(
        tree_has(&tree(&app), "tables/filter/line/0/ring"),
        "the keys' ring is on the line"
    );
    press_key(&mut app, NamedKey::Space);
    assert_eq!(fleet_shown(&app), [0, 1, 4], "Space chose the worker");
    press_key(&mut app, NamedKey::Escape);
    assert!(!view_of(&app).filters.is_open(), "Escape closes the list");
    assert_eq!(selected(&app).2, mark_of("role"));
    // From its chip, Escape puts the keys back on the chip.
    act(&mut app, &format!("{CHIP}role"), None);
    assert!(view_of(&app).filters.is_open());
    press_key(&mut app, NamedKey::Escape);
    assert_eq!(selected(&app).2, format!("{CHIP}role"));
    // A press on its heading is the list's: it lies over the grid's header.
    act(&mut app, &mark_of("chip"), None);
    let ui = presented(&mut app);
    let title = place(&ui, "tables/filter/head/title");
    let (x, y) = (title.x + 10.0, title.y + title.height / 2.0);
    assert_eq!(
        ui.hit_test(x, y).map(|hit| hit.key.as_str().to_owned()),
        Some("tables/filter/panel".to_owned())
    );
    let (px, py) = window_px(&mut app, x, y);
    app.on_cursor_moved(WID, px, py);
    click(&mut app);
    let view = view_of(&app);
    assert!(view.filters.is_open(), "the list stays open");
    assert!(view.sort.is_empty(), "nothing sorted");
    assert_eq!(selected(&app).2, HAS, "the keys stay on the list");
    let pro = "tables/filter/value/M4%20Pro";
    assert_eq!(value_of("M4 Pro"), pro);
    assert_eq!(
        said(&render(&app), pro).as_deref(),
        Some("M4 Pro, not chosen, 2 rows")
    );
    act(&mut app, pro, None);
    assert_eq!(
        fleet_shown(&app),
        [1],
        "the key pressed chose M4 Pro, beside the worker"
    );
    assert_eq!(
        chip_words(&render(&app), "chip").as_deref(),
        Some("CHIP is M4 Pro")
    );
}

/// Each table keeps its own filters while the tab lives, and a read keeps them; a filter on a
/// column a person then hides stands, its chip with it, which opens the column's list though its
/// mark is gone.
/// NEGATIVE CONTROL: with `filters_of` reading the first table's filters whatever the sheet
/// (`.get(sheet.name)` made `.values().next()`), the disks show the machines' chip: `` the disks
/// have no filter ``.
#[test]
fn each_table_keeps_its_filters_and_a_hidden_columns_filter_stands_as_its_chip() {
    let mut app = open_with(machines_and_disks());
    act(&mut app, &mark_of("status"), None);
    act(&mut app, EMPTY, None);
    act(&mut app, &mark_of("status"), None);
    assert_eq!(
        said(&render(&app), "tables/count").as_deref(),
        Some("0 of 3 rows")
    );
    act(&mut app, "tables/sheet/disk", None);
    let ui = render(&app);
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "the disks have no filter"
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("3 rows"));
    act(&mut app, "tables/sheet/machine", None);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(machines_and_disks());
    let ui = render(&app);
    assert_eq!(
        chip_words(&ui, "status").as_deref(),
        Some("STATUS is empty"),
        "the machines keep theirs through a read"
    );
    // The status hidden: its mark goes with its header, its filter and chip stay.
    act(&mut app, COLUMNS, None);
    act(&mut app, &line_of("status"), None);
    act(&mut app, COLUMNS, None);
    let ui = render(&app);
    assert!(!drawn(&ui, "status") && said(&ui, &mark_of("status")).is_none());
    assert_eq!(
        chip_words(&ui, "status").as_deref(),
        Some("STATUS is empty")
    );
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("0 of 3 rows"));
    act(&mut app, &format!("{CHIP}status"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, EMPTY).as_deref(),
        Some("Only rows that are empty, on, 0 rows")
    );
    let (chip, panel) = (
        place(&ui, &format!("{CHIP}status/frame")),
        place(&ui, "tables/filter/panel"),
    );
    assert!(
        (panel.x - chip.x).abs() < 0.5 && panel.y > chip.bottom(),
        "the list opens under its chip: {panel:?} under {chip:?}"
    );
}

/// The chips' words read on their ground on every shipped theme, and their line, standing only
/// while a filter does, is counted once in the page's height: the grid gives it its height and
/// the gap under it, and the write bar still ends inside the window.
/// NEGATIVE CONTROL: with the page leaving the line out of its room (its `room -= line + 10.0`
/// removed), the grid keeps its height: `` the grid gives the line its room left: 694.0 right:
/// 654.0 ``.
#[test]
fn the_chips_read_on_their_ground_and_their_line_is_counted_once_in_the_page() {
    use crate::widget::DrawPrim;
    let mut app = open_with(fleet());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(0), "chip", "M9".into());
    let ui = render(&app);
    let work = place(&ui, "tables/work").height;
    act(&mut app, &mark_of("role"), None);
    act(&mut app, &value_of("worker"), None);
    act(&mut app, &mark_of("role"), None);
    let ui = render(&app);
    let line = place(&ui, "tables/chips");
    let tools = place(&ui, "tables/tools");
    assert!(line.y > tools.bottom(), "the chips stand under the tools");
    assert_eq!(
        place(&ui, "tables/work").height,
        work - line.height - 10.0,
        "the grid gives the line its room"
    );
    let bar = place(&ui, "tables/bar");
    assert!(
        bar.bottom() <= VIEWPORT.height,
        "the write bar ends in the window: {bar:?}"
    );
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
        let ground = prims(&format!("{CHIP}role/ground"))
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Panel { fill, .. } => Some([fill[0], fill[1], fill[2]]),
                _ => None,
            })
            .expect("the chip fills its ground");
        for (what, key) in [
            ("the chip's words", format!("{CHIP}role/words")),
            ("its close mark", format!("{CHIP}role/close")),
        ] {
            let ink = prims(&key)
                .iter()
                .find_map(|prim| match prim {
                    DrawPrim::Text { color, .. } => Some([color[0], color[1], color[2]]),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} draws words"));
            let ratio = crate::native_appearance::contrast_ratio(ink, ground);
            if ratio < crate::native_appearance::SHEET_WORD_FLOOR {
                under.push(format!("{name}: {what} reads {ratio:.1}"));
            }
        }
    }
    assert!(under.is_empty(), "{} themes fail: {under:#?}", under.len());
}

/// Whether the page is writing, and how many changes wait to be written.
fn writing_and_staged(app: &mut App) -> (bool, usize) {
    let tables = app.native_runtime.tables_mut().unwrap();
    (tables.writing, tables.staged.len())
}

/// [`fleet`] read again with the build machine (`m5`) a desk: no row holds build.
fn fleet_without_build() -> Snapshot {
    let mut moved = fleet();
    moved.sheets[0].rows[5]
        .cells
        .insert("role".to_owned(), text("desk"));
    moved
}

/// A value's line a press unchooses stays in the open list, unchosen, under the keys and the
/// pointer, though no row holds that value any longer: Return then presses that line again,
/// never the page's default button, Write.
/// NEGATIVE CONTROL: with `offered` keeping no value of the open list's own (its `open.kept` left out), the
/// line goes with the press: `` the unchosen line stays left: None right: Some("build, not
/// chosen, 0 rows") ``.
#[test]
fn a_value_unchosen_keeps_its_line_while_the_list_is_open() {
    let mut app = open_with(fleet());
    act(&mut app, &mark_of("role"), None);
    act(&mut app, &value_of("build"), None);
    act(&mut app, &mark_of("role"), None);
    assert_eq!(fleet_shown(&app), [5]);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(fleet_without_build());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(0), "chip", "M9".into());
    act(&mut app, &mark_of("role"), None);
    let build = value_of("build");
    assert_eq!(
        said(&render(&app), &build).as_deref(),
        Some("build, chosen, 0 rows"),
        "a value chosen is offered though no row holds it"
    );
    // The choices, the worker's and the desk's lines, then the build's.
    for _ in 0..4 {
        press_key(&mut app, NamedKey::ArrowDown);
    }
    assert_eq!(selected(&app).2, build);
    press_key(&mut app, NamedKey::Space);
    let ui = presented(&mut app);
    assert!(!tree_has(&tree(&app), "tables/chips"), "the filter is gone");
    assert_eq!(
        said(&ui, &build).as_deref(),
        Some("build, not chosen, 0 rows"),
        "the unchosen line stays"
    );
    assert_eq!(selected(&app).2, build, "the keys stay on it");
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        writing_and_staged(&mut app),
        (false, 1),
        "Return writes nothing"
    );
    assert_eq!(
        said(&render(&app), &build).as_deref(),
        Some("build, chosen, 0 rows"),
        "Return pressed the line"
    );
}

/// A read that takes the last row holding the value whose line has the keys takes that line
/// from the open list; the keys go to the line now standing where it stood, so Return presses a
/// line the list draws, never the page's default button, Write.
/// NEGATIVE CONTROL: with `settle_filters` answering `false` while a list is open, the keys stay on the line
/// the read took: `` the keys rest on the line where build stood left:
/// "tables/filter/value/build" right: "tables/filter/value/desk" ``.
#[test]
fn a_read_that_takes_the_line_under_the_keys_leaves_them_on_a_line_drawn() {
    let mut app = open_with(fleet());
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(0), "chip", "M9".into());
    act(&mut app, &mark_of("role"), None);
    // The choices, the worker's line, then the build's, as the list lays them out.
    for _ in 0..3 {
        press_key(&mut app, NamedKey::ArrowDown);
    }
    assert_eq!(selected(&app).2, value_of("build"));
    presented(&mut app);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .show(fleet_without_build());
    // The runtime takes the reader's replies when it is woken for them.
    app.native_runtime.poll_tables();
    let ui = presented(&mut app);
    let focus = selected(&app).2;
    assert_eq!(
        focus,
        value_of("desk"),
        "the keys rest on the line where build stood"
    );
    assert!(ui.semantic(&UiKey::new(focus)).is_some(), "a line drawn");
    press_key(&mut app, NamedKey::Enter);
    assert_eq!(
        writing_and_staged(&mut app),
        (false, 1),
        "Return writes nothing"
    );
}

static ALIASES: aterm_db::typed::Column = aterm_db::typed::Column::new(
    "aliases",
    aterm_db::typed::Ty::Set(&aterm_db::typed::Ty::Text(64)),
);
static LIMITS: aterm_db::typed::Column = aterm_db::typed::Column::new(
    "limits",
    aterm_db::typed::Ty::List(
        &aterm_db::typed::Ty::Struct(aterm_db::alab::registry::LIMIT),
        8,
    ),
);
static SOURCES: aterm_db::typed::Column = aterm_db::typed::Column::new(
    "notes_src",
    aterm_db::typed::Ty::List(&aterm_db::typed::Ty::SrcRef, 8),
);

/// [`fleet`] with a set of words (`aliases`: the first machine's members are `Lab, east` and
/// `m3`, which its cell shows as a JSON list; the second's `m2` and `m9`; the third's `m2`), a
/// list of records the grid shows as JSON (`limits`, the first two machines') and a list of
/// import coordinates (`notes_src`, the first machine's).
fn fleet_with_sets() -> Snapshot {
    let mut snapshot = fleet();
    let sheet = &mut snapshot.sheets[0];
    for (name, compiled) in [
        ("aliases", &ALIASES),
        ("limits", &LIMITS),
        ("notes_src", &SOURCES),
    ] {
        let mut added = column(name, Group::Stored, true);
        added.compiled = Some(compiled);
        sheet.columns.push(added);
    }
    let records = r#"[{"by":"m3","ops":3},{"by":"m4","ops":1}]"#;
    for (n, (aliases, limits, sources)) in [
        (r#"["Lab, east","m3"]"#, records, r#"["xlsx:A1","xlsx:B2"]"#),
        ("m2, m9", records, ""),
        ("m2", "", ""),
    ]
    .into_iter()
    .enumerate()
    {
        let cells = &mut sheet.rows[n].cells;
        cells.insert("aliases".to_owned(), text(aliases));
        cells.insert("limits".to_owned(), text(limits));
        cells.insert("notes_src".to_owned(), text(sources));
    }
    snapshot
}

/// A set's or a list's row holds each of its members as its cell shows them, a JSON list of
/// words (where a member holds a comma, or always for import coordinates) as its words; a list
/// the grid shows as JSON of records holds its cell's words as one value. A chip on a set says it
/// `includes` its values, which a row holds among others; the empty value beside them reads `or
/// is empty`, and alone `is empty`, as `Only rows that are empty` says it.
/// NEGATIVE CONTROL: with `held` splitting a JSON list on its commas (its `[` arm made `false && …`), the
/// member holding a comma is no value: `` a member holding a comma is one value left: None
/// right: Some("Lab, east, not chosen, 1 row") ``.
#[test]
fn a_sets_and_a_lists_values_are_their_members_as_the_grid_shows_them() {
    let mut app = open_with(fleet_with_sets());
    act(&mut app, &mark_of("aliases"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, &value_of("Lab, east")).as_deref(),
        Some("Lab, east, not chosen, 1 row"),
        "a member holding a comma is one value"
    );
    assert!(
        said(&ui, &value_of("[\"Lab")).is_none(),
        "no fragment of the JSON"
    );
    assert_eq!(
        said(&ui, &value_of("m2")).as_deref(),
        Some("m2, not chosen, 2 rows")
    );
    assert_eq!(
        said(&ui, &value_of("m3")).as_deref(),
        Some("m3, not chosen, 1 row")
    );
    act(&mut app, &value_of("m2"), None);
    let ui = render(&app);
    assert_eq!(fleet_shown(&app), [1, 2]);
    assert_eq!(
        chip_words(&ui, "aliases").as_deref(),
        Some("ALIASES includes m2")
    );
    act(&mut app, BLANK, None);
    assert_eq!(
        chip_words(&render(&app), "aliases").as_deref(),
        Some("ALIASES includes m2 or is empty")
    );
    assert_eq!(fleet_shown(&app), [1, 2, 3, 4, 5]);
    act(&mut app, &value_of("m2"), None);
    assert_eq!(
        chip_words(&render(&app), "aliases").as_deref(),
        Some("ALIASES is empty"),
        "the empty value alone reads as its one-press choice"
    );
    // A list of records is one value, its cell's words; a list of import coordinates holds
    // each of them.
    act(&mut app, &mark_of("limits"), None);
    let records = r#"[{"by":"m3","ops":3},{"by":"m4","ops":1}]"#;
    assert_eq!(
        said(&render(&app), &value_of(records)).as_deref(),
        Some(format!("{records}, not chosen, 2 rows").as_str())
    );
    act(&mut app, &mark_of("notes_src"), None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, &value_of("xlsx:A1")).as_deref(),
        Some("xlsx:A1, not chosen, 1 row")
    );
    assert_eq!(
        said(&ui, &value_of("xlsx:B2")).as_deref(),
        Some("xlsx:B2, not chosen, 1 row")
    );
}

/// A row shown from elsewhere (a check's finding, a reference) that the filters pass keeps them,
/// though the lifecycle filter left it out: the lifecycle filter alone gives way. A row the
/// filters leave out shows once they are cleared.
/// NEGATIVE CONTROL: with `show_row` clearing the filters wherever the row is not shown (its `passes` clause
/// made `true`), the worker's filter goes: `` the filter the row passes stands left: None
/// right: Some("ROLE is worker") ``.
#[test]
fn showing_a_row_the_filters_pass_keeps_them() {
    let mut snapshot = with_life(fleet(), None);
    snapshot.sheets[0].rows[0]
        .cells
        .insert(aterm_db::typed::LIFECYCLE.to_owned(), text("retired"));
    let mut app = open_with(snapshot);
    act(&mut app, &mark_of("role"), None);
    act(&mut app, &value_of("worker"), None);
    act(&mut app, &mark_of("role"), None);
    assert_eq!(fleet_shown(&app), [1, 4], "the retired worker is left out");
    let show = |app: &mut App, pk: String| {
        let mut view = TablesViewState::default();
        with_view_mut(app, |state| view = std::mem::take(state));
        view.show_row(
            app.native_runtime.tables_mut().unwrap(),
            "machine",
            &pk,
            None,
        );
        with_view_mut(app, |state| *state = view);
    };
    show(&mut app, pk_of(0));
    assert_eq!(
        chip_words(&render(&app), "role").as_deref(),
        Some("ROLE is worker"),
        "the filter the row passes stands"
    );
    assert_eq!(fleet_shown(&app), [0, 1, 4]);
    assert_eq!(view_of(&app).row.as_deref(), Some(pk_of(0).as_str()));
    // A desk: the filter leaves it out, and goes.
    show(&mut app, pk_of(2));
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "the filter is cleared"
    );
    assert!(fleet_shown(&app).contains(&2));
}

/// A row shown from elsewhere lets each of the find, the lifecycle filter and the filters on the
/// columns' values give way only where it leaves the row out: a find that passes the row stands
/// while the filters that alone leave it out are cleared; where the find or the lifecycle filter
/// leaves it out too, that one gives way first, and the filters with it, so the row shows.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `show_row` emptying the find
/// wherever the row is not shown (its `found` test made `self.shown_rows(…)`'s), the find that
/// passes the desk goes with the filters: `` the find that passes the row stands left: "" right:
/// "m2" ``.
#[test]
fn showing_a_row_clears_only_what_leaves_it_out() {
    let mut snapshot = with_life(fleet(), None);
    snapshot.sheets[0].rows[3]
        .cells
        .insert(aterm_db::typed::LIFECYCLE.to_owned(), text("retired"));
    let mut app = open_with(snapshot);
    let show = |app: &mut App, pk: String| {
        let mut view = TablesViewState::default();
        with_view_mut(app, |state| view = std::mem::take(state));
        view.show_row(
            app.native_runtime.tables_mut().unwrap(),
            "machine",
            &pk,
            None,
        );
        with_view_mut(app, |state| *state = view);
    };
    let workers = |app: &mut App| {
        act(app, &mark_of("role"), None);
        act(app, &value_of("worker"), None);
        act(app, &mark_of("role"), None);
    };
    let find = |app: &App| view_of(app).find.value().to_owned();

    // The find holds the desk (its label) and two M2s; the workers' filter alone leaves it out.
    workers(&mut app);
    act(&mut app, FIND, Some(SemanticInput::Text("m2".into())));
    assert_eq!(fleet_shown(&app), [0, 4]);
    show(&mut app, pk_of(2));
    assert_eq!(find(&app), "m2", "the find that passes the row stands");
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "the filter is cleared"
    );
    assert_eq!(fleet_shown(&app), [0, 2, 4]);

    // The find leaves the desk out too: it is emptied, and the filter cleared.
    workers(&mut app);
    act(&mut app, FIND, Some(SemanticInput::Text("build".into())));
    assert_eq!(fleet_shown(&app), Vec::<usize>::new());
    show(&mut app, pk_of(2));
    assert_eq!(find(&app), "");
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "the filter is cleared"
    );
    assert_eq!(fleet_shown(&app), [0, 1, 2, 4, 5], "m3 is retired");

    // The lifecycle filter leaves the retired m3 out too: it shows all rows, and the filter is
    // cleared.
    workers(&mut app);
    show(&mut app, pk_of(3));
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "the filter is cleared"
    );
    assert_eq!(fleet_shown(&app), [0, 1, 2, 3, 4, 5]);
    assert_eq!(view_of(&app).row.as_deref(), Some(pk_of(3).as_str()));
}

/// The cell bar names a selected cell whose row the grid leaves out by what leaves it out: the
/// find (`not among the rows found`, whatever else does too), the filters on the columns' values
/// where the find passes it (`left out by the filters`), the lifecycle filter where both pass it
/// (`not among the retired rows`). The row comes back, and its value with it, once none does.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `left_out_words` saying
/// `NOT_FOUND` of every row left out, the bar blames the find the row passes: `` the filters
/// leave the desk out left: "not among the rows found" right: "left out by the filters" ``.
#[test]
fn the_cell_bar_says_what_leaves_the_selected_row_out() {
    use super::super::native_tables_tests::bar_says;
    let mut app = open_with(with_life(fleet(), None));
    act(&mut app, &format!("tables/cell/{}/role", pk_of(2)), None);
    let state = |app: &App| bar_says(&render(app)).last().cloned().unwrap_or_default();
    assert_eq!(state(&app), "desk");

    act(&mut app, &mark_of("role"), None);
    act(&mut app, &value_of("worker"), None);
    act(&mut app, &mark_of("role"), None);
    assert_eq!(
        bar_says(&render(&app)).first().map(String::as_str),
        Some("m2 \u{b7} Role"),
        "the bar names the cell"
    );
    assert_eq!(
        state(&app),
        "left out by the filters",
        "the filters leave the desk out"
    );

    act(&mut app, FIND, Some(SemanticInput::Text("build".into())));
    assert_eq!(
        state(&app),
        "not among the rows found",
        "the find leaves it out too"
    );

    act(&mut app, FIND, Some(SemanticInput::Text(String::new())));
    act(&mut app, &format!("{REMOVE}role"), None);
    act(&mut app, rows::FILTER, None);
    assert_eq!(
        state(&app),
        "not among the retired rows",
        "the lifecycle filter alone leaves the live desk out"
    );

    // Retired rows, then all of them.
    act(&mut app, rows::FILTER, None);
    assert!(fleet_shown(&app).contains(&2));
    assert_eq!(state(&app), "desk", "the row back, its value with it");
}

static SEEN: aterm_db::typed::Column =
    aterm_db::typed::Column::new("seen", aterm_db::typed::Ty::Time);

/// `At least` and `At most` read a bound as the column's kind does: a whole number; a date,
/// which is its whole day, or a date and its minute. A bound that does not read so (`2026-11-1`
/// on the way to `2026-11-14`, `abc`) is marked invalid, leaves no row out and is no chip's; each
/// field shows the form its words take while it is empty.
/// NEGATIVE CONTROL: with `bound` keeping any words typed (`(!typed.is_empty()).then(…)` for its `Point::of`
/// test), the date cut short stands as a chip: `` and bounds nothing ``.
#[test]
fn a_bound_reads_as_its_columns_kind_and_one_that_does_not_bounds_nothing() {
    let mut snapshot = fleet();
    let sheet = &mut snapshot.sheets[0];
    let mut seen = column("seen", Group::SetByAterm, false);
    seen.kind = ColumnKind::Time;
    seen.compiled = Some(&SEEN);
    sheet.columns.push(seen);
    let times = [
        "2026-11-05 09:00",
        "2026-11-14 23:59",
        "2026-11-15 10:00",
        "",
        "2026-11-14 08:00",
        "2026-11-15 00:00",
    ];
    for (row, at) in sheet.rows.iter_mut().zip(times) {
        row.cells.insert("seen".to_owned(), text(at));
    }
    let mut app = open_with(snapshot);
    let invalid = |app: &App, key: &str| {
        render(app)
            .semantic(&UiKey::new(key.to_owned()))
            .and_then(|node| node.state)
            .is_some_and(|state| state.invalid)
    };
    let hint = |app: &App, key: &str| {
        fn find<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
            if node.key.as_str() == key {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, key))
        }
        match &find(&tree(app).root, key)?.content {
            UiContent::TextField(control) => control.spec.placeholder.clone(),
            _ => None,
        }
    };
    act(&mut app, &mark_of("seen"), None);
    assert_eq!(
        hint(&app, MOST).as_deref(),
        Some("2026-11-16 or 2026-11-16 14:05")
    );
    act(
        &mut app,
        MOST,
        Some(SemanticInput::Text("2026-11-1".into())),
    );
    assert!(invalid(&app, MOST), "a date cut short is marked");
    assert!(!tree_has(&tree(&app), "tables/chips"), "and bounds nothing");
    assert_eq!(fleet_shown(&app), [0, 1, 2, 3, 4, 5]);
    act(
        &mut app,
        MOST,
        Some(SemanticInput::Text("2026-11-14".into())),
    );
    assert!(!invalid(&app, MOST));
    assert_eq!(fleet_shown(&app), [0, 1, 4], "a date is its whole day");
    assert_eq!(
        chip_words(&render(&app), "seen").as_deref(),
        Some("SEEN at most 2026-11-14")
    );
    act(
        &mut app,
        MOST,
        Some(SemanticInput::Text("2026-11-14 12:00".into())),
    );
    assert_eq!(fleet_shown(&app), [0, 4], "a minute bounds by its minute");
    act(&mut app, MOST, Some(SemanticInput::Text(String::new())));
    act(
        &mut app,
        LEAST,
        Some(SemanticInput::Text("2026-11-15".into())),
    );
    assert_eq!(fleet_shown(&app), [2, 5]);
    // A number's bound.
    act(&mut app, &mark_of("ram"), None);
    assert_eq!(hint(&app, LEAST).as_deref(), Some("a whole number"));
    act(&mut app, LEAST, Some(SemanticInput::Text("abc".into())));
    assert!(invalid(&app, LEAST));
    assert!(
        chip_words(&render(&app), "ram").is_none(),
        "no chip says it"
    );
    assert_eq!(fleet_shown(&app), [2, 5], "the time's bound alone stands");
}

/// A count of linked rows shows none as an empty cell, which a bound reads as its 0: `At most 2`
/// keeps the rows no row links to, and `At least 1` leaves them out.
/// NEGATIVE CONTROL: with `counts` answering `false`, `At most 2` leaves out the rows no row links to: ``
/// left: [] right: ["mc-0000000000000000", "mc-0000000000000002"] ``.
#[test]
fn a_linked_counts_empty_cell_is_its_zero_to_a_bound() {
    let mut snapshot = machines();
    snapshot.sheets[0].rows[1]
        .cells
        .insert(LINKED.to_owned(), text("3"));
    let mut app = open_with(snapshot);
    let mark = mark_of(LINKED);
    act(&mut app, &mark, None);
    act(&mut app, MOST, Some(SemanticInput::Text("2".into())));
    assert_eq!(rows_in_order(&app, 3), [pk_of(0), pk_of(2)]);
    act(&mut app, MOST, Some(SemanticInput::Text(String::new())));
    act(&mut app, LEAST, Some(SemanticInput::Text("1".into())));
    assert_eq!(rows_in_order(&app, 3), [pk_of(1)]);
}

/// The open list stays where it opened while it is open: the first filter set brings the
/// chips' line over the grid, and the last taken away (from its chip) takes the line, and
/// neither moves the list under the pointer.
/// NEGATIVE CONTROL: with `list` laying itself out afresh each time (its `open.at` arm made `false && …`),
/// the list moves down under the chips' line: `` the list stays as the chips' line comes left:
/// LogicalRect { x: 205.0, y: 360.0, width: 299.0, height: 28.0 } right: LogicalRect { x: 205.0,
/// y: 320.0, width: 299.0, height: 28.0 } ``.
#[test]
fn the_open_list_stays_where_it_opened_as_the_chips_line_comes_and_goes() {
    let mut app = open_with(fleet());
    act(&mut app, &mark_of("role"), None);
    let build = place(&render(&app), &value_of("build"));
    act(&mut app, &value_of("worker"), None);
    let ui = render(&app);
    assert!(tree_has(&tree(&app), "tables/chips"));
    assert_eq!(
        place(&ui, &value_of("build")),
        build,
        "the list stays as the chips' line comes"
    );
    act(&mut app, &mark_of("role"), None);
    act(&mut app, &format!("{CHIP}role"), None);
    let panel = place(&render(&app), "tables/filter/panel");
    act(&mut app, &value_of("worker"), None);
    let ui = render(&app);
    assert!(
        !tree_has(&tree(&app), "tables/chips"),
        "the last filter gone"
    );
    assert_eq!(
        place(&ui, "tables/filter/panel"),
        panel,
        "the list stays as the chips' line goes"
    );
}

/// Chips that do not fit their line wrap onto the next, which the page counts in its height;
/// every chip's close mark and `Clear filters` stay inside the line.
/// NEGATIVE CONTROL: with `CHIP_LINES` 1, the chips stay on one line: `` the chips wrap onto a second line
/// ``.
#[test]
fn chips_that_do_not_fit_wrap_and_every_close_mark_stays_on_the_page() {
    let mut app = open_with(fleet());
    let work = place(&render_wide(&app, 700.0), "tables/work").height;
    act(&mut app, &mark_of("label"), None);
    for n in 0..6 {
        act(&mut app, &value_of(&format!("m{n}")), None);
    }
    act(&mut app, &mark_of("role"), None);
    for value in ["worker", "build", "desk"] {
        act(&mut app, &value_of(value), None);
    }
    act(&mut app, BLANK, None);
    act(&mut app, &mark_of("ram"), None);
    act(&mut app, LEAST, Some(SemanticInput::Text("16".into())));
    act(&mut app, MOST, Some(SemanticInput::Text("128".into())));
    act(&mut app, &mark_of("chip"), None);
    act(&mut app, HAS, None);
    act(&mut app, &mark_of("chip"), None);
    let ui = render_wide(&app, 700.0);
    assert!(
        tree_has(&tree(&app), "tables/chips/line/1"),
        "the chips wrap onto a second line"
    );
    let line = place(&ui, "tables/chips");
    assert_eq!(
        place(&ui, "tables/work").height,
        work - line.height - 10.0,
        "the grid gives the lines their room"
    );
    for name in ["label", "role", "ram", "chip"] {
        let close = place(&ui, &format!("{REMOVE}{name}"));
        assert!(
            close.right() <= line.right() + 0.5 && close.bottom() <= line.bottom() + 0.5,
            "{name}'s close mark is on the page: {close:?} in {line:?}"
        );
    }
    let clear = place(&ui, CLEAR);
    assert!(
        clear.right() <= line.right() + 0.5 && clear.bottom() <= line.bottom() + 0.5,
        "Clear filters is on the page: {clear:?} in {line:?}"
    );
}

/// While the keys are on the open list, Page Down and Command-Home move them among its lines,
/// and Shift with an arrow reaches neither the grid's selection nor its range: the list keeps
/// the keys, and Escape still closes it.
/// NEGATIVE CONTROL: with the filters' `act` leaving `ExtendFocus` to the page (its arm made `if false`),
/// Shift-Down moves the selection and the keys into the grid: `` left: ("mc-0000000000000001",
/// "label", "tables/cell/mc-0000000000000001/label") right: ("mc-0000000000000000", "label",
/// "tables/filter/has") ``.
#[test]
fn the_page_keys_and_shift_with_an_arrow_stay_with_the_open_list() {
    let mut app = open_with(fleet());
    act(&mut app, &format!("{CELL}{}/label", pk_of(0)), None);
    act(&mut app, &mark_of("role"), None);
    presented(&mut app);
    press_key(&mut app, NamedKey::PageDown);
    let paged = selected(&app).2;
    assert!(
        ![HAS, EMPTY].contains(&paged.as_str()) && paged.starts_with("tables/filter/"),
        "Page Down moves the keys a room's worth down the list: {paged}"
    );
    presented(&mut app);
    press_key(&mut app, NamedKey::PageDown);
    assert_eq!(selected(&app).2, BLANK, "and stops at its foot");
    assert_eq!(view_of(&app).scroll.get(), 0.0, "the grid stays");
    key_with(&mut app, Key::Named(NamedKey::Home), Modifiers::SUPER);
    assert_eq!(selected(&app).2, HAS, "Command-Home its head");
    key_with(&mut app, Key::Named(NamedKey::ArrowDown), Modifiers::SHIFT);
    let view = view_of(&app);
    assert!(view.filters.is_open());
    assert_eq!(
        selected(&app),
        (pk_of(0), "label".to_owned(), HAS.to_owned())
    );
    assert!(view.anchor.is_none(), "no range");
    press_key(&mut app, NamedKey::Escape);
    assert!(!view_of(&app).filters.is_open());
    assert_eq!(selected(&app).2, mark_of("role"));
}

/// The open list is one stop of Tab's: Tab or Shift-Tab from any of its lines closes it and goes
/// on from the mark it opened from, to the control after or before the mark.
/// NEGATIVE CONTROL: with the window's Tab not asking `tab_leaves_list` (its answer made `false && …`),
/// Tab walks on into the list's next line: `` Tab closes the list ``.
#[test]
fn tab_leaves_the_open_list_from_its_mark() {
    let mut app = open_with(fleet());
    let order = presented(&mut app).focus_order;
    let mark = UiKey::new(mark_of("role"));
    let at = order.iter().position(|key| *key == mark).unwrap();
    let (before, after) = (order[at - 1].clone(), order[at + 1].clone());
    let none = Modifiers::empty();
    act(&mut app, &mark_of("role"), None);
    presented(&mut app);
    press_key(&mut app, NamedKey::ArrowDown);
    key_with(&mut app, Key::Named(NamedKey::Tab), none);
    assert!(!view_of(&app).filters.is_open(), "Tab closes the list");
    assert_eq!(
        selected(&app).2,
        after.as_str(),
        "and goes on past the mark"
    );
    act(&mut app, &mark_of("role"), None);
    presented(&mut app);
    press_key(&mut app, NamedKey::ArrowDown);
    key_with(&mut app, Key::Named(NamedKey::Tab), Modifiers::SHIFT);
    assert!(!view_of(&app).filters.is_open());
    assert_eq!(
        selected(&app).2,
        before.as_str(),
        "Shift-Tab goes back past it"
    );
}

/// A header's filter mark is a funnel drawn in lines, never a sort's triangle of bars: in the
/// note's ink, in the primary one while a filter stands; open, its seat is washed and the funnel
/// stays as it is.
/// NEGATIVE CONTROL: with the mark's face drawn as `StyleRef::SortDown`, it is a sort's bars: `` the funnel
/// is drawn in lines: Panel { x: 396.0, y: 172.0, w: 2.0, h: 2.0, radius: 0.0, fill: [208, 208,
/// 208, 255] } ``.
#[test]
fn a_filter_mark_is_a_funnel_beside_a_sorts_triangle() {
    use crate::widget::DrawPrim;
    let mut app = open_with(fleet());
    act(&mut app, "tables/head/role", None);
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let prims = |ui: &CompiledUi, key: &str| {
        ui.painted(roles)
            .into_iter()
            .find(|(painted, _)| painted.as_str() == key)
            .map(|(_, prims)| prims)
            .unwrap_or_else(|| panic!("{key} is painted"))
    };
    let glyph = format!("{}/glyph", mark_of("role"));
    let inks = |ui: &CompiledUi| {
        prims(ui, &glyph)
            .iter()
            .map(|prim| match prim {
                DrawPrim::Line { color, .. } => [color[0], color[1], color[2]],
                other => panic!("the funnel is drawn in lines: {other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let ui = render(&app);
    assert!(
        prims(&ui, "tables/head/role/mark")
            .iter()
            .all(|prim| matches!(prim, DrawPrim::Panel { .. })),
        "the sort's mark is bars"
    );
    let note = crate::native_ui::readable_secondary(&roles);
    assert_eq!(inks(&ui), [note; 5], "five lines in the note's ink");
    act(&mut app, &mark_of("role"), None);
    let ui = render(&app);
    assert!(
        !prims(&ui, &format!("{}/open", mark_of("role"))).is_empty(),
        "open, washed"
    );
    assert_eq!(inks(&ui), [note; 5], "the same funnel");
    act(&mut app, &value_of("worker"), None);
    assert_eq!(
        inks(&render(&app)),
        [roles.text_primary; 5],
        "filtered, in the primary ink"
    );
}

/// A chip's press opens its column's list as the mark's does, and says so: expanded while the
/// list it opened is open, collapsed while the list stands open from the mark.
/// NEGATIVE CONTROL: with each chip built as closed (`chip`'s `open` passed `false`), the chip never says
/// its list is open: `` opened from the chip left: Some(false) right: Some(true) ``.
#[test]
fn a_chip_says_whether_the_list_it_opened_is_open() {
    let mut app = open_with(fleet());
    let expanded = |ui: &CompiledUi, key: &str| {
        ui.semantic(&UiKey::new(key.to_owned()))
            .and_then(|node| node.state)
            .and_then(|state| state.expanded)
    };
    let chip = format!("{CHIP}role");
    act(&mut app, &mark_of("role"), None);
    act(&mut app, &value_of("worker"), None);
    let ui = render(&app);
    assert_eq!(expanded(&ui, &mark_of("role")), Some(true));
    assert_eq!(expanded(&ui, &chip), Some(false));
    act(&mut app, &chip, None);
    let ui = render(&app);
    assert_eq!(expanded(&ui, &chip), Some(true), "opened from the chip");
    assert_eq!(expanded(&ui, &mark_of("role")), Some(false));
    act(&mut app, &chip, None);
    assert_eq!(expanded(&render(&app), &chip), Some(false), "closed again");
}

/// Past the 200 most held values the list says in plain words how many more there are.
/// NEGATIVE CONTROL: with the line's words as they were, `less held`: `` left: Some("5 more values, less
/// held, are not listed") right: Some("5 more values, each in fewer rows, are not listed") ``.
#[test]
fn the_list_says_how_many_values_it_does_not_list() {
    let mut app = open_with(tall_sheet(205, 2));
    act(&mut app, &mark_of("label"), None);
    assert_eq!(
        said(&render(&app), "tables/filter/more").as_deref(),
        Some("5 more values, each in fewer rows, are not listed")
    );
}

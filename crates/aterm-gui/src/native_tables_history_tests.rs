// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page's history drawer on hand-made reads: the list it builds from a table's
//! records, and the page with the drawer in it. Every value is synthetic.

use super::super::native_tables_tests::{
    ENTRY, VIEWPORT, WID, act, author, cell_says, group_style_of, key_with, one_sheet, open_with,
    place, presented, press_key, rect, render, said, settle, style_of, tree, two_macs, view_of,
    window_px, with_view_mut,
};
use super::super::*;
use super::*;
use crate::App;
use crate::native_app::ViewMotionCx;
use crate::native_ui::{CompiledUi, LogicalRect};
use aterm_types::keyboard::{Key, Modifiers, NamedKey};
use aterm_window::event::{ElementState, MouseButton, MouseScrollDelta};

/// The fixture's rows: `m9`, `m55` and `m73`.
const A: &str = "mc-000000000000000a";
const B: &str = "mc-000000000000000b";
const C: &str = "mc-000000000000000c";
/// The first record's claimed time, and a day.
const T0: u64 = 1_700_000_000_000;
const DAY: u64 = 86_400_000;

fn op(n: u8) -> [u8; 32] {
    [n; 32]
}

fn hex(n: u8) -> String {
    aterm_codec::hex::encode_lower(&op(n))
}

/// The key of the line for cell `index` of record `n`.
fn line_key(n: u8, index: usize) -> String {
    format!("tables/history/at/{}/{index}", hex(n))
}

fn set(pk: &str, column: &str, now: &str) -> WrittenCell {
    WrittenCell {
        pk: pk.into(),
        column: column.into(),
        now: now.into(),
    }
}

fn record(n: u8, by: &str, at_ms: u64, cells: Vec<WrittenCell>) -> Written {
    Written {
        operation: op(n),
        by: author(by, at_ms),
        cells,
    }
}

fn shown(text: &str) -> Shown {
    Shown {
        text: text.into(),
        ..Shown::default()
    }
}

fn row(pk: &str, cells: &[(&str, &str)]) -> SheetRow {
    SheetRow {
        pk: pk.into(),
        cells: cells
            .iter()
            .map(|(column, text)| ((*column).to_owned(), shown(text)))
            .collect(),
    }
}

/// A machine sheet of three rows keyed by `label`, with `columns` (the neighbour's sheet gives
/// each column's shape; `password` is its secret reference) and `written` as its records.
fn sheet_of(rows: Vec<SheetRow>, written: Vec<Written>) -> Snapshot {
    let mut snapshot = one_sheet(shown("Spare"));
    let sheet = &mut snapshot.sheets[0];
    let plain = sheet.columns[0].clone();
    let password = sheet.columns[1].clone();
    assert_eq!(password.name, "password", "the neighbour's secret column");
    let named = |name: &str| {
        let mut column = plain.clone();
        column.name = name.into();
        column.label = crate::tables_store::label_of(name);
        column
    };
    sheet.columns = vec![
        named("label"),
        named("nickname"),
        named("location"),
        named("hosts"),
        password,
    ];
    sheet.key = "label".into();
    sheet.rows = rows;
    sheet.written = written;
    snapshot
}

/// The table every test here reads: three Macs' rows and eight records, oldest first.
///
/// 1. `m9` bears row A: label, nickname, location, its lifecycle, an import column (hidden).
/// 2. `m55` bears row B: label, location, its lifecycle.
/// 3. `m9` names A `Backup`; 4. `m73` names A `Spare 2`, neither seeing the other.
/// 5. `m55` clears B's location and sets A's password, which reads as its store entry's name.
/// 6. `m73` bears row C in eleven cells: more than a record shows unfolded.
/// 7. `m9` writes only columns the page hides: A's import column, B's lifecycle again.
/// 8. `m9` moves C, its clock five days behind the first record's.
fn history() -> Snapshot {
    let mut born_c = vec![
        set(C, "label", "m73"),
        set(C, "row", "live"),
        set(C, "location", "office"),
    ];
    born_c.extend((1..=8).map(|n| set(C, "hosts", &format!("+ h{n}"))));
    sheet_of(
        vec![
            row(
                A,
                &[
                    ("label", "m9"),
                    ("nickname", "Backup / Spare 2"),
                    ("location", "rack a"),
                    ("password", ENTRY),
                ],
            ),
            row(B, &[("label", "m55"), ("location", "")]),
            row(C, &[("label", "m73"), ("location", "desk")]),
        ],
        vec![
            record(
                1,
                "m9",
                T0,
                vec![
                    set(A, "label", "m9"),
                    set(A, "nickname", "Spare"),
                    set(A, "location", "rack a"),
                    set(A, "row", "live"),
                    set(A, "src", "import-1"),
                ],
            ),
            record(
                2,
                "m55",
                T0,
                vec![
                    set(B, "label", "m55"),
                    set(B, "location", "rack b"),
                    set(B, "row", "live"),
                ],
            ),
            record(3, "m9", T0 + DAY, vec![set(A, "nickname", "Backup")]),
            record(4, "m73", T0 + DAY, vec![set(A, "nickname", "Spare 2")]),
            record(
                5,
                "m55",
                T0 + 2 * DAY,
                vec![set(B, "location", ""), set(A, "password", ENTRY)],
            ),
            record(6, "m73", T0 + 2 * DAY, born_c),
            record(
                7,
                "m9",
                T0 + 3 * DAY,
                vec![set(A, "src", "import-2"), set(B, "row", "live")],
            ),
            record(8, "m9", T0 - 5 * DAY, vec![set(C, "location", "desk")]),
        ],
    )
}

fn nothing_staged() -> BTreeMap<At, String> {
    BTreeMap::new()
}

fn no_folds() -> BTreeMap<[u8; 32], bool> {
    BTreeMap::new()
}

/// The cell lines of `lines`, top down: (record, cell's place in it, value).
fn cells_of_lines(lines: &[Line]) -> Vec<(u8, usize, String)> {
    lines
        .iter()
        .filter_map(|line| match line {
            Line::One {
                operation, cell, ..
            }
            | Line::Cell {
                operation, cell, ..
            } => Some((operation[0], cell.index, cell.value.clone())),
            _ => None,
        })
        .collect()
}

/// The page's tree in a viewport of `height`.
fn tree_at(app: &App, height: f32) -> UiTree {
    let (instance, view) = app
        .active_native_view(WID)
        .expect("the Tables tab is in front");
    let cx = ViewCx {
        viewport: LogicalRect::new(0.0, 0.0, VIEWPORT.width, height),
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    app.native_runtime.render(instance, view, &cx).unwrap()
}

fn state_of(ui: &CompiledUi, key: &str) -> ControlState {
    ui.semantic(&UiKey::new(key.to_owned()))
        .unwrap_or_else(|| panic!("{key} is listed"))
        .state
        .unwrap_or_else(|| panic!("{key} is a control"))
}

/// Every node under `node` whose group, words or button is set in `style`, by key.
fn styled_in(node: &UiNode, style: StyleRef, out: &mut Vec<String>) {
    let own = match &node.content {
        UiContent::Group(spec) => Some(spec.style),
        UiContent::Text(spec) => Some(spec.style),
        UiContent::Button(control) => Some(control.style),
        _ => None,
    };
    if own == Some(style) {
        out.push(node.key.as_str().to_owned());
    }
    for child in &node.children {
        styled_in(child, style, out);
    }
}

/// Whether two lengths are one, within what a layout's sums round.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

fn has_node(node: &UiNode, key: &str) -> bool {
    node.key.as_str() == key || node.children.iter().any(|child| has_node(child, key))
}

/// The fixed height node `key` of `node` is laid out at, where it has one.
fn fixed_height(node: &UiNode, key: &str) -> Option<f32> {
    if node.key.as_str() == key {
        return match node.layout.height {
            Length::Fixed(height) => Some(height),
            _ => None,
        };
    }
    node.children
        .iter()
        .find_map(|child| fixed_height(child, key))
}

fn selection(app: &App) -> (Option<String>, Option<String>) {
    let view = view_of(app);
    (view.row.clone(), view.column.clone())
}

fn wheel(app: &mut App, lines: i32, key: &str) {
    app.dispatch_native_event(
        WID,
        AppEvent::ScrollAt {
            lines,
            key: UiKey::new(key.to_owned()),
        },
    )
    .expect("the page takes the wheel");
}

/// The drawer lists every cell edit of the table, and a line and its grid cell select each
/// other (LT-39's exit test). At Table scope each cell a record set in a column the grid shows
/// is one line, records newest first in the order the read holds them and never by their
/// claimed times; a press on a line selects its cell in the grid and opens nothing; a cell
/// selected in the grid rings its newest line.
#[test]
fn the_drawer_lists_every_cell_edit_of_the_table_and_a_line_and_its_cell_select_each_other() {
    let snapshot = history();
    let sheet = &snapshot.sheets[0];
    let mut folds = no_folds();
    folds.insert(op(6), true);
    let (tally, listed) = lines(sheet, Scope::Table, None, &nothing_staged(), &folds, None);
    // Each record's cells in the order it set them, the newest record first: record 8, whose
    // writer's clock is five days behind every other, is still the first.
    let mut expected: Vec<(u8, usize, String)> = vec![(8, 0, "desk".into())];
    expected.extend([
        (6, 0, "m73".into()),
        (6, 1, "Row \u{2192} added".into()),
        (6, 2, "office".into()),
    ]);
    expected.extend((1..=8).map(|n| (6, n + 2, format!("+ h{n}"))));
    expected.extend([
        (5, 0, "empty".into()),
        (5, 1, ENTRY.into()),
        (4, 0, "Spare 2".into()),
        (3, 0, "Backup".into()),
        (2, 0, "m55".into()),
        (2, 1, "rack b".into()),
        (2, 2, "Row \u{2192} added".into()),
        (1, 0, "m9".into()),
        (1, 1, "Spare".into()),
        (1, 2, "rack a".into()),
        (1, 3, "Row \u{2192} added".into()),
    ]);
    assert_eq!(cells_of_lines(&listed), expected);
    assert_eq!(
        tally,
        Tally {
            all: 7,
            records: 7,
            cells: 23
        },
        "record 7 set only columns the page hides"
    );
    // A day head stands where the claimed day changes, in the list's order.
    let days: Vec<usize> = listed
        .iter()
        .enumerate()
        .filter_map(|(at, line)| matches!(line, Line::Day(_)).then_some(at))
        .collect();
    assert_eq!(days.len(), 4, "8; 6 and 5; 4 and 3; 2 and 1: {listed:?}");
    assert!(matches!(listed.last(), Some(Line::Start)));

    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    // Record 6 folded: record 5's head stands near the list's top.
    let folded = render(&app);
    act(&mut app, &format!("tables/history/fold/{}", hex(6)), None);
    let ui = render(&app);
    for (n, index, value) in &expected {
        let spoken = said(&ui, &line_key(*n, *index)).expect("each cell edit is a line");
        assert!(spoken.ends_with(&format!(": {value}")), "{spoken}");
    }
    assert_eq!(
        tally.words(Scope::Table),
        "7 records \u{b7} 23 cells set \u{b7} in the registry's order; a time is the writing \
         machine's claim"
    );
    // A record that set cells of two rows says so on its head; each line under it is named by
    // its writer and time as a one-cell record's line is, its head off the screen or not.
    assert_eq!(
        said(&folded, &format!("tables/history/fold/{}/count", hex(5))).as_deref(),
        Some("\u{b7} 2 cells in 2 rows")
    );
    let under = said(&ui, &line_key(5, 0)).unwrap();
    assert!(under.starts_with("m55 \u{b7} "), "{under}");
    assert!(under.contains(" \u{b7} claimed "), "{under}");
    assert!(under.ends_with(", m55 \u{b7} Location: empty"), "{under}");
    // The head says them; one too narrow for all its words keeps the counts.
    assert!(
        said(&ui, "tables/history/count")
            .unwrap()
            .starts_with("7 records \u{b7} 23 cells set"),
    );

    // A press on a line selects its cell, as a press on the cell does, and opens nothing.
    assert_eq!(selection(&app), (None, None));
    act(&mut app, &line_key(2, 1), None);
    assert_eq!(selection(&app), (Some(B.into()), Some("location".into())));
    let cell = format!("tables/cell/{B}/location");
    assert_eq!(
        view_of(&app)
            .common
            .last_focus
            .as_ref()
            .map(|k| k.as_str().to_owned()),
        Some(cell.clone()),
        "the keys are the cell's"
    );
    assert_eq!(
        group_style_of(&tree(&app), &format!("{cell}/focus")),
        Some(StyleRef::CellSelected)
    );
    let ui = render(&app);
    assert!(said(&ui, EDIT).is_none(), "no cell opened");
    assert_eq!(
        said(&ui, "tables/pane/id").as_deref(),
        Some(B),
        "the pane follows"
    );
    // A birth's line selects its row's key cell.
    act(&mut app, &line_key(1, 3), None);
    assert_eq!(selection(&app), (Some(A.into()), Some("label".into())));

    // A cell selected in the grid rings its newest line: record 4's, not record 3's.
    act(&mut app, &format!("tables/cell/{A}/nickname"), None);
    let drawn = tree(&app);
    assert_eq!(
        group_style_of(&drawn, &format!("{}/focus", line_key(4, 0))),
        Some(StyleRef::CellSelected),
        "the page's neutral selection ring"
    );
    for (n, index) in [(3, 0), (1, 1)] {
        assert!(!has_node(
            &drawn.root,
            &format!("{}/focus", line_key(n, index))
        ));
    }
    // The ring is painted over the line as the grid's cell wears it: no line says it is
    // selected, a state a press paints in the accent.
    assert!(!state_of(&render(&app), &line_key(4, 0)).selected);
    assert_no_selected_paint(&render(&app));
}

/// No control of the drawer carries a selected state: a selected press or segment paints the
/// accent's chip, and the drawer's marks are the page's neutral ones.
fn assert_no_selected_paint(ui: &CompiledUi) {
    let selected: Vec<&str> = ui
        .semantics
        .iter()
        .filter(|node| node.key.as_str().starts_with("tables/history"))
        .filter(|node| node.state.is_some_and(|state| state.selected))
        .map(|node| node.key.as_str())
        .collect();
    assert!(selected.is_empty(), "{selected:?}");
}

/// Whether segment `key` says it is the current choice.
fn on(ui: &CompiledUi, key: &str) -> bool {
    ui.semantic(&UiKey::new(key.to_owned()))
        .unwrap_or_else(|| panic!("{key} is listed"))
        .value
        == SemanticValue::Bool(true)
}

/// At Row scope the drawer lists what the pane's history listed of the row: each record that
/// set one of its cells, newest first, by its writer, where the history places it and the time
/// its writer claimed, each offering As of; a birth reads `Row → added`, and the lifecycle and
/// import columns the page hides print nothing. No line carries an earlier value.
#[test]
fn row_scope_lists_the_rows_writes_as_the_panes_history_did() {
    let snapshot = history();
    let sheet = &snapshot.sheets[0];
    let (tally, listed) = lines(
        sheet,
        Scope::Row,
        Some((A, Some("location"))),
        &nothing_staged(),
        &no_folds(),
        None,
    );
    assert_eq!(
        cells_of_lines(&listed),
        vec![
            (5, 1, ENTRY.to_owned()),
            (4, 0, "Spare 2".into()),
            (3, 0, "Backup".into()),
            (1, 0, "m9".into()),
            (1, 1, "Spare".into()),
            (1, 2, "rack a".into()),
            (1, 3, "Row \u{2192} added".into()),
        ],
        "record 7's hidden columns and the other rows' cells are not the row's lines"
    );
    assert_eq!(
        tally,
        Tally {
            all: 7,
            records: 4,
            cells: 7
        }
    );
    // Record 1 set four of the row's cells: a head over a line for each, too few to fold.
    assert!(listed.iter().any(|line| matches!(
        line,
        Line::Head {
            operation,
            cells: 4,
            rows: 1,
            open: None,
            ..
        } if *operation == op(1)
    )));
    // No line says what a value replaced: the only arrow is a birth's own word.
    for (_, _, value) in cells_of_lines(&listed) {
        assert!(
            !value.contains('\u{2192}') || value == "Row \u{2192} added",
            "{value}"
        );
    }

    let mut app = open_with(snapshot);
    act(&mut app, &format!("tables/cell/{A}/location"), None);
    act(&mut app, "tables/pane/history", None);
    let ui = render(&app);
    assert_eq!(
        tally.words(Scope::Row),
        "4 of 7 records \u{b7} 7 cells set \u{b7} in the registry's order; a time is the \
         writing machine's claim"
    );
    assert!(
        said(&ui, "tables/history/count")
            .unwrap()
            .starts_with("4 of 7 records \u{b7} 7 cells set"),
    );
    // Who, where the history places the record and the time its writer claimed, then the cell.
    let line = said(&ui, &line_key(4, 0)).unwrap();
    let (writer, cell) = line.split_once(", ").unwrap();
    assert!(writer.starts_with("m73 \u{b7} "), "{line}");
    assert!(writer.contains(" \u{b7} claimed "), "{line}");
    assert_eq!(cell, "m9 \u{b7} Nickname: Spare 2");
    // The head of a record of several cells says the same of its writer.
    let head = format!("tables/history/fold/{}", hex(1));
    assert_eq!(said(&ui, &format!("{head}/who")).as_deref(), Some("m9"));
    let when = said(&ui, &format!("{head}/when")).unwrap();
    assert!(when.starts_with("\u{b7} "), "{when}");
    assert!(when.contains(" \u{b7} claimed "), "{when}");
    assert_eq!(
        said(&ui, &format!("{head}/count")).as_deref(),
        Some("\u{b7} 4 cells")
    );
    let birth = said(&ui, &line_key(1, 3)).unwrap();
    assert!(birth.starts_with("m9 \u{b7} "), "{birth}");
    assert!(birth.ends_with(", m9: Row \u{2192} added"), "{birth}");
    // Each record offers the table as of it; none of another row's does here.
    for n in [5, 4, 3, 1] {
        let as_of = said(&ui, &format!("tables/history/asof/{}", hex(n))).unwrap();
        assert!(as_of.starts_with("The table as of "), "{as_of}");
    }
    for n in [8, 6, 2] {
        assert!(said(&ui, &format!("tables/history/asof/{}", hex(n))).is_none());
    }
}

/// At Cell scope the list is the sequence of values written to the cell, newest first: both
/// values of a conflict are there, each by its writer, and nothing says one replaced another.
#[test]
fn cell_scope_lists_every_value_written_to_the_cell_and_both_of_a_conflict() {
    let snapshot = history();
    let (tally, listed) = lines(
        &snapshot.sheets[0],
        Scope::Cell,
        Some((A, Some("nickname"))),
        &nothing_staged(),
        &no_folds(),
        None,
    );
    assert_eq!(
        cells_of_lines(&listed),
        vec![
            (4, 0, "Spare 2".to_owned()),
            (3, 0, "Backup".into()),
            (1, 1, "Spare".into()),
        ]
    );
    assert_eq!(tally.records, 3);
    let writers: Vec<String> = listed
        .iter()
        .filter_map(|line| match line {
            Line::One { by, .. } => Some(by.who()),
            _ => None,
        })
        .collect();
    assert_eq!(writers, ["m73", "m9", "m9"]);

    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    act(&mut app, "tables/history/scope/cell", None);
    // Nothing selected: the scope has no cell to list.
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/history/unselected").as_deref(),
        Some("Select a cell to see its history")
    );
    assert!(said(&ui, &line_key(4, 0)).is_none());
    assert!(on(&ui, "tables/history/scope/cell"));
    assert!(!on(&ui, "tables/history/scope/table"));
    // The list follows the grid's selection.
    act(&mut app, &format!("tables/cell/{A}/nickname"), None);
    let ui = render(&app);
    for (n, index) in [(4, 0), (3, 0), (1, 1)] {
        assert!(said(&ui, &line_key(n, index)).is_some());
    }
    assert!(said(&ui, &line_key(1, 0)).is_none(), "another cell's line");
    act(&mut app, &format!("tables/cell/{B}/location"), None);
    let ui = render(&app);
    assert!(said(&ui, &line_key(4, 0)).is_none());
    assert_eq!(
        said(&ui, &line_key(5, 0)).unwrap().rsplit(", ").next(),
        Some("m55 \u{b7} Location: empty")
    );
    assert_eq!(
        style_of(&tree(&app), &format!("{}/value", line_key(5, 0))),
        Some(StyleRef::Faint),
        "a cleared cell's word is set faint"
    );
}

/// A record that set more than eight cells in the scope starts folded: its head is a button
/// that says whether it is open, and a press opens and closes it. One of eight or fewer has no
/// fold.
#[test]
fn a_record_over_eight_cells_starts_folded_and_opens() {
    let snapshot = history();
    let sheet = &snapshot.sheets[0];
    let (_, folded) = lines(
        sheet,
        Scope::Table,
        None,
        &nothing_staged(),
        &no_folds(),
        None,
    );
    assert!(folded.iter().any(|line| matches!(
        line,
        Line::Head {
            operation,
            cells: 11,
            rows: 1,
            open: Some(false),
            ..
        } if *operation == op(6)
    )));
    assert!(
        cells_of_lines(&folded).iter().all(|(n, ..)| *n != 6),
        "a folded record lists no cell"
    );
    // Eight cells in one record do not fold; a ninth does.
    for (cells, folds) in [(FOLD_OVER, false), (FOLD_OVER + 1, true)] {
        let written = vec![record(
            1,
            "m9",
            T0,
            (0..cells)
                .map(|n| set(A, "hosts", &format!("+ h{n}")))
                .collect(),
        )];
        let one = sheet_of(vec![row(A, &[("label", "m9")])], written);
        let (_, listed) = lines(
            &one.sheets[0],
            Scope::Table,
            None,
            &nothing_staged(),
            &no_folds(),
            None,
        );
        let open = listed.iter().find_map(|line| match line {
            Line::Head { open, .. } => Some(*open),
            _ => None,
        });
        assert_eq!(open, Some(folds.then_some(false)), "{cells} cells");
    }

    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    let head = format!("tables/history/fold/{}", hex(6));
    let ui = render(&app);
    assert_eq!(state_of(&ui, &head).expanded, Some(false));
    let spoken = said(&ui, &head).unwrap();
    assert!(spoken.starts_with("m73 \u{b7} "), "{spoken}");
    assert!(spoken.ends_with(" \u{b7} 11 cells"), "{spoken}");
    assert!(said(&ui, &line_key(6, 0)).is_none());
    // Its As of is offered folded or open.
    assert!(said(&ui, &format!("tables/history/asof/{}", hex(6))).is_some());

    act(&mut app, &head, None);
    let ui = render(&app);
    assert_eq!(state_of(&ui, &head).expanded, Some(true));
    for index in 0..11 {
        assert!(said(&ui, &line_key(6, index)).is_some(), "cell {index}");
    }
    act(&mut app, &head, None);
    let ui = render(&app);
    assert_eq!(state_of(&ui, &head).expanded, Some(false));
    assert!(said(&ui, &line_key(6, 0)).is_none());

    // Folded over the selected cell's newest line, the head wears the ring.
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    assert_eq!(
        group_style_of(&tree(&app), &format!("{head}/focus")),
        Some(StyleRef::CellSelected)
    );
    assert_no_selected_paint(&render(&app));
}

/// Changes not written come first, under `NOT WRITTEN`, in the one blue the page keeps for
/// that, each with its Undo; the scope narrows them like the rest, and nothing else in the
/// drawer wears the blue.
#[test]
fn changes_not_written_are_pinned_first_with_undo_and_wear_unwritten() {
    let mut app = open_with(history());
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", B, "location", "rack z".into());
    tables.stage("machine", A, "nickname", String::new());
    act(&mut app, "tables/history/show", None);

    let tables = app.native_runtime.tables_mut().unwrap();
    let sheet = tables.sheet("machine").unwrap();
    let (_, listed) = lines(sheet, Scope::Table, None, &tables.staged, &no_folds(), None);
    assert_eq!(
        listed[..3],
        [
            Line::NotWrittenHead,
            Line::NotWritten {
                pk: A.into(),
                column: "nickname".into(),
                place: "m9 \u{b7} Nickname".into(),
                text: "empty".into(),
            },
            Line::NotWritten {
                pk: B.into(),
                column: "location".into(),
                place: "m55 \u{b7} Location".into(),
                text: "rack z".into(),
            },
        ]
    );
    assert!(matches!(listed[3], Line::Day(_)), "then the records");
    // The scope narrows them: row A's list has A's alone, a cell's only its own.
    let (_, of_row) = lines(
        sheet,
        Scope::Row,
        Some((A, None)),
        &tables.staged,
        &no_folds(),
        None,
    );
    let staged_in = |lines: &[Line]| {
        lines
            .iter()
            .filter(|line| matches!(line, Line::NotWritten { .. }))
            .count()
    };
    assert_eq!(staged_in(&of_row), 1);
    let (_, of_cell) = lines(
        sheet,
        Scope::Cell,
        Some((A, Some("location"))),
        &tables.staged,
        &no_folds(),
        None,
    );
    assert_eq!(staged_in(&of_cell), 0);
    assert!(!of_cell.contains(&Line::NotWrittenHead));

    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/history/unwritten").as_deref(),
        Some("NOT WRITTEN")
    );
    let staged = format!("tables/history/staged/{B}/location");
    assert_eq!(
        said(&ui, &staged).as_deref(),
        Some("m55 \u{b7} Location: rack z, not written")
    );
    let drawn = tree(&app);
    assert_eq!(
        group_style_of(&drawn, &format!("{staged}/seat")),
        Some(StyleRef::Unwritten),
        "the wash"
    );
    assert_eq!(
        style_of(&drawn, &format!("{staged}/text")),
        Some(StyleRef::Unwritten),
        "the ink"
    );
    // Blue is the staged lines' alone.
    let mut blue = Vec::new();
    styled_in(&drawn.root, StyleRef::Unwritten, &mut blue);
    let in_drawer: Vec<&String> = blue
        .iter()
        .filter(|key| key.starts_with("tables/history"))
        .collect();
    assert!(!in_drawer.is_empty());
    assert!(
        in_drawer
            .iter()
            .all(|key| key.starts_with("tables/history/staged/")),
        "{in_drawer:?}"
    );

    // Undo takes the staged cell back, whichever row the grid has selected.
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    let undo = format!("tables/history/undo/{B}/location");
    assert_eq!(
        said(&render(&app), &undo).as_deref(),
        Some("Undo m55 \u{b7} Location")
    );
    act(&mut app, &undo, None);
    let tables = app.native_runtime.tables_mut().unwrap();
    assert_eq!(
        tables.staged.keys().cloned().collect::<Vec<_>>(),
        [("machine".to_owned(), A.to_owned(), "nickname".to_owned())]
    );
    // A press on a staged line selects its cell.
    act(
        &mut app,
        &format!("tables/history/staged/{A}/nickname"),
        None,
    );
    assert_eq!(selection(&app), (Some(A.into()), Some("nickname".into())));
    act(&mut app, &format!("tables/history/undo/{A}/nickname"), None);
    let drawn = tree(&app);
    let mut blue = Vec::new();
    styled_in(&drawn.root, StyleRef::Unwritten, &mut blue);
    assert!(blue.is_empty(), "nothing staged, nothing blue: {blue:?}");
    assert!(said(&render(&app), "tables/history/unwritten").is_none());
}

/// A record's As of in the drawer asks for the table as of that record, by the rule the pane's
/// had: off while cells are staged, off for the record the page already shows, whose line is
/// set as the page's current choice; the drawer still lists, and still offers the others.
#[test]
fn as_of_from_a_line_asks_for_that_record_and_marks_the_one_shown() {
    let mut app = open_with(history());
    act(&mut app, "tables/history/show", None);
    let as_of = |n: u8| format!("tables/history/asof/{}", hex(n));
    assert!(state_of(&render(&app), &as_of(3)).enabled);

    // With a cell staged no record offers its point: a past table could not take the edit.
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", B, "location", "rack z".into());
    assert!(!state_of(&render(&app), &as_of(3)).enabled);
    act(&mut app, &format!("tables/history/undo/{B}/location"), None);

    act(&mut app, &as_of(3), None);
    let tables = app.native_runtime.tables_mut().unwrap();
    assert_eq!(tables.asked, 1, "the reader was asked for record 3's point");
    // The reader's answer: the table as of record 3.
    let mut past = history();
    past.at = Some(AsOf {
        operation: op(3),
        by: Some(author("m9", T0 + DAY)),
    });
    tables.take(Reply::Read {
        read: Ok(past),
        moved: true,
    });
    let ui = render(&app);
    assert!(said(&ui, "tables/asof").unwrap().starts_with("As of m9 "));
    assert!(!state_of(&ui, &as_of(3)).enabled, "the point shown");
    assert!(state_of(&ui, &as_of(4)).enabled, "another point");
    assert!(state_of(&ui, &as_of(8)).enabled, "a record after the point");
    let drawn = tree(&app);
    assert_eq!(
        style_of(&drawn, &format!("{}/who", line_key(3, 0))),
        Some(StyleRef::Primary)
    );
    assert_eq!(
        style_of(&drawn, &format!("{}/who", line_key(4, 0))),
        Some(StyleRef::Plain)
    );
}

/// A line whose row the grid does not show changes no selection: the head says which row is
/// not shown, until the selection or the find is another. Find narrows the rows; a row the read
/// no longer lists is named by the key text its records wrote.
#[test]
fn a_line_of_a_row_hidden_by_find_says_so_and_changes_no_selection() {
    let mut snapshot = history();
    // Row C is no longer live: the read lists its records and not the row.
    snapshot.sheets[0].rows.retain(|row| row.pk != C);
    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    act(&mut app, &format!("tables/cell/{B}/location"), None);
    act(&mut app, FIND, Some(SemanticInput::Text("m55".into())));
    assert!(said(&render(&app), &format!("tables/cell/{A}/nickname")).is_none());

    act(&mut app, &line_key(3, 0), None);
    assert_eq!(selection(&app), (Some(B.into()), Some("location".into())));
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("m9 is not in the rows shown")
    );
    // A line of a row the grid shows selects it, and the head counts again.
    act(&mut app, &line_key(2, 0), None);
    assert_eq!(selection(&app), (Some(B.into()), Some("label".into())));
    assert!(
        said(&render(&app), "tables/history/count")
            .unwrap()
            .starts_with("7 records")
    );
    // The note stands only under the find it was pressed under.
    act(&mut app, &line_key(3, 0), None);
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("m9 is not in the rows shown")
    );
    act(&mut app, FIND, Some(SemanticInput::Text(String::new())));
    assert!(
        said(&render(&app), "tables/history/count")
            .unwrap()
            .starts_with("7 records")
    );
    act(&mut app, &line_key(3, 0), None);
    assert_eq!(selection(&app), (Some(A.into()), Some("nickname".into())));

    // A row that is no longer live.
    act(&mut app, &line_key(8, 0), None);
    assert_eq!(selection(&app), (Some(A.into()), Some("nickname".into())));
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("m73 is not in the rows shown")
    );
}

/// A secret cell's line holds what the read holds for the grid, the store entry's name or the
/// mask, and nothing else: the drawer makes no read, so it has no other words to print.
#[test]
fn a_secret_cells_line_shows_only_the_grids_text() {
    const MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}";
    let mut snapshot = history();
    snapshot.sheets[0].written.push(record(
        9,
        "m55",
        T0 + 4 * DAY,
        vec![set(B, "password", MASK)],
    ));
    let sheet = &snapshot.sheets[0];
    let (_, listed) = lines(
        sheet,
        Scope::Table,
        None,
        &nothing_staged(),
        &no_folds(),
        None,
    );
    let passwords: Vec<(String, String)> = listed
        .iter()
        .filter_map(|line| match line {
            Line::One { cell, .. } | Line::Cell { cell, .. } if cell.column == "password" => {
                Some((cell.place.clone(), cell.value.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        passwords,
        [
            ("m55 \u{b7} Password".to_owned(), MASK.to_owned()),
            ("m9 \u{b7} Password".to_owned(), ENTRY.to_owned()),
        ]
    );
    // What the drawer says of the cell is what the grid says of it.
    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    let ui = render(&app);
    let grid = said(&ui, &format!("tables/cell/{A}/password")).unwrap();
    assert!(grid.ends_with(&format!(": {ENTRY}")), "{grid}");
    let line = said(&ui, &line_key(5, 1)).unwrap();
    assert!(
        line.ends_with(&format!(", m9 \u{b7} Password: {ENTRY}")),
        "{line}"
    );
    let line = said(&ui, &line_key(9, 0)).unwrap();
    assert!(
        line.ends_with(&format!("m55 \u{b7} Password: {MASK}")),
        "{line}"
    );
}

/// Closed, as the page opens, the drawer is one line of the grid's row height: the newest
/// record that set a cell the grid shows, by who, where the history places it and how many
/// cells, and Show history; a table with no record says so.
#[test]
fn the_closed_drawer_says_the_newest_record() {
    let mut app = open_with(history());
    let ui = render(&app);
    assert!(!view_of(&app).history.open, "closed until it is asked for");
    assert_eq!(
        said(&ui, "tables/history/title").as_deref(),
        Some("History")
    );
    let newest = said(&ui, "tables/history/newest").unwrap();
    assert!(newest.starts_with("m9 \u{b7} "), "{newest}");
    assert!(newest.ends_with(" \u{b7} 1 cell"), "{newest}");
    assert!(!newest.contains("claimed"), "{newest}");
    assert_eq!(
        said(&ui, "tables/history/show").as_deref(),
        Some("Show history")
    );
    assert!(said(&ui, "tables/history/start").is_none(), "no list");
    assert_eq!(rect(&ui, "tables/history").height, row_height());
    // Record 7 is newer in the read but set nothing the grid shows; without record 8 the
    // newest is record 6 and its eleven cells.
    let mut snapshot = history();
    snapshot.sheets[0].written.pop();
    assert!(
        newest_words(&snapshot.sheets[0], u64::MAX).ends_with(" \u{b7} 11 cells"),
        "{}",
        newest_words(&snapshot.sheets[0], u64::MAX)
    );

    snapshot.sheets[0].written.clear();
    app.native_runtime.tables_mut().unwrap().show(snapshot);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/history/newest").as_deref(),
        Some("No records")
    );
    act(&mut app, "tables/history/show", None);
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/history/count").as_deref(),
        Some("No records")
    );
    assert_eq!(
        said(&ui, "tables/history/start").as_deref(),
        Some("Start of the records read here")
    );
    act(&mut app, "tables/history/hide", None);
    assert!(said(&render(&app), "tables/history/start").is_none());
}

/// The drawer takes its height from the grid's share of the page and nothing else moves. Open,
/// it has about two fifths of the main column while the grid keeps six rows and it has five
/// lines; in a share too small for both the grid keeps four rows and the drawer has the rest;
/// the cell bar's row over the grid is the work's, and the page's least for the work stands.
/// Its list's room is whole lines, at least one: rounded up where the grid can give that and
/// keep what it keeps, else down; in a share whose room over the least holds no whole line, the
/// drawer is its head alone.
/// NEGATIVE CONTROL: with `heights` giving the drawer what it would have, unrounded (its body
/// before this rule, `… .min(room - floor).max(row_height())`), `` 12 lines and a part:
/// 12.046154 ``.
#[test]
fn the_grid_keeps_its_rows_when_the_drawer_opens() {
    let rows = |grid: f32| (grid - grid_height(0.0)) / row_height();
    let lines_in = |drawer: f32| (drawer - drawer_height(0.0)) / line_height();
    let whole = |drawer: f32| {
        let lines = lines_in(drawer);
        assert!(
            near(lines, lines.round()) && lines.round() >= 1.0,
            "{} lines and a part: {lines}",
            lines.floor()
        );
        lines.round()
    };
    // Closed: one row's height, and the gap, off the grid.
    assert_eq!(
        heights(700.0, 0.0, 0.0, 868.0, false),
        (700.0 - GAP - row_height(), row_height())
    );
    // Tall: two fifths of the column, up to its next whole line.
    let (grid, drawer) = heights(700.0, 0.0, 0.0, 868.0, true);
    whole(drawer);
    assert!(
        drawer >= 868.0 * 0.4 && drawer < 868.0 * 0.4 + line_height(),
        "{drawer}"
    );
    assert!(near(grid + GAP + drawer, 700.0));
    assert!(rows(grid) >= 6.0);
    // Shorter: the drawer gives way so the grid keeps six rows, while it has five lines; down
    // to its whole line where up would take the grid's sixth row.
    let both = grid_height(6.0) + drawer_height(5.0) + GAP + 20.0;
    let (grid, drawer) = heights(both, 0.0, 0.0, both + 80.0, true);
    assert!(near(grid + GAP + drawer, both));
    assert!(rows(grid) >= 6.0, "{}", rows(grid));
    assert!(whole(drawer) >= 5.0, "{}", lines_in(drawer));
    assert!(drawer < (both + 80.0) * 0.4, "it gave way to the grid");
    // Too short for both: the grid keeps four rows, the drawer has the rest, up to its next
    // whole line, which the grid gives.
    let both = grid_height(6.0) + drawer_height(5.0) + GAP - 1.0;
    let (grid, drawer) = heights(both, 0.0, 0.0, both + 80.0, true);
    let rest = both - GAP - grid_height(4.0);
    assert_eq!(drawer, drawer_height(whole(drawer)));
    assert!(drawer >= rest && drawer < rest + line_height(), "{drawer}");
    assert!(near(grid + GAP + drawer, both));
    assert!(rows(grid) > 3.0 && rows(grid) <= 4.0, "{}", rows(grid));
    // The cell bar's row is the work's, over the grid's rows.
    let bar = row_height();
    let both = bar + grid_height(6.0) + drawer_height(5.0) + GAP - 1.0;
    let (work, drawer) = heights(both, bar, 0.0, both + 80.0, true);
    whole(drawer);
    assert!(rows(work - bar) > 3.0 && rows(work - bar) <= 4.0);
    assert!(near(work + GAP + drawer, both));
    // So short that the grid's four rows leave the drawer no line: it has one, which the grid
    // gives down to the page's least.
    let least = grid_height(1.0);
    let (grid, drawer) = heights(grid_height(4.0), 0.0, least, 300.0, true);
    assert_eq!(drawer, drawer_height(1.0));
    assert!(grid >= least);
    // Where the least leaves room for one whole line, the open drawer has it; a point less and
    // it is its head alone, as it is closed: never a part of a line. Whether it stands at all is
    // the page's to judge, from the window and the filters' chips, and the page asks only where
    // the share holds the least and the head: in a share it would not give, the drawer is still
    // never cut short of its head.
    let share = least + GAP + drawer_height(1.0);
    assert_eq!(
        heights(share, 0.0, least, 300.0, true),
        (least, drawer_height(1.0))
    );
    assert_eq!(
        heights(share - 1.0, 0.0, least, 300.0, true).1,
        row_height()
    );
    for open in [false, true] {
        let share = least + GAP + row_height() - 1.0;
        assert_eq!(heights(share, 0.0, least, 300.0, open).1, row_height());
        let share = least + GAP + row_height();
        assert_eq!(
            heights(share, 0.0, least, 300.0, open),
            (least, row_height())
        );
    }

    // On the page: the grid above, the line under it, the drawer under that, the tools where
    // they were.
    let mut app = open_with(history());
    let closed = render(&app);
    let (find, grid_was, work_was) = (
        rect(&closed, FIND),
        rect(&closed, "tables/grid"),
        rect(&closed, "tables/work"),
    );
    act(&mut app, "tables/history/show", None);
    let open = render(&app);
    let (grid, drawer) = (rect(&open, "tables/grid"), rect(&open, "tables/history"));
    assert_eq!(rect(&open, FIND), find, "nothing else moves");
    assert_eq!(grid.y, grid_was.y);
    let under = rect(&open, "tables/said");
    assert!(under.y >= grid.bottom(), "{under:?} under {grid:?}");
    assert!(
        near(drawer.y, under.bottom() + GAP),
        "{drawer:?} under {under:?}"
    );
    assert!(
        near(drawer.bottom(), rect(&closed, "tables/history").bottom()),
        "the drawer's foot stays where it was"
    );
    assert!(grid.height < grid_was.height);
    // `grid` counts its own rows: the room it laid its rows out in.
    assert!(view_of(&app).room.get() >= 6.0 * row_height());
    assert!(view_of(&app).history.room.get() >= 5.0 * line_height());
    // A short window: four rows for the grid. What the page gives the work and the drawer is
    // the window less what stands beside them, which the drawer does not change.
    let bar = work_was.height - grid_was.height;
    let others = VIEWPORT.height - (work_was.height + GAP + rect(&closed, "tables/history").height);
    let short = others + bar + grid_height(4.0) + GAP + drawer_height(3.0);
    tree_at(&app, short)
        .compile(LogicalRect::new(0.0, 0.0, VIEWPORT.width, short))
        .unwrap();
    assert!(
        near(view_of(&app).room.get(), 4.0 * row_height()),
        "{}",
        view_of(&app).room.get()
    );
    assert!(view_of(&app).history.room.get() > 0.0);
    // A window too short for the drawer's line, the cell bar and the grid under the bar: the
    // drawer stands, the one way to the records, and the bar gives way; the grid keeps three
    // lines. Both are judged from the window and the filters' chips (none stands here), as if a
    // change waited to be written and the line under the grid stood three of its lines high:
    // with a change staged, the room that judgement reads is the share the page gives the work
    // and the drawer, less what three of the line's lines would take beyond the line's own
    // height.
    let three = 3.0 * 30.0 * scale().max(1.0);
    act(&mut app, "tables/history/hide", None);
    act(&mut app, &format!("tables/cell/{A}/nickname"), None);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", B, "location", "rack z".into());
    let in_window = |height: f32| {
        tree_at(&app, height)
            .compile(LogicalRect::new(0.0, 0.0, VIEWPORT.width, height))
            .unwrap()
    };
    // What stands beside the work and the drawer with this cell's line under the grid and the
    // write bar under that, and what three lines of the line take beyond it.
    let full = render(&app);
    assert!(said(&full, WRITE).is_some(), "the write bar stands");
    let others = VIEWPORT.height
        - (rect(&full, "tables/work").height + GAP + rect(&full, "tables/history").height);
    let judged = 3.0 * said_line() - rect(&full, "tables/said").height;
    assert!(judged >= 0.0, "a line of one row: {judged}");
    let others = others + judged;
    let ui = in_window(others + 150.0);
    assert!(
        said(&ui, "tables/history/show").is_some(),
        "the drawer stands"
    );
    assert!(
        said(&ui, "tables/pane/history").is_some(),
        "and the pane's way to it"
    );
    assert!(
        ui.semantic(&UiKey::new("tables/cellbar".to_owned()))
            .is_none(),
        "the bar gave way"
    );
    assert!(rect(&ui, "tables/grid").height >= three - 0.01);
    // Shorter, the grid's three lines leave the drawer less than its line: it gives way rather
    // than stand cut short or run past the window's foot, and so does the pane's way to it.
    let ui = in_window(others + three + GAP + row_height() - 1.0);
    assert!(said(&ui, "tables/history/show").is_none());
    assert!(said(&ui, "tables/pane/history").is_none());
    assert!(!view_of(&app).history.drawn.get());
    let ui = in_window(others + three + GAP + row_height() + 1.0);
    assert!(near(rect(&ui, "tables/history").height, row_height()));
}

/// The pane lists no history: where it did, one button opens the drawer on the selected row.
#[test]
fn the_pane_lists_no_history_and_opens_the_drawer_at_row_scope() {
    let mut app = open_with(history());
    act(&mut app, &format!("tables/cell/{A}/nickname"), None);
    let ui = render(&app);
    assert!(
        ui.semantics.iter().all(|node| {
            let key = node.key.as_str();
            !key.starts_with("tables/pane/written") && !key.starts_with("tables/pane/asof")
        }),
        "the pane holds no history entry"
    );
    assert_eq!(
        said(&ui, "tables/pane/history").as_deref(),
        Some("History of this row")
    );
    assert!(!view_of(&app).history.open);

    act(&mut app, "tables/pane/history", None);
    let view = view_of(&app);
    assert!(view.history.open);
    assert_eq!(view.history.scope, Scope::Row);
    let ui = render(&app);
    assert!(on(&ui, "tables/history/scope/row"));
    assert!(said(&ui, &line_key(4, 0)).is_some(), "the row's records");
    assert!(said(&ui, &line_key(2, 0)).is_none(), "not another row's");
    // Tab walks the tools, the grid, the drawer's head (its scope control one stop, at the
    // current scope), its list (one stop, its first line), then the pane.
    let stop = |key: &str| {
        ui.focus_order
            .iter()
            .position(|stop| stop.as_str() == key)
            .unwrap_or_else(|| panic!("{key} is a tab stop"))
    };
    let order = [
        stop(FIND),
        stop(&format!("tables/cell/{A}/label")),
        stop("tables/history/scope/row"),
        stop("tables/history/hide"),
        stop(&line_key(5, 1)),
        stop("tables/pane/history"),
    ];
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    for key in [
        "tables/history/scope/cell".to_owned(),
        "tables/history/scope/table".to_owned(),
        line_key(4, 0),
        format!("tables/history/asof/{}", hex(4)),
        line_key(1, 0),
    ] {
        assert!(
            !ui.focus_order.iter().any(|stop| stop.as_str() == key),
            "{key} is no tab stop of its own"
        );
    }
}

/// A table of `n` records, each one cell of row A, the oldest alone in writing its location.
fn long_history(n: u8) -> Snapshot {
    let mut written = vec![record(1, "m9", T0, vec![set(A, "location", "rack a")])];
    written.extend((2..=n).map(|at| {
        record(
            at,
            "m9",
            T0 + u64::from(at) * 1000,
            vec![set(A, "nickname", &format!("name {at}"))],
        )
    }));
    sheet_of(
        vec![row(
            A,
            &[
                ("label", "m9"),
                ("nickname", "name"),
                ("location", "rack a"),
            ],
        )],
        written,
    )
}

/// A wheel over the drawer scrolls its list and leaves the grid's rows; another scope or table
/// starts the list at its top. A line far from what the list shows keeps its place and its
/// press and paints nothing; a cell selected in the grid brings its newest line into view.
#[test]
fn a_wheel_scrolls_the_drawers_list_and_a_selected_cells_line_comes_into_view() {
    let mut snapshot = long_history(120);
    let mut other = snapshot.sheets[0].clone();
    other.name = "account";
    snapshot.sheets.push(other);
    let mut listed = snapshot.listed[0];
    listed.name = "account";
    snapshot.listed.push(listed);
    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    let ui = render(&app);
    // The oldest record's line is far below: listed, off the list's screen, not painted.
    let oldest = line_key(1, 0);
    assert!(ui.semantic(&UiKey::new(oldest.clone())).unwrap().offscreen);
    let drawn = tree(&app);
    assert!(!has_node(&drawn.root, &format!("{oldest}/face")));
    assert!(has_node(&drawn.root, &format!("{}/face", line_key(120, 0))));
    // A list longer than its room wears the grid's bar down its right edge.
    assert_eq!(
        group_style_of(&drawn, "tables/history/thumb"),
        Some(StyleRef::SheetBar)
    );

    assert_eq!(
        fixed_height(&drawn.root, "tables/history/track/lead"),
        Some(0.0),
        "the bar stands at the top of its track"
    );

    wheel(&mut app, 3, &line_key(120, 0));
    let view = view_of(&app);
    assert_eq!(view.history.scroll.get(), 3.0 * line_height());
    assert_eq!(view.scroll.get(), 0.0, "the grid's rows stay");
    let moved = render(&app);
    assert_eq!(
        rect(&moved, &line_key(119, 0)).y,
        rect(&ui, &line_key(119, 0)).y - 3.0 * line_height()
    );
    // The bar moves down its track with the list.
    let (room, limit) = (view.history.room.get(), view.history.limit.get());
    let thumb = fixed_height(&tree(&app).root, "tables/history/thumb").unwrap();
    let lead = fixed_height(&tree(&app).root, "tables/history/track/lead").unwrap();
    assert!(lead > 0.0, "the bar moved: {lead}");
    assert!(
        near(lead, (room - thumb) * 3.0 * line_height() / limit),
        "{lead}"
    );
    let painted = place(&moved, "tables/history/thumb");
    let list = rect(&moved, "tables/history/list");
    assert!(
        near(painted.y, list.y + lead),
        "the bar is painted down its track: {painted:?} in {list:?}"
    );
    // Another scope's list starts at its top; so does another table's.
    act(&mut app, "tables/history/scope/row", None);
    assert_eq!(view_of(&app).history.scroll.get(), 0.0);
    // A list its room holds wears no bar: Row scope with no row selected is one line.
    assert!(!has_node(&tree(&app).root, "tables/history/thumb"));
    act(&mut app, "tables/history/scope/table", None);
    let _ = render(&app);
    wheel(&mut app, 5, "tables/history");
    assert!(view_of(&app).history.scroll.get() > 0.0);
    act(&mut app, "tables/sheet/account", None);
    let _ = render(&app);
    assert_eq!(view_of(&app).history.scroll.get(), 0.0);
    act(&mut app, "tables/sheet/machine", None);
    let _ = render(&app);

    // The grid's cell selected: its newest line, the list's last but one, comes into view.
    act(&mut app, &format!("tables/cell/{A}/location"), None);
    let ui = render(&app);
    let line = ui.semantic(&UiKey::new(oldest.clone())).unwrap();
    assert!(!line.offscreen);
    assert_eq!(
        group_style_of(&tree(&app), &format!("{oldest}/focus")),
        Some(StyleRef::CellSelected),
        "it wears the selected cell's ring"
    );
    assert_no_selected_paint(&ui);
    assert!(
        line.rect.y >= list.y - 0.01 && line.rect.bottom() <= list.bottom() + 0.01,
        "{:?} in {list:?}",
        line.rect
    );
    assert!(has_node(&tree(&app).root, &format!("{oldest}/face")));
    // The bar stands where the list does: near the foot of its track.
    let followed = view_of(&app).history.scroll.get();
    let (room, limit) = (
        view_of(&app).history.room.get(),
        view_of(&app).history.limit.get(),
    );
    let thumb = fixed_height(&tree(&app).root, "tables/history/thumb").unwrap();
    let lead = fixed_height(&tree(&app).root, "tables/history/track/lead").unwrap();
    assert!(near(lead, (room - thumb) * followed / limit), "{lead}");
    assert!(lead > (room - thumb) * 0.9, "{lead} of {}", room - thumb);
    let painted = place(&ui, "tables/history/thumb");
    let list = rect(&ui, "tables/history/list");
    assert!(
        near(painted.y, list.y + lead),
        "the bar is painted down its track: {painted:?} in {list:?}"
    );
    // A wheel away from it is not undone while the selection stays.
    wheel(&mut app, -4, "tables/history/list");
    let _ = render(&app);
    assert_eq!(
        view_of(&app).history.scroll.get(),
        followed - 4.0 * line_height()
    );
    // Focus reaching a line off the list's screen brings it into view.
    let before = view_of(&app).history.scroll.get();
    with_view_mut(&mut app, |view| {
        view.reveal_listed_control(&UiKey::new(line_key(120, 0)));
    });
    assert_eq!(view_of(&app).history.scroll.get(), 1.0 * line_height());
    assert!(before > line_height());
}

/// A table of each Mac's own rows, read only, has the drawer too: it lists what the read holds
/// of the table's records and offers As of by the same rule; a line still selects its cell, and
/// nothing in it stages or offers Undo.
#[test]
fn a_read_only_table_of_macs_own_rows_lists_its_records() {
    let mut snapshot = history();
    snapshot.sheets[0].owned = true;
    snapshot.listed[0].owned = true;
    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    let ui = render(&app);
    assert!(said(&ui, &line_key(4, 0)).is_some(), "the table's records");
    assert!(state_of(&ui, &format!("tables/history/asof/{}", hex(4))).enabled);
    assert!(
        ui.semantics
            .iter()
            .all(|node| !node.key.as_str().starts_with("tables/history/undo/")),
        "nothing staged, no Undo"
    );
    act(&mut app, &line_key(4, 0), None);
    assert_eq!(selection(&app), (Some(A.into()), Some("nickname".into())));
}

/// A staged cell the table's rules refuse says so on its line, in the grid's word and ink, and
/// its Undo still takes it back.
#[test]
fn a_refused_staged_cells_line_says_refused_and_its_undo_works() {
    let mut app = open_with(history());
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", B, "location", "rack z".into());
    let at = ("machine".to_owned(), B.to_owned(), "location".to_owned());
    tables
        .refused
        .insert(at.clone(), "a location names a rack".into());
    act(&mut app, "tables/history/show", None);
    let staged = format!("tables/history/staged/{B}/location");
    assert_eq!(
        said(&render(&app), &staged).as_deref(),
        Some("m55 \u{b7} Location: rack z, not written, refused: a location names a rack")
    );
    assert_eq!(
        style_of(&tree(&app), &format!("{staged}/refused")),
        Some(StyleRef::Danger)
    );
    act(&mut app, &format!("tables/history/undo/{B}/location"), None);
    let tables = app.native_runtime.tables_mut().unwrap();
    assert!(tables.staged.is_empty());
    assert!(tables.refusal(&at).is_none());
}

/// Where the keys stand: the key of the control they are on.
fn focus(app: &App) -> Option<String> {
    view_of(app)
        .common
        .last_focus
        .as_ref()
        .map(|key| key.as_str().to_owned())
}

/// Press `key` as a pointer does: the keys on it without the keyboard's ring, then the press.
fn point(app: &mut App, key: &str) {
    with_view_mut(app, |view| {
        view.common.last_focus = Some(UiKey::new(key.to_owned()));
        view.common.focus_visible = false;
    });
    act(app, key, None);
}

/// Press `key` from the keyboard: Tab put the keys on it, and its ring shows.
fn key_press(app: &mut App, key: &str) {
    with_view_mut(app, |view| {
        view.common.last_focus = Some(UiKey::new(key.to_owned()));
        view.common.focus_visible = true;
    });
    act(app, key, None);
}

/// An Undo goes with its line, so the keys that pressed it never stay on it, where Return would
/// press the page's default button (Write): after a pointer's press they are the selected
/// cell's, as after the pane's Undo, and from the keyboard they rest on Hide history.
/// NEGATIVE CONTROL: with the drawer's Undo moving the keys only when the keyboard pressed it,
/// the pointer's press leaves them on the Undo gone with its line:
/// `` left: Some("tables/history/undo/mc-000000000000000b/location") ``.
#[test]
fn an_undo_in_the_drawer_leaves_the_keys_on_a_control_still_drawn() {
    let mut app = open_with(history());
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", B, "location", "rack z".into());
    tables.stage("machine", A, "nickname", "Spare 3".into());
    act(&mut app, "tables/history/show", None);
    act(&mut app, &format!("tables/cell/{C}/label"), None);

    let undo = format!("tables/history/undo/{B}/location");
    point(&mut app, &undo);
    assert_eq!(focus(&app), Some(format!("tables/cell/{C}/label")));
    let ui = render(&app);
    assert!(
        ui.semantic(&UiKey::new(undo)).is_none(),
        "the Undo went with its line"
    );
    assert!(
        ui.semantic(&UiKey::new(focus(&app).unwrap())).is_some(),
        "the keys are on a control drawn"
    );
    assert_eq!(
        app.native_runtime
            .tables_mut()
            .unwrap()
            .staged
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [("machine".to_owned(), A.to_owned(), "nickname".to_owned())],
        "the other change waits, unwritten"
    );

    key_press(&mut app, &format!("tables/history/undo/{A}/nickname"));
    assert_eq!(focus(&app).as_deref(), Some("tables/history/hide"));
}

/// A pointer's press in the drawer that selects nothing (a line of a row Find hides, a fold's
/// head) gives the keys back to the selected cell, where typing, Delete and Return go, as a
/// press on a header does; with a cell open, to the open cell. From the keyboard the control
/// pressed keeps them.
#[test]
fn a_pointer_press_in_the_drawer_gives_the_keys_back_to_the_selection() {
    let mut app = open_with(history());
    act(&mut app, "tables/history/show", None);
    act(&mut app, &format!("tables/cell/{B}/location"), None);
    act(&mut app, FIND, Some(SemanticInput::Text("m55".into())));
    let cell = format!("tables/cell/{B}/location");

    point(&mut app, &line_key(3, 0));
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("m9 is not in the rows shown")
    );
    assert_eq!(focus(&app), Some(cell.clone()));

    let fold = format!("tables/history/fold/{}", hex(6));
    point(&mut app, &fold);
    assert_eq!(state_of(&render(&app), &fold).expanded, Some(true));
    assert_eq!(focus(&app), Some(cell.clone()));
    key_press(&mut app, &fold);
    assert_eq!(state_of(&render(&app), &fold).expanded, Some(false));
    assert_eq!(focus(&app), Some(fold));

    // The selected cell opened: a pointer's press that changes nothing returns the keys to it.
    act(&mut app, &cell, None);
    assert!(view_of(&app).editing.is_some(), "a second press opens it");
    point(&mut app, &line_key(3, 0));
    assert!(view_of(&app).editing.is_some(), "the cell stays open");
    assert_eq!(focus(&app).as_deref(), Some(EDIT));
}

/// A read that brings records puts their lines above the ones shown, and the line that wears
/// the selected cell's ring stays where it stood in the list's room; a read whose record set
/// the selected cell moves the ring to that record's line and brings it into view.
/// NEGATIVE CONTROL: with `follow` leaving the scroll as it was on a new read, the ringed line
/// is pushed under the list's foot: `` the ringed line stays in view where it stood ``.
#[test]
fn a_read_that_brings_records_keeps_the_ringed_line_in_view() {
    let mut app = open_with(long_history(120));
    act(&mut app, "tables/history/show", None);
    act(&mut app, &format!("tables/cell/{A}/location"), None);
    let ui = render(&app);
    let ringed = line_key(1, 0);
    let list = rect(&ui, "tables/history/list");
    let was = rect(&ui, &ringed);
    assert!(
        was.y >= list.y - 0.01 && was.bottom() <= list.bottom() + 0.01,
        "{was:?} in {list:?}"
    );
    assert!(
        was.bottom() > list.bottom() - 3.0 * line_height(),
        "the ring's line stands near the list's foot: {was:?} in {list:?}"
    );

    // Another machine's three records, read: three lines above.
    let mut read = long_history(120);
    read.sheets[0].written.extend((121..=123).map(|n| {
        record(
            n,
            "m73",
            T0 + u64::from(n) * 1000,
            vec![set(A, "nickname", &format!("name {n}"))],
        )
    }));
    app.native_runtime.tables_mut().unwrap().show(read);
    let ui = render(&app);
    let now = rect(&ui, &ringed);
    assert!(
        near(now.y, was.y),
        "the ringed line stays in view where it stood: {now:?}, was {was:?}"
    );
    assert_eq!(
        group_style_of(&tree(&app), &format!("{ringed}/focus")),
        Some(StyleRef::CellSelected)
    );

    // A record that set the selected cell: the ring moves to its line, which comes into view.
    let mut read = long_history(120);
    read.sheets[0].written.extend((121..=124).map(|n| {
        let column = if n == 124 { "location" } else { "nickname" };
        record(
            n,
            "m73",
            T0 + u64::from(n) * 1000,
            vec![set(A, column, &format!("place {n}"))],
        )
    }));
    app.native_runtime.tables_mut().unwrap().show(read);
    let ui = render(&app);
    let list = rect(&ui, "tables/history/list");
    let newest = rect(&ui, &line_key(124, 0));
    assert!(
        newest.y >= list.y - 0.01 && newest.bottom() <= list.bottom() + 0.01,
        "{newest:?} in {list:?}"
    );
    assert_eq!(
        group_style_of(&tree(&app), &format!("{}/focus", line_key(124, 0))),
        Some(StyleRef::CellSelected)
    );
}

/// A wheel over the closed drawer, which has no list, scrolls the grid's rows, and leaves the
/// list where it was.
#[test]
fn a_wheel_over_the_closed_drawer_scrolls_the_grid() {
    use super::super::native_tables_tests::{pk_of, tall_sheet};
    let mut snapshot = tall_sheet(80, 3);
    snapshot.sheets[0].written = (1..=120)
        .map(|n| {
            record(
                n,
                "m9",
                T0 + u64::from(n) * 1000,
                vec![set(&pk_of(usize::from(n) % 80), "c1", &format!("v{n}"))],
            )
        })
        .collect();
    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    let _ = render(&app);
    wheel(&mut app, 3, "tables/history/list");
    assert_eq!(view_of(&app).history.scroll.get(), 3.0 * line_height());
    act(&mut app, "tables/history/hide", None);
    let _ = render(&app);
    wheel(&mut app, 5, "tables/history/show");
    let view = view_of(&app);
    assert_eq!(view.scroll.get(), 5.0 * row_height(), "the grid's rows");
    assert_eq!(
        view.history.scroll.get(),
        3.0 * line_height(),
        "not the list"
    );
}

/// The current scope is marked as the rail marks the table shown, a neutral band under words in
/// the primary ink, and says it is on as its value; no control of the drawer is selected, a
/// state painted in the accent. The scope control is one Tab stop, at the current scope.
#[test]
fn the_current_scope_is_marked_as_the_rail_marks_the_table_shown() {
    let mut app = open_with(history());
    act(&mut app, "tables/history/show", None);
    for (current, others) in [("table", ["cell", "row"]), ("cell", ["row", "table"])] {
        act(&mut app, &format!("tables/history/scope/{current}"), None);
        let drawn = tree(&app);
        let at = |scope: &str| format!("tables/history/scope/{scope}");
        assert_eq!(
            group_style_of(&drawn, "tables/sheet/machine/band"),
            Some(StyleRef::Code),
            "the rail's mark"
        );
        assert_eq!(
            group_style_of(&drawn, &format!("{}/band", at(current))),
            Some(StyleRef::Code)
        );
        assert_eq!(
            style_of(&drawn, &format!("{}/word", at(current))),
            Some(StyleRef::Primary)
        );
        for other in others {
            assert!(!has_node(&drawn.root, &format!("{}/band", at(other))));
            assert_eq!(
                style_of(&drawn, &format!("{}/word", at(other))),
                Some(StyleRef::Plain)
            );
        }
        let ui = render(&app);
        assert!(on(&ui, &at(current)));
        assert!(others.iter().all(|other| !on(&ui, &at(other))));
        assert_no_selected_paint(&ui);
        let stops: Vec<&str> = ui
            .focus_order
            .iter()
            .map(|stop| stop.as_str())
            .filter(|stop| stop.starts_with("tables/history/scope/"))
            .collect();
        assert_eq!(stops, [at(current)], "one stop, at the current scope");
    }
}

/// In a head too narrow for all of it, Show history and Hide history stay whole inside the
/// drawer: the newest record's words or the counts give way first, then the title, then the
/// scope's words.
#[test]
fn a_narrow_drawer_keeps_its_show_and_hide_buttons_whole() {
    use super::super::native_tables_tests::render_in;
    let mut app = open_with(history());
    for open in [false, true] {
        if open {
            act(&mut app, "tables/history/show", None);
        }
        let button = if open {
            "tables/history/hide"
        } else {
            "tables/history/show"
        };
        for width in [300.0, 360.0, 480.0, VIEWPORT.width] {
            let ui = render_in(&app, width, VIEWPORT.height);
            let (drawer, pressed) = (rect(&ui, "tables/history"), rect(&ui, button));
            assert!(
                pressed.x >= drawer.x - 0.01 && pressed.right() <= drawer.right() + 0.01,
                "{width}: {button} {pressed:?} in {drawer:?}"
            );
            if open {
                for scope in ["cell", "row", "table"] {
                    let segment = rect(&ui, &format!("tables/history/scope/{scope}"));
                    assert!(
                        segment.width > 0.0 && segment.right() <= pressed.x,
                        "{width}: {scope} {segment:?} before {pressed:?}"
                    );
                }
            }
        }
    }
}

/// The list is one Tab stop however many lines it holds, so Tab leaves the drawer for the pane;
/// Up and Down move from line to line, each brought into view, and leave the grid's selection
/// where it was.
/// NEGATIVE CONTROL: with the list not marked one stop (no `segmented`), every line and As of
/// is a stop: `` one stop for the list ``.
#[test]
fn the_drawers_list_is_one_tab_stop_and_the_arrows_move_among_its_lines() {
    let mut app = open_with(long_history(120));
    act(&mut app, &format!("tables/cell/{A}/label"), None);
    act(&mut app, "tables/history/show", None);
    let in_list = |ui: &CompiledUi| -> Vec<String> {
        ui.focus_order
            .iter()
            .map(|stop| stop.as_str().to_owned())
            .filter(|stop| {
                stop.starts_with("tables/history/at/") || stop.starts_with("tables/history/asof/")
            })
            .collect()
    };
    assert_eq!(
        in_list(&render(&app)),
        [line_key(120, 0)],
        "one stop for the list"
    );

    with_view_mut(&mut app, |view| {
        view.common.last_focus = Some(UiKey::new(line_key(120, 0)));
        view.common.focus_visible = true;
    });
    let step = |app: &mut App, rows: i32| {
        app.dispatch_native_event(WID, AppEvent::StepFocus(rows))
            .expect("the page takes the arrow");
        let _ = render(app);
    };
    step(&mut app, 1);
    assert_eq!(focus(&app), Some(line_key(119, 0)));
    step(&mut app, -1);
    step(&mut app, -1);
    assert_eq!(
        focus(&app),
        Some(line_key(120, 0)),
        "the first line is the first"
    );
    for _ in 0..30 {
        step(&mut app, 1);
    }
    assert_eq!(focus(&app), Some(line_key(90, 0)));
    let ui = render(&app);
    let (line, list) = (
        rect(&ui, &line_key(90, 0)),
        rect(&ui, "tables/history/list"),
    );
    assert!(
        line.y >= list.y - 0.01 && line.bottom() <= list.bottom() + 0.01,
        "{line:?} in {list:?}"
    );
    assert_eq!(selection(&app), (Some(A.into()), Some("label".into())));
    assert_eq!(
        in_list(&ui),
        [line_key(90, 0)],
        "the stop is where the keys are"
    );
}

/// A press on a line of a row below the grid's room brings that row into the grid's view, as
/// a selection reached by the arrows does.
#[test]
fn a_line_of_a_row_below_the_grids_room_brings_its_cell_into_view() {
    use super::super::native_tables_tests::{pk_of, tall_sheet};
    let mut snapshot = tall_sheet(80, 3);
    snapshot.sheets[0].written = vec![record(1, "m9", T0, vec![set(&pk_of(79), "c1", "far")])];
    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    let _ = render(&app);
    assert_eq!(view_of(&app).scroll.get(), 0.0);
    act(&mut app, &line_key(1, 0), None);
    assert_eq!(selection(&app), (Some(pk_of(79)), Some("c1".into())));
    let ui = render(&app);
    assert!(view_of(&app).scroll.get() > 0.0, "the grid's rows moved");
    let (cell, grid) = (
        rect(&ui, &format!("tables/cell/{}/c1", pk_of(79))),
        rect(&ui, "tables/grid"),
    );
    assert!(
        cell.y >= grid.y && cell.bottom() <= grid.bottom() + 0.01,
        "{cell:?} in {grid:?}"
    );
}

/// A day head says the day its record claims by this machine's clock, as the page's group heads
/// write: weekday, day, month and year, in capitals.
#[test]
fn a_day_head_says_the_day_its_record_claims() {
    // Noon of a day by this machine's clock, wherever it is: days since 1970-01-01.
    let noon = |days: i64| {
        let unix = days * 86_400 + 12 * 3_600;
        let offset = crate::presence::local_offset_at(unix).unwrap_or(0);
        u64::try_from(unix - offset).unwrap() * 1000
    };
    // Saturday 7 November 2026, Friday 1 January 2027, Thursday 29 February 2024.
    for (days, words) in [
        (20_764, "SAT 7 NOV 2026"),
        (20_819, "FRI 1 JAN 2027"),
        (19_782, "THU 29 FEB 2024"),
    ] {
        assert_eq!(super::day_words(noon(days)), words);
    }
    let snapshot = sheet_of(
        vec![row(A, &[("label", "m9")])],
        vec![record(1, "m9", noon(20_764), vec![set(A, "nickname", "x")])],
    );
    let (_, listed) = lines(
        &snapshot.sheets[0],
        Scope::Table,
        None,
        &nothing_staged(),
        &no_folds(),
        None,
    );
    assert_eq!(listed[0], Line::Day("SAT 7 NOV 2026".into()));
}

/// The drawer never runs the page past the window's foot: wherever it is drawn the page ends
/// inside the window, whether a change waits to be written and whether the selected cell's line
/// wraps, at every height a wide page and a narrow one with no row selected are given. In a
/// window too short for its line, the cell bar and the grid under the bar the bar gives way
/// first; where the bar stands the grid under it keeps three lines while the line under the
/// grid takes three of its lines or fewer, and with its bar three lines where it wraps further.
/// A narrow page judges the drawer on the whole room, as a wide one does (only the bar is judged
/// on half of it there), so the drawer stands in most windows at both widths.
/// NEGATIVE CONTROL: with `page` drawing the drawer whatever the window (`let drawn =
/// sheet.is_some();`), the drawer of a short window runs past its foot: `` 1400 x 268: the page
/// ends inside the window: tables/history LogicalRect { x: 204.0, y: 262.0, width: 828.0,
/// height: 6.0 } ``. With `page` judging the drawer on half the room on a narrow page, as it
/// judges the bar (`(if narrow { steady * 0.5 } else { steady }) - drawer_least >= line * 3.0`),
/// a narrow page with no row selected loses the drawer in windows that hold it and three lines
/// of grid: `` 760: the drawer stands in most windows: 100 ``. With `page` judging the bar
/// without the drawer's line (`(if narrow { steady * 0.5 } else { steady }) >= cell_line +
/// under_bar`), the bar stands where the drawer's line leaves it and the grid under it less than
/// three lines: `` 1400 x 376: the grid and its bar keep three lines: LogicalRect { x: 204.0, y:
/// 134.0, width: 828.0, height: 44.0 } ``. Open, the list's room is whole lines, and the drawer
/// is its head alone only where a line would take the work under its least: with `heights`
/// giving the open drawer what it would have, unrounded (its body before that rule), the grid
/// kept rows the drawer's line could have had: `` 1400 x 376: the drawer is its head alone over
/// a work of 132, least 90 ``.
#[test]
fn the_drawer_never_runs_the_page_past_the_window() {
    page_fit(0);
}

/// [`the_drawer_never_runs_the_page_past_the_window`] under one line of the filters' chips and
/// under the three they wrap onto at most: their line comes off the grid's room once, as the
/// tools row's does, and counts in the window's judgment of the drawer and the cell bar, so the
/// page still ends inside the window, the bar still keeps the grid's lines under it, and the
/// open list's room is still whole lines; the drawer stands in most of the windows that hold the
/// chips too (those the chips' line and its gap leave the room of a window without them).
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `page` judging the drawer and
/// the bar as if no chip stood (`steady` given back the chips' line and its gap), under one line
/// of chips the write bar of a short window runs past its foot: `` 1400 x 376: the page ends
/// inside the window: tables/bar LogicalRect { x: 204.0, y: 340.0, width: 828.0, height: 36.0 }
/// ``.
#[test]
fn the_drawer_never_runs_the_page_past_the_window_under_the_filters() {
    page_fit(1);
    page_fit(super::super::native_tables_tests::CHIP_COLUMNS);
}

/// The sweep of [`the_drawer_never_runs_the_page_past_the_window`], with `chips` filters
/// standing ([`super::super::native_tables_tests::stand_chips`]) where `chips` is not 0.
fn page_fit(chips: usize) {
    use super::super::native_tables_tests::{
        chip_sheet, pk_of, render_in, stand_chips, tall_sheet,
    };
    let mut snapshot = if chips == 0 {
        tall_sheet(80, 3)
    } else {
        chip_sheet(80)
    };
    let long = |who: &str| {
        (
            format!("the value {who} wrote, said whole by the line under the grid, {who}"),
            Some(author(who, 1_000)),
        )
    };
    let cell = snapshot.sheets[0].rows[2].cells.get_mut("c1").unwrap();
    cell.values = vec![long("m55"), long("m73"), long("m22")];
    snapshot.sheets[0].written = vec![record(1, "m9", T0, vec![set(&pk_of(2), "c1", "v")])];
    let mut app = open_with(snapshot);
    if chips > 0 {
        stand_chips(&mut app, chips);
    }
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(2)), None);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", &pk_of(1), "c1", "typed".into());
    // The windows of the sweep, every four points, that the chips' line and its gap take from
    // the room of one without them: the drawer comes that much later.
    let chips_h = if chips == 0 {
        0.0
    } else {
        place(&render_in(&app, 1400.0, 900.0), "tables/chips").height + 10.0
    };
    let lost = (chips_h / 4.0).ceil() as usize;
    for width in [1400.0, 760.0] {
        if width < 900.0 {
            // A narrow page with a row selected ends with the row's pane, which keeps its least.
            with_view_mut(&mut app, |view| view.row = None);
        }
        let mut drawn = 0;
        for height in (200..=900).step_by(4) {
            let height = height as f32;
            let ui = render_in(&app, width, height);
            if ui
                .semantic(&UiKey::new("tables/history/show".to_owned()))
                .is_none()
            {
                continue;
            }
            drawn += 1;
            let foot = height - 16.0;
            for key in ["tables/history", "tables/bar", WRITE] {
                if let Some(node) = ui.semantic(&UiKey::new(key.to_owned())) {
                    assert!(
                        node.rect.bottom() <= foot + 0.5,
                        "{width} x {height}: the page ends inside the window: {key} {:?}",
                        node.rect
                    );
                }
            }
            if ui
                .semantic(&UiKey::new("tables/cellbar".to_owned()))
                .is_some()
            {
                // Under the bar the grid keeps three lines while the line under it takes three
                // of its lines or fewer; one wrapped further takes its room from the grid, down
                // to the three lines the grid and its bar keep between them.
                let (bar, grid) = (rect(&ui, "tables/cellbar"), rect(&ui, "tables/grid"));
                let three = 3.0 * 30.0 * scale().max(1.0);
                assert!(
                    bar.height + grid.height >= three - 0.01,
                    "{width} x {height}: the grid and its bar keep three lines: {grid:?}"
                );
                if rect(&ui, "tables/said").height <= 3.0 * said_line() + 0.01 {
                    assert!(
                        grid.height >= three - 0.01,
                        "{width} x {height}: the bar stands over three lines of grid: {grid:?}"
                    );
                }
            }
        }
        assert!(
            drawn > 100 - lost,
            "{width}: the drawer stands in most windows: {drawn}"
        );
    }

    // Open, the list's room is whole lines, so none shows in part, and the page still ends
    // inside the window; on a narrow page with a row selected and its pane under the grid (the
    // window that page ends with the pane's least is not this test's), a change staged, the
    // list has a line wherever the work keeps a line and the drawer's foot over its least.
    act(&mut app, "tables/history/show", None);
    act(&mut app, "tables/history/scope/row", None);
    let line = line_height();
    for (width, row) in [(1400.0, false), (760.0, false), (760.0, true)] {
        if row {
            act(&mut app, &format!("tables/cell/{}/c1", pk_of(2)), None);
        }
        let mut lined = 0;
        for height in (200..=900).step_by(4) {
            let height = height as f32;
            let ui = render_in(&app, width, height);
            if ui
                .semantic(&UiKey::new("tables/history/hide".to_owned()))
                .is_none()
            {
                continue;
            }
            let room = view_of(&app).history.room.get();
            let lines = (room / line).round();
            assert!(
                near(room, lines * line),
                "{width} x {height}: the list's room is whole lines: {room}"
            );
            if !row {
                for key in ["tables/history", WRITE] {
                    let node = rect(&ui, key);
                    assert!(
                        node.bottom() <= height - 16.0 + 0.5,
                        "{width} x {height}: the page ends inside the window: {key} {node:?}"
                    );
                }
            }
            if lines >= 1.0 {
                lined += 1;
                continue;
            }
            // The head alone: the work has no line and the foot to spare over its least (under
            // the bar the grid's own lines and a row, else three lines of grid).
            let work = rect(&ui, "tables/work").height;
            let least = if ui
                .semantic(&UiKey::new("tables/cellbar".to_owned()))
                .is_some()
            {
                row_height() + grid_chrome() + row_height()
            } else {
                3.0 * 30.0 * scale().max(1.0)
            };
            assert!(
                work - least < line + FOOT,
                "{width} x {height}: the drawer is its head alone over a work of {work}, \
                 least {least}"
            );
        }
        // With a row selected a narrow page gives the grid and the drawer half its room, so
        // the list has a line in the taller third of these windows; in the rest the drawer is
        // its head alone, where its line would cost the work its least (asserted above).
        assert!(
            lined > if row { 50 } else { 100 } - lost,
            "{width}, a row selected {row}: the open list has a line in these windows: {lined}"
        );
    }
}

/// The values Row scope on row `pk` lists of the records read, each with its column and whether
/// it is the row's birth.
fn row_values(app: &mut App, pk: &str) -> Vec<(String, String, bool)> {
    let tables = app.native_runtime.tables_mut().unwrap();
    let sheet = tables.sheet("machine").unwrap();
    let (_, listed) = lines(
        sheet,
        Scope::Row,
        Some((pk, None)),
        &nothing_staged(),
        &no_folds(),
        None,
    );
    listed
        .into_iter()
        .filter_map(|line| match line {
            Line::One { cell, .. } | Line::Cell { cell, .. } => {
                Some((cell.column, cell.value, cell.born))
            }
            _ => None,
        })
        .collect()
}

/// Rows a person adds, retires, ends and restores (LT-24) are the drawer's as any staged cell
/// is, on a registry whose grid shows the lifecycle's column. A new row leads the lines not
/// written with its birth, named as its pane names it, though no cell of it is staged yet, over
/// the cells staged in it; that line's Undo takes the row back and every cell staged in it, as
/// the pane's Remove does. A retire is the row's lifecycle cell staged, whose Undo takes it back
/// as the pane's Undo under the fields does. Written, the row's birth reads `Row → added` once
/// (never `live` as a value) and its retire as the lifecycle's value. A refusal the page's check
/// puts on a new row's lifecycle (a key the table holds already) is said on its birth line. A
/// new row a find hides is named in the head, its birth line pressed, as its lines name it.
/// NEGATIVE CONTROLS, each run once and restored with cp: with the drawer reading a birth only
/// where the grid does not show the lifecycle's column (`column.is_none() && …`, as before LT-24
/// showed it), `` the birth reads once: [("label", "m12", false), ("status", "active", false),
/// ("row", "live", false)] ``; with a new row's lines named as any row's (`false &&` on
/// `rows::is_new` in `lines`, run before that naming moved to `row_words`), `` left:
/// Some("+new-1 · Label: m12, not written") right: Some("New machine · Label: m12, not written")
/// ``; with `history_select` naming the row it does not show by its key text alone
/// (`names(sheet).get(pk).copied().unwrap_or(pk)`, as before), `` left: Some("+new-1 is not in
/// the rows shown") right: Some("New machine is not in the rows shown") ``; with the birth
/// line's Undo unstaging as any line's does (`false && born(…)` in its handler), `` the row is
/// taken back ``; with the refused map leaving out new rows' lifecycle cells, `` left: Some("New
/// machine: Row → added, not written") right: Some("… not written, refused: this new row is the
/// row mc-…, which the file already holds") ``.
#[test]
fn rows_added_and_retired_are_the_drawers_lines_staged_and_written() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, "tables/history/show", None);
    act(&mut app, rows::ADD, None);

    // The new row, nothing of it staged: its birth is the first line not written.
    let born = "tables/history/staged/+new-1/row";
    let undo_born = "tables/history/undo/+new-1/row";
    let ui = render(&app);
    assert_eq!(
        said(&ui, born).as_deref(),
        Some("New machine: Row \u{2192} added, not written")
    );
    assert_eq!(
        said(&ui, undo_born).as_deref(),
        Some("Undo: take back this new machine and every cell staged in it")
    );
    assert!(state_of(&ui, undo_born).enabled);
    // A find the new row does not match hides it (nothing of it is staged): its birth line,
    // pressed, names it in the head as the line does, never by its page key.
    assert!(said(&ui, "tables/cell/+new-1/label").is_some());
    act(&mut app, FIND, Some(SemanticInput::Text("zz".into())));
    assert!(said(&render(&app), "tables/cell/+new-1/label").is_none());
    act(&mut app, born, None);
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("New machine is not in the rows shown")
    );
    act(&mut app, FIND, Some(SemanticInput::Text(String::new())));
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", "+new-1", "label", "m12".into());
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/history/staged/+new-1/label").as_deref(),
        Some("New machine \u{b7} Label: m12, not written"),
        "a new row's cell is named as its pane names the row"
    );
    let tables = app.native_runtime.tables_mut().unwrap();
    let sheet = tables.sheet("machine").unwrap();
    let (_, listed) = lines(sheet, Scope::Table, None, &tables.staged, &no_folds(), None);
    let keys: Vec<String> = listed.iter().take(3).filter_map(key_of).collect();
    assert_eq!(
        keys,
        [born, "tables/history/staged/+new-1/label"],
        "the birth, then its cells"
    );
    // Its Undo is the pane's Remove: the row goes, and the cell staged in it.
    act(&mut app, undo_born, None);
    let ui = render(&app);
    let tables = app.native_runtime.tables_mut().unwrap();
    assert!(tables.born.is_empty(), "the row is taken back");
    assert!(tables.staged.is_empty(), "and every cell staged in it");
    assert!(said(&ui, "tables/cell/+new-1/label").is_none());
    assert!(said(&ui, "tables/history/unwritten").is_none());

    // Written, the row's birth reads once, and its lifecycle's first value is that birth.
    act(&mut app, rows::ADD, None);
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", "+new-2", "label", "m12".into());
    tables.stage("machine", "+new-2", "status", "active".into());
    render(&app);
    act(&mut app, WRITE, None);
    app.native_runtime.tables_mut().unwrap().settle_write();
    let pk = aterm_db::typed::keys::machine_pk("m12");
    let values = row_values(&mut app, &pk);
    assert!(
        values
            .iter()
            .filter(|(_, value, born)| *born && value == "Row \u{2192} added")
            .count()
            == 1
            && !values
                .iter()
                .any(|(column, _, born)| column == "row" && !born),
        "the birth reads once: {values:?}"
    );
    // A new row's lifecycle cell refused (where the check finds the table holds its key
    // already): its birth line says the refusal, as the cell does. The refusal is put where the
    // page's check puts it, between two renders that leave the check as it was.
    act(&mut app, rows::ADD, None);
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", "+new-3", "label", "m13".into());
    render(&app);
    let tables = app.native_runtime.tables_mut().unwrap();
    let rule = "this new row is the row mc-…, which the file already holds";
    tables.checked.insert(
        ("machine".into(), "+new-3".into(), "row".into()),
        rule.into(),
    );
    let ui = render(&app);
    assert_eq!(
        cell_says(&ui, "+new-3", "row"),
        format!("live, new, not written, refused: {rule}"),
        "the grid's cell says it"
    );
    assert_eq!(
        said(&ui, "tables/history/staged/+new-3/row"),
        Some(format!(
            "New machine: Row \u{2192} added, not written, refused: {rule}"
        ))
    );
    assert!(has_node(
        &tree(&app).root,
        "tables/history/staged/+new-3/row/refused"
    ));
    act(&mut app, "tables/history/undo/+new-3/row", None);
    assert!(app.native_runtime.tables_mut().unwrap().born.is_empty());

    // A retire, staged from the row's pane, is its lifecycle's line not written; the drawer's
    // Undo takes it back as the pane's own does.
    act(&mut app, &format!("tables/cell/{pk}/label"), None);
    act(&mut app, "tables/history/scope/row", None);
    act(&mut app, "tables/pane/row/retire", None);
    let staged = format!("tables/history/staged/{pk}/row");
    let ui = render(&app);
    let words = said(&ui, &staged).expect("the retire's line");
    assert_eq!(
        words,
        format!("m12 \u{b7} {}: retired, not written", place_of(&mut app))
    );
    act(&mut app, &format!("tables/history/undo/{pk}/row"), None);
    let ui = render(&app);
    assert!(app.native_runtime.tables_mut().unwrap().staged.is_empty());
    assert!(said(&ui, &staged).is_none());
    assert_ne!(
        said(&ui, &format!("tables/num/{pk}/n")).as_deref(),
        Some("retire")
    );

    // Written, the retire is the lifecycle's value on the row's history.
    act(&mut app, "tables/pane/row/retire", None);
    act(&mut app, WRITE, None);
    app.native_runtime.tables_mut().unwrap().settle_write();
    let values = row_values(&mut app, &pk);
    assert!(
        values.contains(&("row".into(), "retired".into(), false)),
        "{values:?}"
    );
    assert_eq!(
        values.iter().filter(|(.., born)| *born).count(),
        1,
        "a retire is no birth"
    );
}

/// The lifecycle column's words, as a line names its place.
fn place_of(app: &mut App) -> String {
    let tables = app.native_runtime.tables_mut().unwrap();
    let sheet = tables.sheet("machine").unwrap();
    let column = sheet
        .columns
        .iter()
        .find(|c| c.name == aterm_db::typed::LIFECYCLE)
        .expect("the grid shows the lifecycle");
    field_words(column)
}

/// Press and release the pointer's left button where it is.
fn click(app: &mut App) {
    app.on_mouse_input(WID, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(WID, ElementState::Released, MouseButton::Left);
}

/// Hide column `name` from the Columns list, and close the list.
fn hide(app: &mut App, name: &str) {
    act(app, "tables/columns", None);
    act(app, &format!("tables/columns/column/{name}"), None);
    act(app, "tables/columns", None);
}

/// `snapshot`'s table (of [`sheet_of`]) with `n` stored columns every row fills after its label,
/// so the location, the hosts and the password stand past the grid's room across, and the
/// Columns list runs long.
fn wide(mut snapshot: Snapshot, n: usize) -> Snapshot {
    let sheet = &mut snapshot.sheets[0];
    let plain = sheet.columns[1].clone();
    for at in (1..=n).rev() {
        let mut column = plain.clone();
        column.name = format!("f{at}");
        column.label = crate::tables_store::label_of(&column.name);
        sheet.columns.insert(1, column);
        for row in &mut sheet.rows {
            row.cells
                .insert(format!("f{at}"), shown("a value to widen the column by"));
        }
    }
    snapshot
}

/// Whether the grid draws `column`'s cell of row `pk` whole in its room across.
fn shows_whole(app: &App, pk: &str, column: &str) {
    let ui = render(app);
    assert!(
        said(&ui, &format!("tables/head/{column}")).is_some(),
        "{column} is drawn"
    );
    let cell = place(&ui, &format!("tables/cell/{pk}/{column}/seat"));
    let grid = place(&ui, "tables/grid");
    assert!(
        cell.x >= grid.x - 0.5 && cell.right() <= grid.right() + 0.5,
        "{column}'s cell shows whole: {cell:?} in {grid:?}"
    );
}

/// A press on a drawer line names a cell, and seeing that cell is what it asks: where the
/// Columns list hid the cell's column the column is shown again (the list's line says so), and
/// one the fold takes is drawn; either way the cell is selected and the grid scrolls across to
/// it, at Table scope and at Row scope, after the pointer's press (the keys on the cell) and the
/// keyboard's (the keys on the line). No press leaves the selection or the keys on a cell the
/// grid does not draw.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `history_select` not showing
/// the column (its `view.show_column(…)` call removed), the press selects the next column drawn:
/// `` left: (Some("mc-000000000000000b"), Some("password")) right: (Some("mc-000000000000000b"),
/// Some("location")) ``.
#[test]
fn a_line_of_a_hidden_or_folded_columns_cell_shows_the_column_and_selects_the_cell() {
    let mut app = open_with(wide(history(), 12));
    act(&mut app, "tables/history/show", None);
    hide(&mut app, "location");
    let ui = render(&app);
    assert!(
        said(&ui, "tables/head/location").is_none(),
        "the location is hidden"
    );
    assert!(
        said(&ui, "tables/head/hosts").is_none(),
        "no row fills the hosts: they fold"
    );

    // The pointer's press at Table scope on B's location, which record 2 wrote.
    point(&mut app, &line_key(2, 1));
    assert_eq!(selection(&app), (Some(B.into()), Some("location".into())));
    assert_eq!(focus(&app), Some(format!("tables/cell/{B}/location")));
    shows_whole(&app, B, "location");
    assert!(view_of(&app).across > 0.0, "scrolled across to it");
    // Shown again, not only while selected: another cell selected, it is still drawn.
    act(&mut app, &format!("tables/cell/{A}/label"), None);
    let ui = render(&app);
    assert!(
        said(&ui, "tables/head/location").is_some(),
        "the location is shown again"
    );
    assert!(
        said(&ui, "tables/head/hosts").is_none(),
        "the hosts still fold"
    );

    // Hidden again (the selection moves off it), then the keyboard's press on record 5's
    // clearing of it: the keys stay on the line.
    hide(&mut app, "location");
    assert_ne!(selection(&app).1.as_deref(), Some("location"));
    with_view_mut(&mut app, |view| view.across = 0.0);
    key_press(&mut app, &line_key(5, 0));
    assert_eq!(selection(&app), (Some(B.into()), Some("location".into())));
    assert_eq!(focus(&app), Some(line_key(5, 0)));
    shows_whole(&app, B, "location");

    // Row scope on C, a folded column's cell: record 6's first host, under its record opened.
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    act(&mut app, "tables/history/scope/row", None);
    act(&mut app, &format!("tables/history/fold/{}", hex(6)), None);
    point(&mut app, &line_key(6, 3));
    assert_eq!(selection(&app), (Some(C.into()), Some("hosts".into())));
    assert_eq!(focus(&app), Some(format!("tables/cell/{C}/hosts")));
    shows_whole(&app, C, "hosts");
}

/// A press on a drawer line shows its cell's column as the column's line in the Columns list
/// does: a column no row fills is pinned, so it stays drawn once the selection leaves it and the
/// arrows reach it again, whether the fold took it or a person hid it from the list (the list's
/// second press on an empty column hides it and unpins it).
/// NEGATIVE CONTROLS (each run once, restored with cp and touch): with `show_column` never
/// pinning (`folds` made `false`), the folded hosts fold again once the selection leaves them:
/// `` the hosts stay drawn after the selection leaves them ``; with it pinning only a column
/// that was not hidden (`folds && !was_hidden`), the hidden hosts do: `` shown again, the hosts
/// stay drawn as the list's line would leave them ``.
#[test]
fn a_line_of_an_empty_columns_cell_keeps_the_column_drawn_after_the_selection_leaves() {
    let mut app = open_with(history());
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    act(&mut app, "tables/history/show", None);
    act(&mut app, "tables/history/scope/row", None);
    act(&mut app, &format!("tables/history/fold/{}", hex(6)), None);
    assert!(
        said(&render(&app), "tables/head/hosts").is_none(),
        "no row fills the hosts: they fold"
    );

    // Folded: record 6's first host, pressed.
    point(&mut app, &line_key(6, 3));
    assert_eq!(selection(&app), (Some(C.into()), Some("hosts".into())));
    press_key(&mut app, NamedKey::ArrowLeft);
    assert_eq!(selection(&app), (Some(C.into()), Some("location".into())));
    assert!(
        said(&render(&app), "tables/head/hosts").is_some(),
        "the hosts stay drawn after the selection leaves them"
    );
    press_key(&mut app, NamedKey::ArrowRight);
    assert_eq!(
        selection(&app),
        (Some(C.into()), Some("hosts".into())),
        "the arrows reach the hosts again"
    );

    // Hidden: the list's press on the pinned, empty hosts hides them and unpins them.
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    hide(&mut app, "hosts");
    assert!(
        said(&render(&app), "tables/head/hosts").is_none(),
        "the hosts are hidden"
    );
    point(&mut app, &line_key(6, 3));
    assert_eq!(selection(&app), (Some(C.into()), Some("hosts".into())));
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    let ui = render(&app);
    assert!(
        said(&ui, "tables/head/hosts").is_some(),
        "shown again, the hosts stay drawn as the list's line would leave them"
    );
    act(&mut app, "tables/columns", None);
    let line = said(&render(&app), "tables/columns/column/hosts").unwrap_or_default();
    assert!(line.contains("shown"), "the list's line says so: {line}");
}

/// Keys resting on a control of the drawer's list whose line goes for no press of theirs rest
/// on Hide history, a control still drawn, as after an Undo that goes with its line: never on
/// a control the page no longer draws, where Return would reach the page's default button.
/// A Write's answer ends the change that held a hidden column in, so the Columns move the
/// selection off it and the Cell scope lists another cell's lines; and a Write's answer makes a
/// staged line a written one.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `settle_history_keys` moving
/// nothing (answering `false` at once), the keys stay on the hidden location's written line:
/// `` left:
/// Some("tables/history/at/d0c91a64fa1c7de7a9965c6d3bf503394133d40ec7d31340662ae89e7d4c5b91/2")
/// right: Some("tables/history/hide") ``.
#[test]
fn keys_on_a_drawer_line_that_goes_under_them_rest_on_hide_history() {
    use super::super::native_tables_tests::MC;
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    let write = |app: &mut App| {
        act(app, WRITE, None);
        app.native_runtime.tables_mut().unwrap().settle_write();
        // The runtime takes the reader's replies when it is woken for them.
        app.native_runtime.poll_tables();
    };
    // On a line of the location's Cell scope, the location hidden and its change holding it in.
    hide(&mut app, "location");
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "location", "rack z".into());
    act(&mut app, &format!("tables/cell/{MC}/location"), None);
    act(&mut app, "tables/history/show", None);
    act(&mut app, "tables/history/scope/cell", None);
    render(&app);
    let written = view_of(&app)
        .history
        .stops
        .borrow()
        .iter()
        .find(|key| key.starts_with(LINE))
        .cloned()
        .expect("the location's written line");
    with_view_mut(&mut app, |view| {
        view.common.last_focus = Some(UiKey::new(written.clone()));
        view.common.focus_visible = true;
    });
    write(&mut app);
    assert_ne!(
        selection(&app).1.as_deref(),
        Some("location"),
        "the selection left the hidden location"
    );
    let ui = render(&app);
    assert!(said(&ui, &written).is_none(), "the location's line went");
    assert_eq!(focus(&app).as_deref(), Some("tables/history/hide"));
    assert!(ui.semantic(&UiKey::new("tables/history/hide")).is_some());

    // On a staged line at Table scope, written: the location shown again (the list's press on
    // a hidden column's line shows it) and changed again.
    hide(&mut app, "location");
    act(&mut app, "tables/history/scope/table", None);
    app.native_runtime
        .tables_mut()
        .unwrap()
        .stage("machine", MC, "location", "rack y".into());
    let staged = format!("tables/history/staged/{MC}/location");
    assert!(said(&render(&app), &staged).is_some(), "the staged line");
    with_view_mut(&mut app, |view| {
        view.common.last_focus = Some(UiKey::new(staged.clone()));
        view.common.focus_visible = true;
    });
    write(&mut app);
    let ui = render(&app);
    assert!(
        said(&ui, &staged).is_none(),
        "written, the staged line went"
    );
    assert_eq!(focus(&app).as_deref(), Some("tables/history/hide"));
}

/// A new row's birth line, not written, names its lifecycle's cell: where the Columns list hid
/// the lifecycle's column, a press on that line shows it again and selects the cell, as a
/// written line's press does.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `history_select` not showing
/// the column (its `view.show_column(…)` call removed), the press selects the next column drawn:
/// `` left: (Some("+new-1"), Some("hosts")) right: (Some("+new-1"), Some("row")) ``.
#[test]
fn a_new_rows_birth_line_shows_the_lifecycle_column_the_list_hid() {
    let lab = two_macs();
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    hide(&mut app, aterm_db::typed::LIFECYCLE);
    assert!(
        said(&render(&app), "tables/head/row").is_none(),
        "the lifecycle is hidden"
    );
    act(&mut app, "tables/history/show", None);
    act(&mut app, rows::ADD, None);
    point(&mut app, "tables/history/staged/+new-1/row");
    assert_eq!(
        selection(&app),
        (
            Some("+new-1".into()),
            Some(aterm_db::typed::LIFECYCLE.into())
        )
    );
    let ui = render(&app);
    assert!(
        said(&ui, "tables/head/row").is_some(),
        "the lifecycle is shown"
    );
    let keys = focus(&app).unwrap();
    assert_eq!(keys, "tables/cell/+new-1/row");
    assert!(
        ui.semantic(&UiKey::new(keys)).is_some(),
        "the keys rest on a cell drawn"
    );
}

/// Selecting a grid cell narrows the drawer's Cell scope to it; a selection the columns move
/// (a person hides the column under it, and it moves to the next one the grid draws) moves the
/// scope with it: the drawer lists the cell the grid selects, never the hidden one.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `TablesApp::update` not settling
/// the columns (`settled` without its `view.settle_columns(self)`), the selection stays on the
/// hidden location and the drawer with it: `` the hidden location's line goes ``.
#[test]
fn the_cell_scope_follows_a_selection_the_columns_move() {
    let mut app = open_with(history());
    act(&mut app, &format!("tables/cell/{A}/location"), None);
    act(&mut app, "tables/history/show", None);
    act(&mut app, "tables/history/scope/cell", None);
    let ui = render(&app);
    assert!(
        said(&ui, &line_key(1, 2)).is_some(),
        "A's location, as record 1 wrote it"
    );
    assert!(said(&ui, &line_key(5, 1)).is_none());
    act(&mut app, "tables/columns", None);
    act(&mut app, "tables/columns/column/location", None);
    let ui = render(&app);
    assert!(
        said(&ui, &line_key(1, 2)).is_none(),
        "the hidden location's line goes"
    );
    assert!(
        said(&ui, &line_key(5, 1)).is_some(),
        "the password's line, the cell selected"
    );
    // The hosts fold (no row fills them): the password is the next column drawn.
    assert_eq!(selection(&app), (Some(A.into()), Some("password".into())));
}

/// The Columns list opened over the open drawer stays inside the window, laid over the grid and
/// the drawer, and neither moves for it: the Columns button stands in the tools row and each
/// header's fill bar in its header, so the page's height budget is the drawer's alone. A press
/// on the list's words or frame over the drawer's lines is the list's and reaches no line under
/// it; the wheel over it scrolls the list, never the drawer's list or the grid; and Escape
/// closes the list first, leaving the drawer open and the keys on the Columns button.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with the list's panel no surface (its
/// `.surface(…)` removed), a press on its closing words reaches the drawer's line under them:
/// `` (2092.5, 1120.5) is the list's left:
/// Some("tables/history/at/7878787878787878787878787878787878787878787878787878787878787878/0")
/// right: Some("tables/columns/panel") ``.
#[test]
fn the_columns_list_over_the_open_drawer_keeps_its_presses_its_wheel_and_escape() {
    // Columns enough that the list runs taller than the window, down over the drawer.
    let mut app = open_with(wide(long_history(120), 70));
    // A window tall enough for the drawer, which the frame on glass lays out.
    app.on_resize(WID, aterm_window::dpi::PhysicalSize::new(2800, 1800));
    act(&mut app, &format!("tables/cell/{A}/label"), None);
    act(&mut app, "tables/history/show", None);
    let before = presented(&mut app);
    assert!(
        before
            .semantic(&UiKey::new("tables/history/hide"))
            .is_some(),
        "the drawer stands open in {:?}",
        before.bounds
    );
    let (grid, drawer) = (
        rect(&before, "tables/grid"),
        rect(&before, "tables/history"),
    );
    let (tools, button) = (
        place(&before, "tables/tools"),
        rect(&before, "tables/columns"),
    );
    assert!(
        button.y >= tools.y - 0.5 && button.bottom() <= tools.bottom() + 0.5,
        "the Columns button stands in the tools row: {button:?} in {tools:?}"
    );
    act(&mut app, "tables/columns", None);
    let ui = presented(&mut app);
    let panel = place(&ui, "tables/columns/panel");
    let window = ui.bounds;
    assert!(
        panel.x >= window.x
            && panel.y >= window.y
            && panel.right() <= window.right() + 0.5
            && panel.bottom() <= window.bottom() + 0.5,
        "the list stays inside the window: {panel:?} in {window:?}"
    );
    assert_eq!(
        (rect(&ui, "tables/grid"), rect(&ui, "tables/history")),
        (grid, drawer),
        "nothing moves for the list"
    );
    let was = |app: &App| {
        let view = view_of(app);
        (
            selection(app),
            view.history.open,
            view.history.scope,
            view.history.scroll.get(),
            view.scroll.get(),
        )
    };
    let then = was(&app);

    // The wheel over the list's lines where they lie over the drawer's scrolls the list down to
    // its end, its closing words at its foot; the drawer's list and the grid stay.
    let (lines, list) = (
        place(&ui, "tables/columns/lines"),
        rect(&ui, "tables/history/list"),
    );
    let (x, y) = (
        lines.x + 40.0,
        (lines.y.max(list.y) + lines.bottom().min(list.bottom())) / 2.0,
    );
    assert!(
        ui.hits.iter().any(|hit| {
            hit.rect.contains(x, y) && hit.key.as_str().starts_with("tables/history/")
        }),
        "the list's lines {lines:?} lie over the drawer's {list:?} at ({x}, {y}) in {:?}",
        ui.bounds
    );
    let first = |ui: &CompiledUi| place(ui, "tables/columns/fold").y;
    let at_end = |ui: &CompiledUi| {
        ui.paint
            .iter()
            .any(|node| node.key.as_str() == "tables/columns/note" && !node.clipped())
    };
    let top = first(&ui);
    let (px, py) = window_px(&mut app, x, y);
    app.on_cursor_moved(WID, px, py);
    let mut notches = 0;
    while !at_end(&presented(&mut app)) && notches < 100 {
        app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(0.0, -1.0));
        assert_eq!(was(&app), then, "the drawer's list and the grid stay");
        notches += 1;
    }
    let ui = presented(&mut app);
    assert!(first(&ui) < top, "the list scrolls");
    assert!(at_end(&ui), "to its end, in {notches} notches");

    // Each point of the list over a drawer line that none of the list's lines takes (its
    // closing words, its frame) is the list's.
    let panel = place(&ui, "tables/columns/panel");
    let own: Vec<LogicalRect> = ui
        .hits
        .iter()
        .filter(|hit| {
            hit.key.as_str().starts_with("tables/columns/")
                && hit.key.as_str() != "tables/columns/panel"
        })
        .map(|hit| hit.rect)
        .collect();
    let mut over = Vec::new();
    let mut y = panel.y + 0.5;
    while y < panel.bottom() {
        let mut x = panel.x + 0.5;
        while x < panel.right() {
            if !own.iter().any(|at| at.contains(x, y))
                && ui.hits.iter().any(|hit| {
                    hit.rect.contains(x, y) && hit.key.as_str().starts_with("tables/history/")
                })
            {
                over.push((x, y));
            }
            x += 4.0;
        }
        y += 2.0;
    }
    assert!(
        over.len() > 20,
        "the list's words lie over the drawer's lines: {}",
        over.len()
    );
    for (x, y) in &over {
        assert_eq!(
            ui.hit_test(*x, *y).map(|hit| hit.key.as_str()),
            Some("tables/columns/panel"),
            "({x}, {y}) is the list's"
        );
    }
    // A press there: nothing under the list hears it, and the list stays open as it was.
    let note = place(&ui, "tables/columns/note");
    let (x, y) = over
        .iter()
        .copied()
        .find(|(x, y)| note.contains(*x, *y))
        .expect("the closing words lie over the drawer's lines");
    let (px, py) = window_px(&mut app, x, y);
    app.on_cursor_moved(WID, px, py);
    click(&mut app);
    assert!(view_of(&app).columns.open, "the list stays open");
    assert_eq!(was(&app), then, "nothing under the list heard the press");
    assert_eq!(
        said(&presented(&mut app), "tables/columns").as_deref(),
        said(&ui, "tables/columns").as_deref(),
        "no column hidden or shown"
    );

    // Escape closes the list, not the drawer.
    press_key(&mut app, NamedKey::Escape);
    let view = view_of(&app);
    assert!(!view.columns.open, "Escape closes the list");
    assert!(view.history.open, "and leaves the drawer open");
    assert_eq!(focus(&app).as_deref(), Some("tables/columns"));
}

/// Tab's order with the drawer open and a column hidden and others folded: the Columns button is
/// one stop, in the tools row's order after the find and before the grid; the drawer's list is
/// one stop; no header, edge or cell of a column the grid does not draw is a stop. Open, the
/// list's lines are stops in its order, one after another, its surface none, and Tab moves
/// from one to the next with the list open.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `left_out` never leaving a
/// hidden column out (its `hidden.contains` clause made false), the location's header, edge and
/// cells are stops: `` nothing hidden or folded is a stop: ["tables/head/location",
/// "tables/edge/location", "tables/cell/mc-000000000000000a/location", … ] ``.
#[test]
fn tab_stops_at_the_columns_button_and_the_drawer_once_and_at_nothing_hidden() {
    let mut app = open_with(history());
    act(&mut app, &format!("tables/cell/{A}/nickname"), None);
    act(&mut app, "tables/history/show", None);
    hide(&mut app, "location");
    let stops = |app: &App| -> Vec<String> {
        render(app)
            .focus_order
            .iter()
            .map(|stop| stop.as_str().to_owned())
            .collect()
    };
    let order = stops(&app);
    let at = |key: &str| order.iter().position(|stop| stop == key);
    let columns = at("tables/columns").expect("the Columns button is a stop");
    assert_eq!(
        order
            .iter()
            .filter(|stop| *stop == "tables/columns")
            .count(),
        1
    );
    assert!(at(FIND).is_some_and(|find| find < columns), "{order:?}");
    let grid = order
        .iter()
        .position(|stop| stop.starts_with("tables/head/") || stop.starts_with("tables/cell/"))
        .expect("the grid has stops");
    assert!(columns < grid, "{order:?}");
    let drawer: Vec<&String> = order
        .iter()
        .filter(|stop| {
            ["at/", "fold/", "staged/", "asof/", "undo/"]
                .iter()
                .any(|part| stop.starts_with(&format!("tables/history/{part}")))
        })
        .collect();
    assert_eq!(
        drawer.len(),
        1,
        "one stop for the drawer's list: {drawer:?}"
    );
    let hidden: Vec<&String> = order
        .iter()
        .filter(|stop| {
            ["location", "hosts"].iter().any(|name| {
                stop.ends_with(&format!("/{name}")) || stop.contains(&format!("/{name}/"))
            })
        })
        .collect();
    assert!(
        hidden.is_empty(),
        "nothing hidden or folded is a stop: {hidden:?}"
    );

    act(&mut app, "tables/columns", None);
    let order = stops(&app);
    let listed: Vec<&String> = order
        .iter()
        .filter(|stop| stop.starts_with("tables/columns/"))
        .collect();
    let first = order
        .iter()
        .position(|stop| stop == "tables/columns/fold")
        .expect("the switch is a stop");
    assert_eq!(
        order[first..first + listed.len()]
            .iter()
            .collect::<Vec<_>>(),
        listed,
        "the list's lines stand one after another"
    );
    assert_eq!(
        listed.first().map(|s| s.as_str()),
        Some("tables/columns/fold")
    );
    assert_eq!(
        listed.last().map(|s| s.as_str()),
        Some("tables/columns/all")
    );
    assert_eq!(
        listed.len(),
        6,
        "the switch, four columns and Show all: {listed:?}"
    );
    assert!(!listed.iter().any(|stop| *stop == "tables/columns/panel"));
    key_with(&mut app, Key::Named(NamedKey::Tab), Modifiers::empty());
    assert_eq!(focus(&app).as_deref(), Some(listed[1].as_str()));
    assert!(
        view_of(&app).columns.open,
        "Tab within the list keeps it open"
    );
}

/// Stand a filter on `column` that keeps the rows holding `value`, through its list as a person
/// does: its mark, the value's line, its mark again.
fn only(app: &mut App, column: &str, value: &str) {
    use super::super::native_tables_filters::{MARK, value_key};
    let mark = format!("{MARK}{column}");
    act(app, &mark, None);
    act(app, &value_key(value), None);
    act(app, &mark, None);
}

/// How many filters stand on the machines.
fn standing(app: &mut App) -> usize {
    let sheet = app
        .native_runtime
        .tables_mut()
        .unwrap()
        .sheet("machine")
        .unwrap()
        .clone();
    view_of(app).filters_of(&sheet).len()
}

/// A line whose row the filters on the columns' values alone leave out, pressed, shows the row
/// by the rule Check's Show row follows (`clear_filters_for`): the filters are cleared, every
/// one, and the line's cell is selected, the keys where any line that selects leaves them (the
/// cell after a pointer's press, the line from the keyboard), never on a chip the press took
/// away. A row the filters pass keeps them, whatever else leaves it out; and where the find or
/// the lifecycle filter leaves the row out too, clearing them would show no row, so they stand,
/// the selection stays and the head names the row, as under the find alone.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `history_select` not clearing
/// them (its `clear_filters_for` call removed), the press names the row as not shown: `` the
/// filters gave way left: 1 right: 0 ``.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `clear_filters_for` clearing
/// them wherever they leave the row out (its `life_shows` and `found` clauses removed), the
/// filters go though the find still leaves the row out: `` the filters stand where the find
/// leaves the row out too left: 0 right: 1 ``.
#[test]
fn a_line_of_a_row_the_filters_leave_out_clears_them_and_selects_its_cell() {
    let mut app = open_with(history());
    act(&mut app, "tables/history/show", None);
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    let count = said(&render(&app), "tables/history/count");
    only(&mut app, "location", "desk");
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("1 of 3 rows"));
    assert!(
        said(&ui, &format!("tables/cell/{A}/nickname")).is_none(),
        "m9 is filtered out"
    );

    // A pointer's press on record 3's line, A's nickname.
    point(&mut app, &line_key(3, 0));
    assert_eq!(standing(&mut app), 0, "the filters gave way");
    let ui = render(&app);
    assert_eq!(said(&ui, "tables/count").as_deref(), Some("3 rows"));
    assert_eq!(selection(&app), (Some(A.into()), Some("nickname".into())));
    assert_eq!(focus(&app), Some(format!("tables/cell/{A}/nickname")));
    assert_eq!(
        said(&ui, "tables/history/count"),
        count,
        "the head counts, naming no row as not shown"
    );

    // From the keyboard, record 2's B location, which the filter leaves out: the keys stay on
    // the line.
    only(&mut app, "location", "desk");
    key_press(&mut app, &line_key(2, 1));
    assert_eq!(standing(&mut app), 0);
    assert_eq!(selection(&app), (Some(B.into()), Some("location".into())));
    assert_eq!(focus(&app), Some(line_key(2, 1)));

    // A row the filters pass that the find leaves out: they stay, and the head names it.
    only(&mut app, "location", "rack a");
    act(&mut app, FIND, Some(SemanticInput::Text("m55".into())));
    point(&mut app, &line_key(3, 0));
    assert_eq!(standing(&mut app), 1, "a row they pass keeps them");
    assert_eq!(selection(&app), (Some(B.into()), Some("location".into())));
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("m9 is not in the rows shown")
    );

    // A row both the filters and the find leave out: clearing the filters would show no row,
    // so they stand with the find, and the head names the row.
    act(&mut app, "tables/chips/remove/location", None);
    only(&mut app, "location", "desk");
    point(&mut app, &line_key(3, 0));
    assert_eq!(
        standing(&mut app),
        1,
        "the filters stand where the find leaves the row out too"
    );
    assert_eq!(selection(&app), (Some(B.into()), Some("location".into())));
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("m9 is not in the rows shown")
    );

    // A row both the filters and the lifecycle filter leave out (m9 retired, the live rows
    // shown): the filters stand there too.
    let mut snapshot = history();
    snapshot.sheets[0].life = Some(rows::RowLife {
        words: &["live", "retired"],
        ends: "retired",
        born_elsewhere: None,
        born_columns: Vec::new(),
        set_by: Vec::new(),
    });
    snapshot.sheets[0].rows[0]
        .cells
        .insert(aterm_db::typed::LIFECYCLE.to_owned(), shown("retired"));
    let mut app = open_with(snapshot);
    act(&mut app, "tables/history/show", None);
    act(&mut app, &format!("tables/cell/{C}/label"), None);
    only(&mut app, "location", "desk");
    point(&mut app, &line_key(3, 0));
    assert_eq!(
        standing(&mut app),
        1,
        "the filters stand where the lifecycle filter leaves the row out too"
    );
    assert_eq!(selection(&app), (Some(C.into()), Some("label".into())));
    assert_eq!(
        said(&render(&app), "tables/history/count").as_deref(),
        Some("m9 is not in the rows shown")
    );
}

/// The filters narrow the grid's rows, never the history: closed, the drawer's line names the
/// table's newest record, and open at Table scope it lists the same records in the same words,
/// with the same counts in its head, whatever the filters leave out; it says nothing of them.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `drawer` listing only the
/// records that set a cell of a row the grid shows (`sheet.written` narrowed to them before
/// `lines`), the head counts fewer records: `` the head counts what it counted left: Some("2
/// records · 12 cells set") right: Some("7 records · 23 cells set") ``.
#[test]
fn the_drawer_lists_every_record_whatever_the_filters_show() {
    let mut app = open_with(history());
    let closed = said(&render(&app), "tables/history/newest");
    assert!(closed.is_some(), "the closed line names the newest record");
    act(&mut app, "tables/history/show", None);
    let listed = |app: &App| {
        let ui = render(app);
        (
            said(&ui, "tables/history/count"),
            (1..=8)
                .map(|n| said(&ui, &line_key(n, 0)))
                .collect::<Vec<_>>(),
        )
    };
    let (count, lines) = listed(&app);
    act(&mut app, "tables/history/hide", None);
    only(&mut app, "location", "desk");
    assert_eq!(
        said(&render(&app), "tables/count").as_deref(),
        Some("1 of 3 rows")
    );
    assert_eq!(said(&render(&app), "tables/history/newest"), closed);
    act(&mut app, "tables/history/show", None);
    let (filtered_count, filtered_lines) = listed(&app);
    assert_eq!(filtered_count, count, "the head counts what it counted");
    assert_eq!(filtered_lines, lines, "the same lines in the same words");
}

/// [`long_history`] with `n` more rows, each with a nickname of its own, so the list of the
/// nicknames' values runs taller than the window.
fn many_nicknames(n: usize) -> Snapshot {
    let mut snapshot = long_history(120);
    snapshot.sheets[0].rows.extend((0..n).map(|at| {
        row(
            &format!("mc-1{at:015x}"),
            &[
                ("label", &format!("n{at}")),
                ("nickname", &format!("nick {at}")),
            ],
        )
    }));
    snapshot
}

/// A column's list of values opened over the open drawer stays inside the window, laid over the
/// grid and the drawer, and neither moves for it. A press on the list's words or frame over the
/// drawer's lines is the list's and reaches no line under it; the wheel over it scrolls the list,
/// never the drawer's list or the grid; and Escape closes the list first, leaving the drawer open
/// and the keys on the column's mark.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with the list's panel no surface (its
/// `.surface(…)` removed), a point of its frame over the drawer's lines is a drawer line's: ``
/// (216.5, 1749.5) is the list's left:
/// Some("tables/history/at/6060606060606060606060606060606060606060606060606060606060606060/0")
/// right: Some("tables/filter/panel") ``.
#[test]
fn a_columns_list_of_values_over_the_open_drawer_keeps_its_presses_its_wheel_and_escape() {
    use super::super::native_tables_filters::{HAS, MARK};
    let mut app = open_with(many_nicknames(80));
    // A window tall enough for the drawer, which the frame on glass lays out.
    app.on_resize(WID, aterm_window::dpi::PhysicalSize::new(2800, 1800));
    act(&mut app, &format!("tables/cell/{A}/label"), None);
    act(&mut app, "tables/history/show", None);
    let before = presented(&mut app);
    assert!(
        before
            .semantic(&UiKey::new("tables/history/hide"))
            .is_some(),
        "the drawer stands open in {:?}",
        before.bounds
    );
    let (grid, drawer) = (
        rect(&before, "tables/grid"),
        rect(&before, "tables/history"),
    );
    let mark = format!("{MARK}nickname");
    act(&mut app, &mark, None);
    let ui = presented(&mut app);
    let panel = place(&ui, "tables/filter/panel");
    let window = ui.bounds;
    assert!(
        panel.x >= window.x
            && panel.y >= window.y
            && panel.right() <= window.right() + 0.5
            && panel.bottom() <= window.bottom() + 0.5,
        "the list stays inside the window: {panel:?} in {window:?}"
    );
    assert_eq!(
        (rect(&ui, "tables/grid"), rect(&ui, "tables/history")),
        (grid, drawer),
        "nothing moves for the list"
    );
    let was = |app: &App| {
        let view = view_of(app);
        (
            selection(app),
            view.history.open,
            view.history.scope,
            view.history.scroll.get(),
            view.scroll.get(),
        )
    };
    let then = was(&app);
    let count = said(&ui, "tables/count");

    // The wheel over the list's lines where they lie over the drawer's scrolls the list down to
    // its end, its closing words at its foot; the drawer's list and the grid stay.
    let (lines, list) = (
        place(&ui, "tables/filter/lines"),
        rect(&ui, "tables/history/list"),
    );
    let (x, y) = (
        lines.x + 40.0,
        (lines.y.max(list.y) + lines.bottom().min(list.bottom())) / 2.0,
    );
    assert!(
        ui.hits.iter().any(|hit| {
            hit.rect.contains(x, y) && hit.key.as_str().starts_with("tables/history/")
        }),
        "the list's lines {lines:?} lie over the drawer's {list:?} at ({x}, {y}) in {:?}",
        ui.bounds
    );
    let first = |ui: &CompiledUi| place(ui, HAS).y;
    let at_end = |ui: &CompiledUi| {
        ui.paint
            .iter()
            .any(|node| node.key.as_str() == "tables/filter/note" && !node.clipped())
    };
    let top = first(&ui);
    let (px, py) = window_px(&mut app, x, y);
    app.on_cursor_moved(WID, px, py);
    let mut notches = 0;
    while !at_end(&presented(&mut app)) && notches < 100 {
        app.on_mouse_wheel(WID, MouseScrollDelta::LineDelta(0.0, -1.0));
        assert_eq!(was(&app), then, "the drawer's list and the grid stay");
        notches += 1;
    }
    let ui = presented(&mut app);
    assert!(first(&ui) < top, "the list scrolls");
    assert!(at_end(&ui), "to its end, in {notches} notches");

    // Each point of the list over a drawer line that none of the list's lines takes (its
    // closing words, its frame) is the list's.
    let panel = place(&ui, "tables/filter/panel");
    let own: Vec<LogicalRect> = ui
        .hits
        .iter()
        .filter(|hit| {
            hit.key.as_str().starts_with("tables/filter/")
                && hit.key.as_str() != "tables/filter/panel"
        })
        .map(|hit| hit.rect)
        .collect();
    let mut over = Vec::new();
    let mut y = panel.y + 0.5;
    while y < panel.bottom() {
        let mut x = panel.x + 0.5;
        while x < panel.right() {
            if !own.iter().any(|at| at.contains(x, y))
                && ui.hits.iter().any(|hit| {
                    hit.rect.contains(x, y) && hit.key.as_str().starts_with("tables/history/")
                })
            {
                over.push((x, y));
            }
            x += 4.0;
        }
        y += 2.0;
    }
    assert!(
        over.len() > 20,
        "the list's words lie over the drawer's lines: {}",
        over.len()
    );
    for (x, y) in &over {
        assert_eq!(
            ui.hit_test(*x, *y).map(|hit| hit.key.as_str()),
            Some("tables/filter/panel"),
            "({x}, {y}) is the list's"
        );
    }
    // A press there: nothing under the list hears it, and the list stays open as it was.
    let note = place(&ui, "tables/filter/note");
    let (x, y) = over
        .iter()
        .copied()
        .find(|(x, y)| note.contains(*x, *y))
        .expect("the closing words lie over the drawer's lines");
    let (px, py) = window_px(&mut app, x, y);
    app.on_cursor_moved(WID, px, py);
    click(&mut app);
    assert!(view_of(&app).filters.is_open(), "the list stays open");
    assert_eq!(was(&app), then, "nothing under the list heard the press");
    assert_eq!(
        said(&presented(&mut app), "tables/count"),
        count,
        "no filter stands"
    );

    // Escape closes the list, not the drawer.
    press_key(&mut app, NamedKey::Escape);
    let view = view_of(&app);
    assert!(!view.filters.is_open(), "Escape closes the list");
    assert!(view.history.open, "and leaves the drawer open");
    assert_eq!(focus(&app), Some(mark));
}

/// After a read the filters' rest and the drawer's both run (`update` and `poll_tables` run
/// `settle_filters` before `settle_history_keys`), and neither strands keys the other moved:
/// keys on a line of a column's open list of values that the read takes go to the line in its
/// place, with the drawer open beside it, and stay in the list, so Return presses a line of the
/// list and never reaches the page's default button, Write.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `settle_history_keys` judging
/// keys on any control (its test of the drawer's own keys removed), it takes them off the list's
/// lines as soon as the arrows put them there, before the read: `` left:
/// Some("tables/history/hide") right: Some("tables/filter/value/desk") ``.
#[test]
fn a_read_moves_the_lists_keys_and_the_drawers_rest_leaves_them() {
    use super::super::native_tables_filters::{MARK, value_key};
    let mut app = open_with(history());
    let tables = app.native_runtime.tables_mut().unwrap();
    tables.stage("machine", B, "nickname", "Spare 9".into());
    act(&mut app, "tables/history/show", None);
    act(&mut app, &format!("{MARK}location"), None);
    // The choices, then the desk's line: one row each, in the sort's order before the rack's.
    for _ in 0..2 {
        press_key(&mut app, NamedKey::ArrowDown);
    }
    assert_eq!(focus(&app), Some(value_key("desk")));
    presented(&mut app);
    let mut moved = history();
    moved.sheets[0].rows[2]
        .cells
        .insert("location".to_owned(), shown("office"));
    app.native_runtime.tables_mut().unwrap().show(moved);
    app.native_runtime.poll_tables();
    let ui = presented(&mut app);
    assert_eq!(
        focus(&app),
        Some(value_key("office")),
        "the keys stay in the list"
    );
    assert!(ui.semantic(&UiKey::new(focus(&app).unwrap())).is_some());
    assert!(view_of(&app).history.open && view_of(&app).filters.is_open());
    press_key(&mut app, NamedKey::Enter);
    let tables = app.native_runtime.tables_mut().unwrap();
    assert_eq!(
        (tables.writing, tables.staged.len()),
        (false, 1),
        "Return writes nothing"
    );
    assert_eq!(standing(&mut app), 1, "Return pressed the office's line");
}

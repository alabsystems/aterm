// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Tables page under 900 px of width (docs/PLAN.md LT-37): one column, the rail one table
//! picker, and the selected row's pane (or Check's selected finding) below the grid. Every value
//! is synthetic.

use super::native_tables_tests::{MC, WID, act, author, one_sheet, open_with, said, view_of};
use super::*;
use crate::App;
use crate::native_app::ViewMotionCx;
use crate::native_tables_check::{Checked, Found, OPEN};
use crate::native_ui::{CompiledUi, LogicalRect};

/// A window under 900 px across.
const NARROW: LogicalRect = LogicalRect::new(0.0, 0.0, 760.0, 900.0);

fn render_at(app: &App, viewport: LogicalRect) -> CompiledUi {
    let (instance, view) = app
        .active_native_view(WID)
        .expect("the Tables tab is in front");
    let cx = ViewCx {
        viewport,
        animation_phase_ms: 0,
        motion: ViewMotionCx::default(),
        terminal_font_px: 16.0,
        terminal_scale: 1.0,
        semantic_font: None,
        document: None,
    };
    app.native_runtime
        .render(instance, view, &cx)
        .unwrap()
        .compile(viewport)
        .unwrap()
}

/// Where `key` is laid out; it must be listed.
fn rect(ui: &CompiledUi, key: &str) -> LogicalRect {
    ui.semantic(&UiKey::new(key.to_owned()))
        .unwrap_or_else(|| panic!("{key} is listed"))
        .rect
}

/// Whether `key` stands whole inside the window.
fn whole(ui: &CompiledUi, key: &str, viewport: LogicalRect) -> bool {
    let at = rect(ui, key);
    at.x >= 0.0 && at.y >= 0.0 && at.right() <= viewport.width && at.bottom() <= viewport.height
}

/// Scroll the pane until `key` stands whole in the window, as a wheel over it would; the window
/// laid out as it then stands. Panics where no scroll brings it whole.
fn scroll_to(app: &mut App, key: &str) -> CompiledUi {
    for _ in 0..80 {
        let ui = render_at(app, NARROW);
        if whole(&ui, key, NARROW) {
            return ui;
        }
        app.dispatch_native_event(
            WID,
            AppEvent::ScrollAt {
                lines: 1,
                key: UiKey::new("tables/pane/list"),
            },
        )
        .unwrap();
    }
    panic!("{key} stands in the window once the pane scrolls to it");
}

/// A machine whose nickname two Macs set apart, with a history of writes to it, beside a second
/// table the picker chooses; and a finding of each for Check.
fn narrow_snapshot() -> Snapshot {
    let conflict = Shown {
        text: "Backup / Spare".into(),
        by: None,
        values: vec![
            ("Backup".into(), Some(author("m55", 2_000))),
            ("Spare".into(), Some(author("m73", 1_000))),
        ],
        ..Shown::default()
    };
    let mut snapshot = one_sheet(conflict);
    snapshot.sheets[0].written = vec![
        Written {
            operation: [1; 32],
            by: author("m73", 1_000),
            cells: vec![WrittenCell {
                pk: MC.into(),
                column: "nickname".into(),
                now: "Spare".into(),
            }],
        },
        Written {
            operation: [2; 32],
            by: author("m55", 2_000),
            cells: vec![WrittenCell {
                pk: MC.into(),
                column: "nickname".into(),
                now: "Backup".into(),
            }],
        },
    ];
    let mut other = snapshot.sheets[0].clone();
    other.name = "address";
    other.title = "Addresses";
    other.singular = "Address";
    other.rows.clear();
    other.written.clear();
    snapshot.sheets.push(other);
    let mut listed = snapshot.listed[0];
    listed.name = "address";
    listed.title = "Addresses";
    listed.singular = "Address";
    snapshot.listed.push(listed);
    snapshot.check = Some(Box::new(Ok(Checked {
        findings: vec![Found {
            kind: "conflict".into(),
            said: aterm_db::typed::check::said("conflict").into(),
            collection: "machine".into(),
            pks: vec![MC.into()],
            columns: vec!["nickname".into()],
            detail: String::new(),
        }],
        store: crate::native_tables_check::Store::default(),
    })));
    snapshot
}

/// A grid that grows shorter keeps the selected row it showed in view: under 900 px a row
/// selected at the foot of the grid puts the row's pane below and leaves the grid half the
/// room, and the row stays whole in what the grid now shows, as a move of the selection would
/// keep it.
/// NEGATIVE CONTROL: with `grid` not keeping the selected row in view where its room shrank
/// (`room < was` never true), the row is left under the pane: `` the selected row is in the
/// shorter grid ``.
#[test]
fn a_row_selected_at_the_foot_of_a_narrow_grid_stays_in_view() {
    use super::native_tables_tests::{pk_of, place, tall_sheet};
    let mut app = open_with(tall_sheet(80, 3));
    let ui = render_at(&app, NARROW);
    let grid = rect(&ui, "tables/grid");
    let seat = |n: usize| format!("tables/cell/{}/c1/seat", pk_of(n));
    // The lowest row the grid shows whole before any row is selected.
    let last = (0..80)
        .filter(|n| {
            let at = place(&ui, &seat(*n));
            at.y >= grid.y && at.bottom() <= grid.bottom()
        })
        .max()
        .expect("the grid shows rows");
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(last)), None);
    let ui = render_at(&app, NARROW);
    let shorter = rect(&ui, "tables/grid");
    assert!(shorter.height < grid.height, "{shorter:?} under {grid:?}");
    let at = place(&ui, &seat(last));
    assert!(
        at.y >= shorter.y && at.bottom() <= shorter.bottom(),
        "the selected row is in the shorter grid: {at:?} in {shorter:?}"
    );
    assert!(
        rect(&ui, "tables/pane/title").y >= shorter.bottom(),
        "its pane is below the grid"
    );
}

/// Under 900 px of width nothing of the page is lost: the rail is one table picker, which lists
/// the tables in the grid's place and shows the one chosen; the selected row's fields and its
/// conflict with each value's Keep sit below the grid, whole inside the window, and its history
/// is the drawer's under the grid, which the pane's History of this row opens on the row; and
/// Check puts its selected finding, with the way to its row, below its list the same way.
/// NEGATIVE CONTROL: with `page` dropping the pane it lays out below the grid (`below` never
/// pushed), the row's pane is not listed: `` tables/pane/title is listed ``. With `page` laying
/// out the wide page at any width (`narrow = width < 0.0`), the rail stands beside the grid:
/// `` the rail is the picker ``.
#[test]
fn a_narrow_page_keeps_the_selected_rows_fields_conflicts_and_history() {
    let mut app = open_with(narrow_snapshot());
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    let ui = render_at(&app, NARROW);
    assert!(
        ui.semantic(&UiKey::new("tables/rail")).is_none(),
        "the rail is the picker"
    );
    assert!(
        ui.semantic(&UiKey::new("tables/title")).is_none(),
        "the picker names the table in the title's place"
    );
    assert_eq!(
        said(&ui, PICKER).as_deref(),
        Some("Table: Machines; choose another table")
    );
    let grid = rect(&ui, "tables/said");
    // The row's fields, its conflict with a Keep for each value, and the way to its history:
    // below the grid and its line, each brought whole into the window by scrolling the pane.
    for key in [
        "tables/pane/title".to_owned(),
        "tables/pane/field/password".to_owned(),
        "tables/pane/conflict/nickname/head".to_owned(),
        format!("{KEEP}nickname/0"),
        format!("{KEEP}nickname/1"),
        "tables/pane/history".to_owned(),
    ] {
        assert!(
            rect(&ui, &key).y >= grid.bottom(),
            "{key} is below the grid"
        );
        let ui = scroll_to(&mut app, &key);
        assert!(
            rect(&ui, &key).y >= rect(&ui, "tables/said").bottom(),
            "{key} is below the grid"
        );
    }
    // The row's history: the pane's History of this row opens the drawer on the row, under the
    // grid and its line, each of the row's records brought whole into its list by a wheel.
    act(&mut app, "tables/pane/history", None);
    for operation in [[2; 32], [1; 32]] {
        let key = format!(
            "tables/history/at/{}/0",
            aterm_codec::hex::encode_lower(&operation)
        );
        let shown = (0..40).find_map(|_| {
            let ui = render_at(&app, NARROW);
            let (at, list) = (rect(&ui, &key), rect(&ui, "tables/history/list"));
            if at.y >= list.y && at.bottom() <= list.bottom() && whole(&ui, &key, NARROW) {
                return Some(at.y >= rect(&ui, "tables/said").bottom());
            }
            app.dispatch_native_event(
                WID,
                AppEvent::ScrollAt {
                    lines: 1,
                    key: UiKey::new("tables/history/list"),
                },
            )
            .unwrap();
            None
        });
        assert_eq!(
            shown,
            Some(true),
            "{key} is below the grid, whole in the drawer"
        );
    }
    act(&mut app, "tables/history/hide", None);
    // A Keep below the grid stages its value as it does beside it.
    act(&mut app, &format!("{KEEP}nickname/1"), None);
    let ui = render_at(&app, NARROW);
    assert_eq!(
        said(&ui, "tables/bar/count").as_deref(),
        Some("1 change not written")
    );
    let ui = scroll_to(&mut app, &format!("{UNDO}nickname"));
    assert!(said(&ui, &format!("{UNDO}nickname")).is_some());
    act(&mut app, "tables/discard", None);

    // The picker lists every table and Check in the grid's place; a press shows the one chosen.
    act(&mut app, PICKER, None);
    let ui = render_at(&app, NARROW);
    for key in ["tables/sheet/machine", "tables/sheet/address", OPEN] {
        assert!(whole(&ui, key, NARROW), "{key} is in the picker's list");
    }
    assert!(ui.semantic(&UiKey::new("tables/grid")).is_none());
    act(&mut app, "tables/sheet/address", None);
    let ui = render_at(&app, NARROW);
    assert!(!view_of(&app).picking, "a choice closes the picker");
    assert_eq!(
        said(&ui, PICKER).as_deref(),
        Some("Table: Addresses; choose another table")
    );

    // Check from the picker: its selected finding sits below its list, with the way to its row.
    act(&mut app, PICKER, None);
    act(&mut app, OPEN, None);
    act(&mut app, "tables/check/finding/0", None);
    let ui = render_at(&app, NARROW);
    assert_eq!(
        said(&ui, PICKER).as_deref(),
        Some("Table: Check; choose another table")
    );
    let list = rect(&ui, "tables/check/list");
    for key in ["tables/pane/said", "tables/check/row/0/0"] {
        assert!(rect(&ui, key).y >= list.bottom(), "{key} is below the list");
        scroll_to(&mut app, key);
    }
    act(&mut app, "tables/check/row/0/0", None);
    let ui = render_at(&app, NARROW);
    assert_eq!(
        said(&ui, PICKER).as_deref(),
        Some("Table: Machines; choose another table")
    );
    scroll_to(&mut app, "tables/pane/field/password");
}

/// A table or Check chosen from the picker leaves the keys on a control the page still draws,
/// the picker, which names what it shows: Return then opens the list again, and never presses
/// the page's default button, Write, while changes wait.
/// NEGATIVE CONTROL: with a choice leaving the keys on the chosen line (`view.focus(&key)` at
/// any width), the keys rest on a line the closed picker no longer draws: `` the keys rest on
/// a control the page draws ``.
#[test]
fn a_choice_from_the_picker_leaves_the_keys_on_the_picker() {
    use super::native_tables_tests::{key_with, presented};
    use aterm_types::keyboard::{Key, Modifiers, NamedKey};
    let mut app = open_with(narrow_snapshot());
    act(&mut app, &format!("tables/cell/{MC}/nickname"), None);
    act(&mut app, &format!("{KEEP}nickname/1"), None);
    let waiting = |app: &App| said(&render_at(app, NARROW), "tables/bar/count");
    assert_eq!(waiting(&app).as_deref(), Some("1 change not written"));
    for choice in ["tables/sheet/address", OPEN, "tables/sheet/machine"] {
        act(&mut app, PICKER, None);
        act(&mut app, choice, None);
        let ui = presented(&mut app);
        assert!(ui.semantic(&UiKey::new(PICKER)).is_some(), "{choice}");
        let focus = view_of(&app).common.last_focus.clone().expect("a focus");
        assert!(
            ui.semantic(&focus).is_some(),
            "the keys rest on a control the page draws: {focus:?} after {choice}"
        );
        assert_eq!(focus.as_str(), PICKER, "{choice}");
        key_with(&mut app, Key::Named(NamedKey::Enter), Modifiers::empty());
        assert!(view_of(&app).picking, "Return opens the list again");
        assert!(
            !app.native_runtime.tables_mut().unwrap().writing,
            "nothing is written"
        );
        act(&mut app, PICKER, None);
    }
    assert_eq!(waiting(&app).as_deref(), Some("1 change not written"));
}

/// Under 900 px the cell bar keeps its place and its height: between the find field and the
/// grid, a row high and as wide as the grid, whether or not a row is selected, and it names
/// the selected cell there as it does on the wide page. The grid gives the bar its row once:
/// with no row selected the closed history drawer under the line under the grid ends at the
/// window's foot, with one selected the row's pane below the shortened grid does, and on the
/// wide page the drawer does, so the page neither runs past the window nor leaves a row of it
/// unused.
#[test]
fn a_narrow_page_keeps_the_cell_bar_on_the_grid() {
    use super::native_tables_tests::{pk_of, place, tall_sheet};
    let mut app = open_with(tall_sheet(80, 3));
    // The main column's padding ends 16 points above the window's foot.
    let foot = |viewport: LogicalRect| viewport.height - 16.0;
    let bar_on_grid = |ui: &CompiledUi, at: &str| {
        let (bar, grid) = (place(ui, "tables/cellbar"), place(ui, "tables/grid"));
        assert!(
            bar.y >= rect(ui, FIND).bottom(),
            "{at}: under the tools: {bar:?}"
        );
        assert_eq!(bar.bottom(), grid.y, "{at}: on the grid");
        assert_eq!(bar.height, row_height(), "{at}: a row high");
        assert_eq!((bar.x, bar.width), (grid.x, grid.width), "{at}");
        assert!(
            rect(ui, "tables/said").y >= grid.bottom(),
            "{at}: the line is under the grid"
        );
        (bar, grid)
    };
    let ui = render_at(&app, NARROW);
    let (bar, grid) = bar_on_grid(&ui, "no row selected");
    assert_eq!(
        said(&ui, "tables/cellbar/address").as_deref(),
        Some("No cell selected")
    );
    let drawer = rect(&ui, "tables/history");
    assert!(
        drawer.y >= rect(&ui, "tables/said").bottom(),
        "under the line"
    );
    assert!(
        (drawer.bottom() - foot(NARROW)).abs() < 0.5,
        "the page ends at the window's foot: {drawer:?}"
    );

    // A row selected: the grid is half the room, the bar stays on it, the pane ends the page.
    act(&mut app, &format!("tables/cell/{}/c1", pk_of(2)), None);
    let ui = render_at(&app, NARROW);
    let (bar_now, shorter) = bar_on_grid(&ui, "a row selected");
    assert_eq!(bar_now, bar, "the bar stays where it was");
    assert!(shorter.height < grid.height, "{shorter:?} under {grid:?}");
    assert_eq!(
        said(&ui, "tables/cellbar/address").as_deref(),
        Some("m2 \u{b7} C1")
    );
    assert_eq!(
        said(&ui, "tables/cellbar/value").as_deref(),
        Some("a value wide enough to fill its column to the page's cap"),
        "the whole value, which the narrow grid's cell cuts"
    );
    let pane = place(&ui, "tables/pane");
    assert!(
        pane.y >= rect(&ui, "tables/said").bottom(),
        "the pane is under the line"
    );
    assert!(
        (pane.bottom() - foot(NARROW)).abs() < 0.5,
        "the pane ends at the window's foot: {pane:?}"
    );

    // The wide page: the same bar, the line at the window's foot.
    let wide = LogicalRect::new(0.0, 0.0, 1400.0, 900.0);
    let ui = render_at(&app, wide);
    bar_on_grid(&ui, "wide");
    let drawer = rect(&ui, "tables/history");
    assert!(
        drawer.y >= rect(&ui, "tables/said").bottom(),
        "wide: under the line"
    );
    assert!(
        (drawer.bottom() - foot(wide)).abs() < 0.5,
        "the wide page ends at the window's foot: {drawer:?}"
    );
}

/// The grid gives the cell bar its row and keeps, under it, its own lines (the group, header
/// and type lines) and one whole row, beside the history drawer's line under the line under
/// the grid: in a window too short for the three the bar gives way first, and the drawer only
/// where three lines of grid leave it less than its line. Whether either stands is the window's
/// to say, never the selection's or a staged change's: every page judges both as if changes
/// waited to be written and the line under the grid stood three lines high, and a narrow page
/// judges the bar by the halved grid whether or not a row is selected. So selecting a cell,
/// selecting one whose line under the grid wraps onto more lines, past three of them included,
/// and typing into one (which brings the write bar) take away neither the bar that names the
/// cell nor the drawer, nor bring either back, with the drawer closed or open; and a window
/// taller than one that has either has it too. Wherever the bar or the drawer stands, at every
/// height around where each comes, wide and narrow, the page ends inside the window (its write
/// bar, else the drawer, else the line under the grid), and where the bar stands the selected
/// row stands whole in the grid's rows with nothing staged, with a change staged, with a line
/// wrapped onto two or three lines, and with both. A line wrapped past three lines takes its
/// room from the grid, down to the three lines the grid, its bar and the drawer's line keep
/// between them; the write bar still ends inside the window. That is the one change the drawer
/// makes to main's floor: where the line under the grid wraps past three lines, a grid without
/// the bar keeps its three lines with the drawer's line, not over it, as main's grid keeps them
/// with its bar, since the drawer stands whatever is selected and never runs the page past the
/// window.
/// NEGATIVE CONTROL (the first four run on main's page before the drawer's line, the last three
/// on the one budget): with `page` judging the bar on the room the write bar and the wrapped
/// line leave the grid (`steady` the room after both), what is selected and staged adds or
/// removes the bar: `` 760 x 520: wrapped past three lines: neither a selection nor a staged
/// change adds or removes the bar left: true right: false ``. With `page` weighing the bar on
/// three page lines under it (`under_bar = line * 3.0`, the grid's head before its type
/// line), the grid under the bar cuts the selected row: `` 760 x 504: staged and wrapped: the
/// selected row shows whole: LogicalRect { x: 123.0, y: 205.5, width: 260.0, height: 28.0 } in
/// LogicalRect { x: 17.0, y: 207.0, width: 726.0, height: 26.5 } ``. With `page` judging the
/// bar on a line of one row under the grid (`said_judged = said_least`), a wrapped line leaves
/// the grid under the bar less than a row: `` 1400 x 372: staged and wrapped: the selected row
/// shows whole: LogicalRect { x: 311.0, y: 203.0, width: 260.0, height: 28.0 } in LogicalRect
/// { x: 205.0, y: 207.0, width: 826.0, height: 24.0 } ``. With the grid under the bar kept at
/// its lines and a row whatever the room (`floor = cell_line + under_bar`), a line wrapped past
/// three lines pushes the write bar out of the window: `` 1400 x 396: wrapped past three
/// lines: tables/bar ends inside the window: LogicalRect { x: 204.0, y: 341.0, width: 828.0,
/// height: 40.0 } ``. With `page` also judging the drawer on the room as laid out, the
/// drawer's own rule before the one budget (`let drawn = drawn && room - drawer_least >= line *
/// 3.0;` once the line and the write bar are out of `room`), a line wrapped past three lines
/// takes the drawer away when it is selected: `` 760 x 392, the drawer closed: wrapped past three
/// lines: neither a selection nor a staged change adds or removes the drawer left: true right:
/// false ``. With `page` judging the bar without the drawer's line, main's rule
/// (`(if narrow { steady * 0.5 } else { steady }) >= cell_line + under_bar`), the drawer's line
/// cuts the selected row under the bar: `` 1400 x 412, the drawer closed: staged and wrapped: the
/// selected row shows whole: LogicalRect { x: 311.0, y: 205.0, width: 260.0, height: 28.0 } in
/// LogicalRect { x: 205.0, y: 207.0, width: 826.0, height: 26.0 } ``. With main's own floor
/// asked of the page (`grid.height >= three_lines` without the bar, whatever the line), the one
/// budget fails it where a line wrapped past three lines shares the three with the drawer's
/// line: `` 1400 x 392, the drawer closed: wrapped past three lines: the grid keeps three lines:
/// LogicalRect { x: 204.0, y: 106.0, width: 828.0, height: 88.0 } ``.
#[test]
fn a_window_too_short_for_the_cell_bar_keeps_its_grid() {
    short_windows(0);
}

/// [`a_window_too_short_for_the_cell_bar_keeps_its_grid`]'s sweep under one line of the
/// filters' chips: their line comes off the grid's room once, as the tools row does, and counts
/// in the window's judgment of the bar and the drawer, so every law of that sweep holds on what
/// it leaves (whether each stands is the window's and the chips' to say, never a selection's or
/// a staged change's, and the page ends inside the window).
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `page` judging the bar and the
/// drawer as if no chip stood (`steady` given back the chips' line and its gap), the bar stands
/// where the chips leave the grid under it less than its three lines: `` 760 x 412, the drawer
/// closed: staged and wrapped: the grid keeps three lines: LogicalRect { x: 16.0, y: 146.0,
/// width: 728.0, height: 89.0 } ``.
#[test]
fn a_window_too_short_for_the_cell_bar_keeps_its_grid_under_a_line_of_filters() {
    short_windows(1);
}

/// The same sweep under the three lines the filters' chips wrap onto at most.
/// NEGATIVE CONTROL: the one quoted on
/// [`a_window_too_short_for_the_cell_bar_keeps_its_grid_under_a_line_of_filters`], run once on
/// both: `` 760 x 460, the drawer closed: staged and wrapped: the grid keeps three lines:
/// LogicalRect { x: 16.0, y: 206.0, width: 728.0, height: 77.0 } ``.
#[test]
fn a_window_too_short_for_the_cell_bar_keeps_its_grid_under_three_lines_of_filters() {
    short_windows(super::native_tables_tests::CHIP_COLUMNS);
}

/// The sweep of [`a_window_too_short_for_the_cell_bar_keeps_its_grid`], with `chips` filters
/// standing ([`super::native_tables_tests::stand_chips`]) on a
/// [`super::native_tables_tests::chip_sheet`] where `chips` is not 0, over windows taller by the
/// chips' lines and the gap under them, so each leaves the grid the room the same window without
/// them does.
fn short_windows(chips: usize) {
    use super::native_tables_tests::{chip_sheet, pk_of, place, stand_chips, tall_sheet};
    let three_lines = 3.0 * 30.0 * scale().max(1.0);
    // The main column's padding ends 16 points above the window's foot.
    let foot = |viewport: LogicalRect| viewport.height - 16.0;
    let barred = |ui: &CompiledUi| ui.semantic(&UiKey::new("tables/cellbar")).is_some();
    let drawn = |ui: &CompiledUi| ui.semantic(&UiKey::new("tables/history")).is_some();
    let (pk, other) = (pk_of(2), pk_of(5));
    let seat = format!("tables/cell/{pk}/c1/seat");
    // The page with the history drawer closed or `opened`; with `values`, row 2's first cell
    // selected and a conflict of so many long values, which the line under the grid says each
    // of (none selects the cell with no conflict); with `staged`, a change typed into another
    // row and not written, so the write bar stands under the grid.
    let open = |values: Option<usize>, staged: bool, opened: bool| {
        let mut snapshot = if chips == 0 {
            tall_sheet(80, 3)
        } else {
            chip_sheet(80)
        };
        if let Some(values) = values {
            let cell = snapshot.sheets[0].rows[2].cells.get_mut("c1").unwrap();
            cell.values = (0..values)
                .map(|n| {
                    let who = format!("m{}", 20 + n);
                    (
                        format!("the value {who} wrote, said whole by the line under the grid"),
                        Some(author(&who, 1_000)),
                    )
                })
                .collect();
        }
        let mut app = open_with(snapshot);
        if chips > 0 {
            stand_chips(&mut app, chips);
        }
        if values.is_some() {
            act(&mut app, &format!("tables/cell/{pk}/c1"), None);
        }
        if staged {
            app.native_runtime
                .tables_mut()
                .unwrap()
                .stage("machine", &other, "c1", "typed".into());
        }
        if opened {
            act(&mut app, "tables/history/show", None);
        }
        app
    };
    for width in [NARROW.width, 1400.0] {
        let tall = LogicalRect::new(0.0, 0.0, width, 900.0);
        // How much lower the chips' lines and the gap under them set the grid: the windows
        // swept are that much taller, so each leaves the grid the room one without them does.
        let lift = if chips > 0 {
            // One line of chips for one filter, and the three they wrap onto at most for more.
            let lines = if chips == 1 { 1.0 } else { 3.0 };
            let ui = render_at(&open(None, false, false), tall);
            let chips_h = place(&ui, "tables/chips").height;
            assert_eq!(
                chips_h,
                lines * 30.0 * scale().max(1.0),
                "{width}: {chips} chips stand on {lines} lines"
            );
            (chips_h + 10.0).round() as u32
        } else {
            0
        };
        // The line's height is its words' at this width, whatever the window's height.
        let line_h = |app: &App| rect(&render_at(app, tall), "tables/said").height;
        // The fewest values whose line wraps past three of its lines; one fewer is the tallest
        // line of three lines or fewer.
        let longer = (2..16)
            .find(|n| line_h(&open(Some(*n), true, false)) > 3.0 * said_line())
            .expect("a conflict of long values wraps past three lines");
        let one_row = line_h(&open(None, false, false));
        assert_eq!(
            one_row,
            line_h(&open(Some(0), false, false)),
            "{width}: one row each"
        );
        let two_or_three = line_h(&open(Some(longer - 1), false, false));
        assert!(
            two_or_three > one_row && two_or_three <= 3.0 * said_line(),
            "{width}: the conflict's line wraps onto two or three lines: {two_or_three}"
        );
        // Every twenty points, and every four around the heights the drawer and the bar come at.
        let coarse: Vec<u32> = (200 + lift..=900 + lift).step_by(20).collect();
        let bare = open(None, false, false);
        let comes = |stands: &dyn Fn(&CompiledUi) -> bool| {
            *coarse
                .iter()
                .find(|h| {
                    stands(&render_at(
                        &bare,
                        LogicalRect::new(0.0, 0.0, width, **h as f32),
                    ))
                })
                .expect(
                    "a window 900 points tall (and the chips' lines) has the bar and the drawer",
                )
        };
        let mut heights: BTreeSet<u32> = coarse.iter().copied().collect();
        for at in [comes(&drawn), comes(&barred)] {
            heights.extend((at - 40..=at + 40).step_by(4));
        }
        for opened in [false, true] {
            let bare = open(None, false, opened);
            let chosen = open(Some(0), false, opened);
            let staged = open(Some(0), true, opened);
            let wrapped = open(Some(longer - 1), false, opened);
            let both = open(Some(longer - 1), true, opened);
            let long = open(Some(longer), true, opened);
            for app in [&staged, &both, &long] {
                assert_eq!(
                    said(&render_at(app, tall), "tables/bar/count").as_deref(),
                    Some("1 change not written"),
                    "{width}: the write bar stands"
                );
            }
            // Tallest first, as a window shrinks: the grid keeps the selected row it showed
            // whole in view as its room shrinks.
            let (mut bars, mut drawers) = (Vec::new(), Vec::new());
            for &height in heights.iter().rev() {
                let viewport = LogicalRect::new(0.0, 0.0, width, height as f32);
                let drawer = if opened { "open" } else { "closed" };
                let at = |of: &str| format!("{width} x {height}, the drawer {drawer}: {of}");
                let none = render_at(&bare, viewport);
                // Each fixture: whether the write bar stands, whether a row is selected, and
                // whether the line under the grid takes three lines or fewer.
                for (of, app, writes, selected, within) in [
                    ("nothing selected", &bare, false, false, true),
                    ("selected", &chosen, false, true, true),
                    ("staged", &staged, true, true, true),
                    ("wrapped", &wrapped, false, true, true),
                    ("staged and wrapped", &both, true, true, true),
                    ("wrapped past three lines", &long, true, true, false),
                ] {
                    let ui = render_at(app, viewport);
                    assert_eq!(
                        barred(&none),
                        barred(&ui),
                        "{}: neither a selection nor a staged change adds or removes the bar",
                        at(of)
                    );
                    assert_eq!(
                        drawn(&none),
                        drawn(&ui),
                        "{}: neither a selection nor a staged change adds or removes the drawer",
                        at(of)
                    );
                    let grid = place(&ui, "tables/grid");
                    // The page ends inside the window wherever the bar or the drawer stands:
                    // the write bar and its Write, else the drawer, else the line.
                    if barred(&ui) || drawn(&ui) {
                        let ends = if writes {
                            vec!["tables/bar", WRITE]
                        } else if drawn(&ui) {
                            vec!["tables/history"]
                        } else {
                            vec!["tables/said"]
                        };
                        for key in ends {
                            let end = place(&ui, key);
                            assert!(
                                end.bottom() <= foot(viewport) + 0.5,
                                "{}: {key} ends inside the window: {end:?}",
                                at(of)
                            );
                        }
                    }
                    if drawn(&ui) {
                        let drawer = place(&ui, "tables/history");
                        assert!(
                            drawer.y >= place(&ui, "tables/said").bottom() - 0.5,
                            "{}: the drawer stands under the line: {drawer:?}",
                            at(of)
                        );
                    }
                    if !barred(&ui) {
                        // A line wrapped past three lines takes its room from the grid down to
                        // three lines that the grid and the drawer's line keep between them.
                        let line = if drawn(&ui) && !within {
                            history::least()
                        } else {
                            0.0
                        };
                        assert!(
                            grid.height + line >= three_lines,
                            "{}: the grid keeps three lines: {grid:?}",
                            at(of)
                        );
                        continue;
                    }
                    let bar = place(&ui, "tables/cellbar");
                    assert_eq!(bar.bottom(), grid.y, "{}", at(of));
                    assert!(
                        bar.height + grid.height >= three_lines,
                        "{}: the grid and its bar keep three lines: {grid:?}",
                        at(of)
                    );
                    if selected && within {
                        assert!(
                            grid.height >= three_lines,
                            "{}: the grid keeps three lines: {grid:?}",
                            at(of)
                        );
                        let (row, rows) = (place(&ui, &seat), place(&ui, "tables/rows"));
                        assert!(
                            row.y >= rows.y - 0.5 && row.bottom() <= rows.bottom() + 0.5,
                            "{}: the selected row shows whole: {row:?} in {rows:?}",
                            at(of)
                        );
                    }
                }
                bars.push(barred(&none));
                drawers.push(drawn(&none));
            }
            for (what, mut stands) in [("the bar", bars), ("the drawer", drawers)] {
                stands.reverse();
                assert_eq!(
                    stands.first(),
                    Some(&false),
                    "{width}: the shortest has no {what}"
                );
                assert_eq!(
                    stands.last(),
                    Some(&true),
                    "{width}: the tallest has {what}"
                );
                assert!(
                    stands.windows(2).all(|pair| pair[0] <= pair[1]),
                    "{width}: a taller window never loses {what}: {stands:?}"
                );
            }
        }
    }
}

/// The filters' chips count in the window's judgment of the cell bar and the history drawer
/// once, as the tools row does: the least window that has either is taller by the chips' lines
/// and the gap under them, to the point, at both widths, under one line of chips and under the
/// three they wrap onto at most. So a filter added in a window that only just holds the drawer
/// takes it away, and taking the filter off brings the drawer back.
/// NEGATIVE CONTROL (run once, restored with cp and touch): with `page` judging both as if no
/// chip stood (`steady` given back the chips' line and its gap), the least windows do not move:
/// `` 760, 1 chips: the bar comes 40 points later left: 0 right: 40 ``.
#[test]
fn the_chips_count_once_in_the_windows_judgment_of_the_bar_and_the_drawer() {
    use super::native_tables_filters::REMOVE;
    use super::native_tables_tests::{CHIP_COLUMNS, chip_sheet, place, stand_chips};
    let barred = |ui: &CompiledUi| ui.semantic(&UiKey::new("tables/cellbar")).is_some();
    let drawn = |ui: &CompiledUi| ui.semantic(&UiKey::new("tables/history")).is_some();
    let at = |width: f32, height: u32| LogicalRect::new(0.0, 0.0, width, height as f32);
    // The least whole height of a window `width` wide that has what `stands` asks: a taller
    // window never loses the bar or the drawer (the sweeps above).
    let least = |app: &App, width: f32, stands: &dyn Fn(&CompiledUi) -> bool| {
        let (mut short, mut tall) = (100_u32, 1600_u32);
        assert!(stands(&render_at(app, at(width, tall))), "{width} x {tall}");
        assert!(
            !stands(&render_at(app, at(width, short))),
            "{width} x {short}"
        );
        while tall - short > 1 {
            let mid = u32::midpoint(short, tall);
            if stands(&render_at(app, at(width, mid))) {
                tall = mid;
            } else {
                short = mid;
            }
        }
        tall
    };
    for width in [NARROW.width, 1400.0] {
        let mut bare = open_with(chip_sheet(80));
        stand_chips(&mut bare, 0);
        let (bar, drawer) = (least(&bare, width, &barred), least(&bare, width, &drawn));
        for n in [1, CHIP_COLUMNS] {
            let mut app = open_with(chip_sheet(80));
            stand_chips(&mut app, n);
            let taken = (place(&render_at(&app, at(width, 900)), "tables/chips").height + 10.0)
                .round() as u32;
            for (what, was, now) in [
                ("bar", bar, least(&app, width, &barred)),
                ("drawer", drawer, least(&app, width, &drawn)),
            ] {
                assert!(
                    now.abs_diff(was + taken) <= 1,
                    "{width}, {n} chips: the {what} comes {taken} points later left: {} right: \
                     {taken}",
                    now.saturating_sub(was)
                );
            }
            // In the least window that has the drawer, the filter takes it away; taken off, the
            // drawer comes back.
            if n == 1 {
                assert!(!drawn(&render_at(&app, at(width, drawer))), "{width}");
                act(&mut app, &format!("{REMOVE}c3"), None);
                assert!(drawn(&render_at(&app, at(width, drawer))), "{width}");
            }
        }
    }
}

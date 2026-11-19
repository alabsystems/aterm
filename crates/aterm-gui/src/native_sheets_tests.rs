// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sheet set and the hourly chart, drawn from the fleet relations' fixture (FP-01: a fold of
//! synthetic inputs, no live read) and from an hourly relation this file defines. Nothing opens
//! a window: each tree is compiled and its nodes painted to prims.

use super::*;
use crate::native_tables::native_tables_tests::said;
use crate::native_tables_choices::ty_words;
use crate::native_ui::{CompiledUi, UiKey, UiTree};
use aterm_db::fleet::{fixture, fold::fold};
use aterm_db::relation::{col, opt};

const NOW: u64 = 1_800_000_000_000;

/// The fixture's own machine (m12), the row the examples are looked for in first.
const M12: &str = "mc-76c301532f85bec9";

/// An hourly relation in the shape of the fleet log's activity: a row an hour, its use and its
/// capacity, either of which may not have been read.
static HOURS: Schema = Schema {
    name: "hours",
    doc: "one machine's cores by the hour",
    key: &["hour"],
    order: "by hour",
    columns: &[
        col("hour", Ty::Time, "the hour's start"),
        opt("used", Ty::Uint, "cores busy in the hour, hundredths"),
        opt("capacity", Ty::Uint, "cores there were"),
    ],
};

struct Hour {
    at: u64,
    used: Option<u64>,
    capacity: Option<u64>,
}

impl Row for Hour {
    const SCHEMA: &'static Schema = &HOURS;

    fn cells(&self) -> Vec<Cell> {
        vec![
            Cell::of(self.at),
            Cell::read(self.used),
            Cell::read(self.capacity),
        ]
    }
}

fn compiled(root: UiNode, width: f32, height: f32) -> CompiledUi {
    UiTree::new(group("test").layout(Layout::column()).children(vec![root]))
        .compile(LogicalRect::new(0.0, 0.0, width, height))
        .expect("the tree compiles")
}

/// How many nodes of `node`'s tree have keys that start with `prefix`: what a view builds,
/// before the compiler leaves out what lies outside its viewport.
fn built(node: &UiNode, prefix: &str) -> usize {
    usize::from(node.key.as_str().starts_with(prefix))
        + node
            .children
            .iter()
            .map(|child| built(child, prefix))
            .sum::<usize>()
}

fn prims_of<'a>(painted: &'a [(UiKey, Vec<DrawPrim>)], key: &str) -> &'a [DrawPrim] {
    painted
        .iter()
        .find(|(painted, _)| painted.as_str() == key)
        .map(|(_, prims)| prims.as_slice())
        .unwrap_or_else(|| panic!("{key} is painted"))
}

fn words_painted(painted: &[(UiKey, Vec<DrawPrim>)], key: &str) -> Vec<String> {
    prims_of(painted, key)
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Text { s, .. } => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn is(form: Form, drawn: &str, spoken: &str) -> CellFace {
    CellFace {
        form,
        drawn: drawn.to_owned(),
        spoken: spoken.to_owned(),
    }
}

/// TK-35's exit test. One API draws three fleet relations and one relation this file defines,
/// from each one's schema; the module names only `relation` and `typed`, and `fleet` is a unix
/// module the Windows cell's build of it never sees.
///
/// - **L1, columns, types and words from the schema.** Each sheet has a head per column, its
///   name, words and doc the schema's, `key · ` before the type's words of a column of the
///   relation's key whatever its type; an enum cell draws one of its column's words; every
///   enum cell that holds a word draws that word (the word `unread` too, never drawn unread),
///   and every time cell the time's date and minute.
/// - **L2, rows in the relation's order.** The rows' keys are the relation's, in its order, and
///   laid out down the sheet in it; two rows of one key get words of their own (`~2`), each its
///   key's words as one word of a key, and a press finds its row by its word.
/// - **L3, an unread cell is drawn unread.** A cell is drawn `?` and heard "not read" exactly
///   where its relation's cell is `Cell::Unread`; a machine's unread cell paints `?` beside the
///   real zero nearest it in its row, and that row's known-empty cell is blank (the row is m12's
///   where it holds all three; each is found in the fold's data, never named by a column whose
///   reading the fleet's packages own); a cell a row's producer left out is unread.
/// - **L4, an edit is refused.** A second press on the selected cell puts the refusal in words
///   under the sheet, and an edit says the same; no field is ever laid out and the cell still
///   draws what it held.
/// - **L5, the hourly chart.** Its slots are the relation's hours: a gap where an hour has no
///   row, an unread use or no capacity, drawn as dashed rails and never a bar; a zero is a bar a
///   point tall; a use over its capacity a full bar; an hour's whole is its capacity × `per`;
///   an hour past the window is not drawn; `per` 0 and a first hour off the hour are refused;
///   in a slot under 2 wide no bar or rail reaches past its slot.
/// - **L6, a set of sheets, bounded.** The strip names the sheets in the set's order, a press
///   shows one, and a sheet lays out only the rows its room shows.
///
/// NEGATIVE CONTROLS, each run on its own build of the module with the mutant in it:
/// - L1: with `face` leaving out the columns no row holds a value in, `machines: a head per
///   column` fails (55 heads, 56 columns: `update_to`, which no machine's row holds, is gone),
///   observed 2026-11-18 on the fold of FP-09 and FP-12 (*); with an enum's word spelled
///   `Value::to_string()`, `machines: "\"active\"" is a word of status` fails; with a key column
///   of type `key` given `key` alone (`if key && !matches!(…, Ty::Key)`), `machines: machine's
///   words` fails (`"key"`, not `"key · key"`), observed 2026-11-18; with an enum's word drawn
///   `Form::Value`, `machines mc-0874… status` fails (`form: Value, drawn: "active"`, not
///   `form: Word`), and with a time drawn as its milliseconds, `machines mc-76c3… reach_at`
///   fails (`drawn: "1799999980000"`, not its date and minute), both observed 2026-11-18 (*).
/// - L2: with `face` sorting its rows by their keys' text, `machines: rows in the relation's
///   order` fails (`mc-01b8…` first, where the relation has `mc-0874…`), observed 2026-11-18
///   with the enum and time check after it (*); with the `~n` loop skipped (`while false &&
///   …`), the second row's word is the first's, not `{first}~2`; with a row's word its key's
///   words as they are (no `key_word`), the first row's word holds the stamp's space, not
///   `%20`; both observed 2026-11-18.
/// - L3: with `Cell::Unread` sent down the empty arm, the form check fails (`mc-0874…
///   reach_at: CellFace { form: Empty, drawn: "", spoken: "empty" }`); drawn `0`, the cell is
///   `drawn: "0"` where `"?"` is asserted; drawn through `Cell::json`, it is `drawn: "null"`;
///   with a cell a row left out taken as `Cell::Empty`, `a cell a row left out is unread` fails
///   (`form: Empty, drawn: ""`, not `form: Unread, drawn: "?"`), observed 2026-11-18; with a
///   cell heard by its column's name, not its label, the example unread cell (m12's
///   `update_to` on that fold) is heard `update_to of mc-76c3…: not read`, observed 2026-11-18
///   (*).
/// - L4: with the refusal never laid out under the sheet, and apart with a second press that
///   refuses without calling `edit`, `a second press on the selected cell is refused in words
///   under the sheet` fails (`said(…/machines/refused)` is `None`), each observed 2026-11-18;
///   with the refusal naming its column by its name, not its label, the refusal fails to say
///   its column's label (`"update_to cannot be edited here: Machines is computed" says UPDATE
///   TO`), observed 2026-11-18 (*).
/// - L5: with a gap drawn as an empty bar (a Panel 0 tall) and no dashes, `a gap's rail along
///   the foot` fails (`[]`, not `[(100.0, 39.0), (200.0, 19.0), (300.0, 19.0)]`); with native_ui's
///   paint arm for the chart taken out (its label painted as Body text), the same rail check
///   fails with `[]`; with a use of 0 taken for a gap, the slots fail at hour 12 (`Gap`, not
///   `Share { used: 0, whole: 800 }`); with an hour that has no row carrying the hour before
///   it, they fail at hours 5 and 6 (`Share { used: 140, whole: 800 }`, not `Gap`); with a
///   column a spec names but the relation lacks taken as the first of its type,
///   `refused(&hours, &ChartSpec { used: "nope", ..spec })` fails. Observed 2026-11-18: with
///   `whole: capacity` (no `per`), the slots fail at hour 0 (`Share { used: 20, whole: 8 }`,
///   not `whole: 800`); with the window's filter `*at <= spec.hours`, the hour after the last
///   indexes past the 24 slots (`index out of bounds: the len is 24 but the index is 24`);
///   with the `per == 0` refusal, then the on-the-hour refusal, taken out, `per 0`, then `a
///   first hour not on the hour`, fails; with a point's gutter kept in a slot under 2 wide,
///   `each hour's mark within its slot` fails (the first gap's rail spans 0.0 to -0.29).
/// - L6: with a sheet laying out every row, `217 of 217 rows built, at most 40` fails. (Counted
///   in the compiled tree, it passed: the compiler leaves out a node wholly outside its clip;
///   only the fit check saw the row the viewport's foot cut. The count is of the built tree.)
///
/// (*) Observed on a build that held several mutants, each switched on alone for its run, where
/// the same build with none switched on passed: L1's column drop, L3's cell heard by name and
/// L4's refusal by name on one build; L2's sort and L1's enum and time mutants on another.
#[test]
fn a_sheet_and_a_chart_are_drawn_from_a_relations_schema_and_an_unread_cell_is_drawn_unread() {
    let tables = fold(&fixture::inputs(NOW), NOW).tables;
    let relations = [
        Relation::of("Machines", &tables.machines),
        Relation::of("AI accounts", &tables.accounts),
        Relation::of("Agents", &tables.agents),
    ];
    let set: Vec<SheetFace> = relations.iter().map(face).collect();
    let roles = Roles::from_theme(aterm_render::Theme::default());
    let (machines, accounts, agents) = (&set[0], &set[1], &set[2]);
    let column = |sheet: &SheetFace, name: &str| {
        sheet
            .heads
            .iter()
            .position(|head| head.name == name)
            .unwrap_or_else(|| panic!("{}: a column {name}", sheet.name))
    };
    // Where the examples below are looked for first; a fold without it still has examples.
    let m12 = machines.rows.iter().position(|row| row.key == M12);

    // L1: a head per column of the schema, in its order, with its name, words and doc; each
    // word an enum cell draws is one of its column's.
    for (relation, sheet) in relations.iter().zip(&set) {
        let schema = relation.schema;
        assert_eq!(sheet.name, schema.name);
        assert_eq!(
            sheet.heads.len(),
            schema.columns.len(),
            "{}: a head per column",
            schema.name
        );
        for (head, col) in sheet.heads.iter().zip(schema.columns) {
            assert_eq!(head.name, col.column.name);
            assert_eq!(head.doc, col.doc);
            assert_eq!(head.label, crate::tables_store::label_of(col.column.name));
            // Its type's words, the Tables page's, after `key · ` on a column of the key
            // whatever its type.
            let ty = ty_words(&col.column.ty, &|target: &str| target.to_owned());
            let want = if schema.key.contains(&col.column.name) {
                format!("key \u{b7} {ty}")
            } else {
                ty
            };
            assert_eq!(head.words, want, "{}: {}'s words", schema.name, head.name);
        }
        for row in &sheet.rows {
            assert_eq!(row.cells.len(), schema.columns.len());
            for (drawn, col) in row.cells.iter().zip(schema.columns) {
                if drawn.form == Form::Word {
                    let Ty::Enum(words) = col.column.ty else {
                        panic!("{}: a word in {}", schema.name, col.column.name);
                    };
                    assert!(
                        words.contains(&drawn.drawn.as_str()),
                        "{}: {:?} is a word of {}",
                        schema.name,
                        drawn.drawn,
                        col.column.name
                    );
                }
            }
        }
    }
    // The key's head is told apart from another column of keys.
    let words = |sheet: &SheetFace, name: &str| sheet.heads[column(sheet, name)].words.clone();
    assert_ne!(
        words(accounts, "account"),
        words(accounts, "provider"),
        "the key of accounts and a column of keys"
    );

    // L2: the rows' keys are the relation's key cells, in the relation's order.
    for (relation, sheet) in relations.iter().zip(&set) {
        let schema = relation.schema;
        let keys: Vec<String> = relation
            .rows
            .iter()
            .map(|cells| {
                schema
                    .key
                    .iter()
                    .map(|name| {
                        let at = schema.position(name).expect("a key column");
                        cells[at].shown(&schema.columns[at].column.ty)
                    })
                    .collect::<Vec<_>>()
                    .join(" \u{b7} ")
            })
            .collect();
        let drawn: Vec<String> = sheet.rows.iter().map(|row| row.key.clone()).collect();
        assert_eq!(drawn, keys, "{}: rows in the relation's order", schema.name);
    }
    // L1 (cont.), on rows L2 has shown to be the relation's, in its order: every enum cell that
    // holds one of its column's words draws that word, the word `unread` too, which is never a
    // cell nobody read; and every time cell draws the time's date and minute on this Mac's
    // clock. Whatever the fold read: no column whose reading the fleet's packages own is named.
    let (mut enum_words, mut unread_words, mut times) = (0, 0, 0);
    for (relation, sheet) in relations.iter().zip(&set) {
        for (cells, row) in relation.rows.iter().zip(&sheet.rows) {
            for ((cell, drawn), col) in cells.iter().zip(&row.cells).zip(relation.schema.columns) {
                let Cell::Value(value) = cell else {
                    continue;
                };
                let at = format!("{} {} {}", relation.schema.name, row.key, col.column.name);
                if let (Ty::Enum(words), Some(word)) = (&col.column.ty, value.as_str())
                    && words.contains(&word)
                {
                    enum_words += 1;
                    unread_words += usize::from(word == "unread");
                    assert_eq!(*drawn, is(Form::Word, word, word), "{at}");
                } else if let (Ty::Time, Some(at_ms)) = (&col.column.ty, value.as_u64()) {
                    times += 1;
                    let stamp = stamp_words(at_ms);
                    assert_eq!(*drawn, is(Form::Value, &stamp, &stamp), "{at}");
                }
            }
        }
    }
    assert!(
        enum_words > 0 && unread_words > 0 && times > 0,
        "{enum_words} enum words, {unread_words} of them `unread`, and {times} times drawn"
    );
    let mut view = SheetsView::new("sheets");
    let ui = compiled(view.node(&set, 2400.0, 900.0), 2400.0, 900.0);
    let lanes: Vec<f32> = machines
        .rows
        .iter()
        .map_while(|row| {
            ui.semantic(&UiKey::new(format!(
                "sheets/sheet/machines/row/{}",
                row.word
            )))
            .map(|lane| lane.rect.y)
        })
        .collect();
    assert!(lanes.len() > 10, "{} rows laid out", lanes.len());
    assert!(
        lanes.windows(2).all(|pair| pair[0] < pair[1]),
        "rows laid down the sheet in the relation's order: {lanes:?}"
    );
    // A head is heard as its column's name, its type's words and what its schema says it holds.
    let reach = &relations[0].schema.columns[column(machines, "reach")];
    assert_eq!(
        said(&ui, "sheets/sheet/machines/head/reach"),
        Some(format!(
            "{}, {}: {}",
            crate::tables_store::label_of("reach"),
            ty_words(&reach.column.ty, &|target: &str| target.to_owned()),
            reach.doc
        ))
    );
    let rows = |sheet: &SheetFace| format!("{}, {} rows", sheet.title, sheet.rows.len());
    assert_eq!(
        said(&ui, "sheets"),
        Some(format!("{}, read only", rows(machines)))
    );

    // L6: the strip names the set's sheets in its order; a press shows one; a sheet lays out
    // only the rows its room shows.
    let mut tabs = SheetsView::new("sheets");
    let tree = tabs.node(&set, 1400.0, 900.0);
    let lanes = built(&tree, "sheets/sheet/machines/row/");
    let most = (900.0 / row_height()) as usize + 8;
    assert!(
        (1..=most).contains(&lanes),
        "{lanes} of {} rows built, at most {most}",
        machines.rows.len()
    );
    let ui = compiled(tree, 1400.0, 900.0);
    let strip: Vec<String> = set
        .iter()
        .map(|sheet| said(&ui, &format!("sheets/strip/{}", sheet.name)).expect("a sheet's tab"))
        .collect();
    assert_eq!(
        strip,
        relations
            .iter()
            .map(|relation| format!("{}, {} rows", relation.title, relation.rows.len()))
            .collect::<Vec<_>>()
    );
    assert!(ui.fit_failures().is_empty(), "{:#?}", ui.fit_failures());
    assert_eq!(tabs.press(&set, "sheets/strip/agents"), Pressed::Shown);
    let tree = tabs.node(&set, 1400.0, 900.0);
    assert_eq!(built(&tree, "sheets/sheet/machines/"), 0);
    let ui = compiled(tree, 1400.0, 900.0);
    assert!(
        said(
            &ui,
            &format!("sheets/sheet/agents/cell/{}/sid", agents.rows[0].word)
        )
        .is_some(),
        "an agents cell is laid out"
    );
    assert_eq!(
        said(&ui, "sheets"),
        Some(format!("{}, read only", rows(agents)))
    );

    // L3: a cell is drawn `?` and heard "not read" exactly where the relation's cell is unread,
    // never 0 and never blank; a real zero is 0 and a known-empty cell blank.
    let mut unread = 0;
    for (relation, sheet) in relations.iter().zip(&set) {
        for (cells, row) in relation.rows.iter().zip(&sheet.rows) {
            for ((cell, drawn), col) in cells.iter().zip(&row.cells).zip(relation.schema.columns) {
                let not_read = *cell == Cell::Unread;
                assert_eq!(
                    drawn.form == Form::Unread,
                    not_read,
                    "{} {}: {drawn:?}",
                    row.key,
                    col.column.name
                );
                if not_read {
                    unread += 1;
                    assert_eq!(*drawn, is(Form::Unread, "?", UNREAD));
                }
            }
        }
    }
    assert!(unread > 1000, "{unread} unread cells");
    // The examples drawn below, found in the fold's own data, never named by a column whose
    // reading the fleet's packages own: a machine's cell nobody read and the real zero nearest
    // it in its row, of a row that holds a known-empty cell too, m12's where it holds all three.
    let schema = relations[0].schema;
    let examples = |cells: &[Cell]| {
        let empty = cells.iter().position(|cell| *cell == Cell::Empty)?;
        let zeros: Vec<usize> = (0..cells.len())
            .filter(|at| {
                matches!(schema.columns[*at].column.ty, Ty::Uint)
                    && matches!(&cells[*at], Cell::Value(value) if value.as_u64() == Some(0))
            })
            .collect();
        let (unread, zero) = (0..cells.len())
            .filter(|at| cells[*at] == Cell::Unread)
            .flat_map(|unread| zeros.iter().map(move |zero| (unread, *zero)))
            .min_by_key(|(unread, zero)| unread.abs_diff(*zero))?;
        Some((unread, zero, empty))
    };
    let (example, (unread_at, zero_at, empty_at)) = m12
        .into_iter()
        .chain(0..relations[0].rows.len())
        .find_map(|row| examples(&relations[0].rows[row]).map(|at| (row, at)))
        .expect("a machine with a cell nobody read, a real zero and a known-empty cell");
    let example_cells = &machines.rows[example].cells;
    assert_eq!(example_cells[unread_at], is(Form::Unread, "?", "not read"));
    assert_eq!(example_cells[zero_at], is(Form::Value, "0", "0"));
    assert_eq!(example_cells[empty_at], is(Form::Empty, "", "empty"));
    let (unread_name, zero_name) = (
        schema.columns[unread_at].column.name,
        schema.columns[zero_at].column.name,
    );
    // A row whose producer stated fewer cells than its schema has columns: the cells it left
    // out nobody read, never blank.
    let short = face(&Relation {
        title: "Short".to_owned(),
        schema: &HOURS,
        rows: vec![vec![Cell::of(NOW)]],
    });
    assert_eq!(
        short.rows[0].cells[1..],
        [is(Form::Unread, "?", UNREAD), is(Form::Unread, "?", UNREAD)],
        "a cell a row left out is unread"
    );
    view.scroll_to(example, unread_at.min(zero_at));
    let ui = compiled(view.node(&set, 2400.0, 900.0), 2400.0, 900.0);
    let (example_word, example_key) = (
        machines.rows[example].word.clone(),
        machines.rows[example].key.clone(),
    );
    let cell_key = |name: &str| format!("sheets/sheet/machines/cell/{example_word}/{name}");
    // A cell is heard as its column's label of its row's key, then what it holds.
    let (unread_label, zero_label) = (
        crate::tables_store::label_of(unread_name),
        crate::tables_store::label_of(zero_name),
    );
    assert_eq!(
        said(&ui, &cell_key(unread_name)).as_deref(),
        Some(format!("{unread_label} of {example_key}: not read").as_str())
    );
    assert_eq!(
        said(&ui, &cell_key(zero_name)).as_deref(),
        Some(format!("{zero_label} of {example_key}: 0").as_str())
    );
    let painted = ui.painted(roles);
    let unread_key = format!("{}/text", cell_key(unread_name));
    let zero_key = format!("{}/text", cell_key(zero_name));
    assert_eq!(words_painted(&painted, &unread_key), ["?"]);
    assert_eq!(words_painted(&painted, &zero_key), ["0"]);
    assert!(ui.fit_failures().is_empty(), "{:#?}", ui.fit_failures());

    // L4: a press selects a cell; a second press on it, or an edit, is refused in words under
    // the sheet; nothing lays out a field, and the cell still draws what it held.
    assert_eq!(SheetsView::new("sheets").edit(&set), None);
    assert_eq!(view.press(&set, &cell_key(unread_name)), Pressed::Selected);
    let ui = compiled(view.node(&set, 2400.0, 900.0), 2400.0, 900.0);
    assert_eq!(
        said(&ui, &cell_key(unread_name)).as_deref(),
        Some(format!("{unread_label} of {example_key}: not read, selected").as_str())
    );
    assert!(said(&ui, "sheets/sheet/machines/refused").is_none());
    assert_eq!(view.press(&set, &cell_key(unread_name)), Pressed::Refused);
    // The second press itself puts the refusal under the sheet; an edit then says it again.
    let ui = compiled(view.node(&set, 2400.0, 900.0), 2400.0, 900.0);
    let refusal = said(&ui, "sheets/sheet/machines/refused")
        .expect("a second press on the selected cell is refused in words under the sheet");
    for word in ["cannot", unread_label.as_str(), "Machines"] {
        assert!(refusal.contains(word), "{refusal:?} says {word}");
    }
    assert_eq!(
        view.edit(&set),
        Some(refusal.as_str()),
        "an edit is refused"
    );
    assert!(
        ui.semantics
            .iter()
            .all(|node| node.role != SemanticRole::TextField),
        "no field is laid out"
    );
    let painted = ui.painted(roles);
    assert_eq!(words_painted(&painted, &unread_key), ["?"]);
    assert_eq!(words_painted(&painted, &zero_key), ["0"]);
    assert_eq!(view.press(&set, "elsewhere/x"), Pressed::NotMine);
    assert_eq!(view.press(&set, &cell_key(zero_name)), Pressed::Selected);
    let ui = compiled(view.node(&set, 2400.0, 900.0), 2400.0, 900.0);
    assert!(
        said(&ui, "sheets/sheet/machines/refused").is_none(),
        "another cell selected, the refusal is gone"
    );

    // L5: the hourly chart's slots are the relation's hours, by its columns' names and types:
    // hundredths of a core used of whole cores, so each hour's whole is its capacity × 100.
    let hour = HOUR_MS;
    let h0 = NOW / hour * hour - 24 * hour;
    let figures = |n: u64| match n {
        10 => (None, Some(8)),
        12 => (Some(0), Some(8)),
        15 => (Some(300), Some(0)),
        20 => (Some(900), Some(6)),
        n => (Some(30 * n + 20), Some(8)),
    };
    let mut rows: Vec<Hour> = (0..24)
        .filter(|n| *n != 5 && *n != 6)
        .map(|n| {
            let (used, capacity) = figures(n);
            Hour {
                at: h0 + n * hour,
                used,
                capacity,
            }
        })
        .collect();
    // An hour before the chart's first, and the hour after its last and one further on.
    let outside = |at: u64| Hour {
        at,
        used: Some(100),
        capacity: Some(8),
    };
    rows.insert(0, outside(h0 - hour));
    rows.extend([outside(h0 + 24 * hour), outside(h0 + 30 * hour)]);
    let hours = Relation::of("Cores busy", &rows);
    let spec = ChartSpec {
        hour: "hour",
        used: "used",
        capacity: "capacity",
        per: 100,
        from_ms: h0,
        hours: 24,
    };
    let drawn = chart(&hours, &spec).expect("the chart of the hours");
    let gaps = [5, 6, 10, 15];
    let want = |n: usize| match n {
        _ if gaps.contains(&n) => Slot::Gap,
        n => {
            let (used, capacity) = figures(n as u64);
            Slot::Share {
                used: used.expect("a use"),
                whole: capacity.expect("a capacity") * 100,
            }
        }
    };
    assert_eq!(drawn.slots, (0..24).map(want).collect::<Vec<_>>());
    // A capacity in `used`'s own units is its whole as it is.
    let per_one = chart(&hours, &ChartSpec { per: 1, ..spec }).expect("the chart, per 1");
    assert_eq!(
        per_one.slots[20],
        Slot::Share {
            used: 900,
            whole: 6
        }
    );
    assert_eq!(
        drawn.slots[12],
        Slot::Share {
            used: 0,
            whole: 800
        }
    );
    assert_eq!(
        drawn.slots[20],
        Slot::Share {
            used: 900,
            whole: 600
        }
    );
    let (chart_w, chart_h) = (480.0, 60.0);
    let chart_ui = compiled(
        chart_node("sheets/chart", &drawn, chart_w, chart_h),
        chart_w,
        chart_h,
    );
    let heard = said(&chart_ui, "sheets/chart").expect("the chart is heard");
    assert!(heard.contains("20 of 24 hours sampled"), "{heard}");
    let rect = chart_ui
        .semantic(&UiKey::new("sheets/chart"))
        .expect("the chart is laid out")
        .rect;
    let painted = chart_ui.painted(roles);
    let prims = prims_of(&painted, "sheets/chart");
    let slot_w = rect.width / 24.0;
    let rails = |at_y: f32| {
        prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Dashed { x, y, w, h, .. } if *h == 0.0 && *w > 0.0 && *y == at_y => {
                    Some((*x, *w))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let runs: Vec<(f32, f32)> = [(5, 7), (10, 11), (15, 16)]
        .iter()
        .map(|&(from, to)| {
            (
                rect.x + from as f32 * slot_w,
                (to - from) as f32 * slot_w - 1.0,
            )
        })
        .collect();
    assert_eq!(
        rails(rect.bottom() - 1.0),
        runs,
        "a gap's rail along the foot"
    );
    assert_eq!(rails(rect.y), runs, "a gap's rail along the head");
    let bars: Vec<(f32, f32, f32)> = prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Panel { x, y, w, h, .. } => Some((x + w / 2.0, *y, *h)),
            _ => None,
        })
        .collect();
    for n in 0..24 {
        let left = rect.x + n as f32 * slot_w;
        let in_slot: Vec<(f32, f32)> = bars
            .iter()
            .filter(|(centre, _, _)| *centre >= left && *centre < left + slot_w)
            .map(|(_, y, h)| (*y, *h))
            .collect();
        let tall = match want(n) {
            Slot::Gap => {
                assert!(in_slot.is_empty(), "hour {n} is a gap, no bar: {in_slot:?}");
                continue;
            }
            Slot::Share { used, whole } => (chart_h * (used as f32 / whole as f32).min(1.0))
                .round()
                .max(1.0),
        };
        assert_eq!(in_slot, [(rect.bottom() - tall, tall)], "hour {n}'s bar");
    }
    assert_eq!(
        bars.iter().filter(|(_, _, h)| *h == 1.0).count(),
        1,
        "the zero's bar is a point tall"
    );
    assert!(
        chart_ui.fit_failures().is_empty(),
        "{:#?}",
        chart_ui.fit_failures()
    );
    // A week's hours in a chart narrower than they are many: no bar or gap's rail reaches past
    // its slot or over a neighbour's.
    let week: Vec<&str> = (0..168)
        .map(|n| if n % 3 == 0 { "-" } else { "1/2" })
        .collect();
    let narrow = LogicalRect::new(0.0, 0.0, 120.0, 40.0);
    let mut prims = Vec::new();
    paint_chart(&mut prims, narrow, Some(&week.join(",")), &roles);
    let mut spans: Vec<(f32, f32)> = prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Panel { x, w, .. } => Some((*x, x + w)),
            DrawPrim::Dashed { x, y, w, .. } if *y == narrow.bottom() - 1.0 => Some((*x, x + w)),
            _ => None,
        })
        .collect();
    assert_eq!(spans.len(), 168, "a bar or a rail an hour");
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(
        spans.iter().all(|(from, to)| from < to)
            && spans.windows(2).all(|pair| pair[0].1 <= pair[1].0 + 1e-3)
            && spans
                .last()
                .is_some_and(|(_, to)| *to <= narrow.right() + 1e-3),
        "each hour's mark within its slot: {spans:?}"
    );
    let refused = |relation: &Relation, spec: &ChartSpec| chart(relation, spec).is_err();
    assert!(refused(&hours, &ChartSpec { per: 0, ..spec }), "per 0");
    assert!(
        refused(
            &hours,
            &ChartSpec {
                from_ms: h0 + 1,
                ..spec
            }
        ),
        "a first hour not on the hour"
    );
    assert!(refused(
        &relations[0],
        &ChartSpec {
            hour: "reach",
            used: "cpu_used_x100",
            capacity: "cpu_cores",
            per: 100,
            ..spec
        }
    ));
    assert!(refused(
        &hours,
        &ChartSpec {
            used: "nope",
            ..spec
        }
    ));
    let twice = [
        Hour {
            at: h0 + 3 * hour,
            used: Some(1),
            capacity: Some(2),
        },
        Hour {
            at: h0 + 3 * hour,
            used: Some(1),
            capacity: Some(2),
        },
    ];
    assert!(refused(&Relation::of("Twice", &twice), &spec));

    // L2 (cont.): two rows a relation names alike are drawn with words of their own, each its
    // key's words as one word of a key, and a press finds its row by its word.
    let alike = [face(&Relation::of("Twice", &twice))];
    let (first, second) = (&alike[0].rows[0].word, &alike[0].rows[1].word);
    assert_eq!(*first, key_word(&stamp_words(h0 + 3 * hour)));
    assert_eq!(*second, format!("{first}~2"));
    let mut pick = SheetsView::new("twice");
    let used_key = |word: &str| format!("twice/sheet/hours/cell/{word}/used");
    assert_eq!(pick.press(&alike, &used_key(second)), Pressed::Selected);
    let ui = compiled(pick.node(&alike, 1400.0, 400.0), 1400.0, 400.0);
    let picked = |word: &str| {
        said(&ui, &used_key(word))
            .unwrap_or_else(|| panic!("{word}'s cell is laid out"))
            .ends_with(", selected")
    };
    assert!(
        picked(second) && !picked(first),
        "the second row is selected, not the first"
    );
}

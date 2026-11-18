// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The history drawer's Registry scope: its lines on hand-made records (kinds, folds, days,
//! not counted, what a selected record had not seen, its one Tab stop) and, on scratch
//! registries, its filters and a re-read that brings new records. Every value is synthetic.

use super::super::super::native_tables_tests::{
    MC, WID, act, one_sheet, open_with, put, render, said, settle, two_macs, view_of, with_view_mut,
};
use super::super::super::{Shown, TablesApp};
use super::super::{FIND_RECORDS, Line, key_of};
use super::*;
use crate::App;
use crate::native_app::{AppEvent, SemanticInput};
use crate::native_ui::{CompiledUi, UiKey};
use aterm_db::alab::Registry as Store;

/// A claimed time, and a day.
const T0: u64 = 1_700_000_000_000;
const DAY: u64 = 86_400_000;

fn op(n: u8) -> [u8; 32] {
    [n; 32]
}

fn hex(n: u8) -> String {
    aterm_codec::hex::encode_lower(&op(n))
}

fn record_key(n: u8) -> String {
    format!("tables/history/record/{}", hex(n))
}

fn location(pk: &str, place: &str) -> LoggedCell {
    LoggedCell {
        collection: "machine".into(),
        pk: pk.into(),
        column: Some("location".into()),
        words: format!("Location \u{2192} {place}"),
    }
}

/// Record `n`, by `label` of `kind`, claimed at `at_ms`; its place in the causal order is `n`.
fn logged(n: u8, kind: AuthorKind, label: &str, at_ms: u64, cells: Vec<LoggedCell>) -> Logged {
    Logged {
        operation: op(n),
        by: Author {
            kind,
            label: Some(label.into()),
            id: format!("{:0>8}", label.trim_start_matches('m')),
            at_ms,
            hlc: (T0 + u64::from(n), 0),
        },
        machine: None,
        inert: false,
        cells,
    }
}

/// The page on a hand-made sheet, its drawer at Registry scope listing `records` (newest first)
/// as if read, nothing older left.
fn listing(records: Vec<Logged>) -> App {
    let mut app = open_with(one_sheet(Shown {
        text: "m9".into(),
        ..Shown::default()
    }));
    let tables = tables_mut(&mut app);
    tables.registry.records = records;
    tables.registry.read = true;
    act(&mut app, "tables/history/show", None);
    act(&mut app, "tables/history/scope/registry", None);
    app
}

fn tables_mut(app: &mut App) -> &mut TablesApp {
    app.native_runtime
        .tables_mut()
        .expect("the Tables page is open")
}

fn lines_of(app: &App) -> Vec<Line> {
    let view = view_of(app);
    lines(
        app.native_runtime
            .tables()
            .expect("the Tables page is open"),
        view,
        &view.history.runs,
    )
}

/// Each line, as the test reads it: a day's head, a fold and its count, a record's digest's
/// first byte, a note, the way to older records.
fn shape(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line {
            Line::Day(day) => format!("day {day}"),
            Line::Run {
                records, oldest, ..
            } => format!("fold of {records} to {}", oldest[0]),
            Line::Record {
                operation, apart, ..
            } => format!("{}{}", operation[0], if *apart { " apart" } else { "" }),
            Line::Note(words) => format!("note {words}"),
            Line::More => "more".to_owned(),
            Line::Wrote { part, .. } => format!("wrote {}", part.slug()),
            other => format!("{other:?}"),
        })
        .collect()
}

/// A line says its kind as every line of the page does: no word for an explicit write, `aterm`
/// or `import` otherwise; three consecutive records of one machine and kind fold into a line
/// that says it is a fold of three records and never one write, while a record of another kind
/// by the same machine breaks the fold. A line names its table and its row, or how many rows,
/// and its count of cells; a record that projects nothing says `not counted`, and selected,
/// says so above what it wrote, then what it had not seen.
/// NEGATIVE CONTROL: with `groups` folding records of any kind by one machine
/// (`next.by.kind == first.by.kind ||` in place of `&&`), the aterm record joins the fold:
/// `` left: ["day …", "fold of 4 to 3", "2", "1", …] right: ["day …", "fold of 3 to 4", "3", …]
/// ``.
#[test]
fn a_line_says_its_kind_and_a_fold_is_never_one_write() {
    let mut inert = logged(
        1,
        AuthorKind::Explicit,
        "m55",
        T0,
        vec![location(MC, "rack a")],
    );
    inert.inert = true;
    let records = vec![
        logged(
            6,
            AuthorKind::Explicit,
            "m55",
            T0 + 6,
            vec![location(MC, "rack f")],
        ),
        logged(
            5,
            AuthorKind::Explicit,
            "m55",
            T0 + 5,
            vec![location(MC, "rack e")],
        ),
        logged(
            4,
            AuthorKind::Explicit,
            "m55",
            T0 + 4,
            vec![location(MC, "rack d")],
        ),
        logged(
            3,
            AuthorKind::Aterm,
            "m55",
            T0 + 3,
            vec![location(MC, "rack c")],
        ),
        logged(
            2,
            AuthorKind::Import,
            "m73",
            T0 + 2,
            vec![
                location(MC, "rack b"),
                location("mc-000000000000000a", "rack b"),
            ],
        ),
        inert,
    ];
    let mut app = listing(records);
    let day = super::super::day_words(T0);
    assert_eq!(
        shape(&lines_of(&app)),
        [
            format!("day {day}"),
            "fold of 3 to 4".to_owned(),
            "3".to_owned(),
            "2".to_owned(),
            "1".to_owned(),
            "note Start of the registry\u{2019}s records".to_owned(),
        ]
    );
    let ui = render(&app);
    let fold = said(&ui, &format!("tables/history/run/{}", hex(4))).unwrap();
    assert!(
        fold.starts_with("A fold of 3 records by m55, ") && fold.ends_with(", folded"),
        "{fold}"
    );
    assert!(!fold.contains("write"), "never one write: {fold}");
    let line = |ui: &CompiledUi, n: u8| said(ui, &record_key(n)).unwrap();
    assert!(
        line(&ui, 3).starts_with("aterm \u{b7} m55 \u{b7} "),
        "{}",
        line(&ui, 3)
    );
    assert!(
        line(&ui, 3).ends_with(", 1 cell, Machines \u{b7} m9: Location \u{2192} rack c"),
        "{}",
        line(&ui, 3)
    );
    assert!(line(&ui, 2).starts_with("import \u{b7} m73 \u{b7} "));
    assert!(
        line(&ui, 2).ends_with(
            ", 2 cells, Machines \u{b7} 2 rows: Location \u{2192} rack b; Location \u{2192} rack b"
        ),
        "{}",
        line(&ui, 2)
    );
    assert!(
        line(&ui, 1).starts_with("m55 \u{b7} "),
        "no kind word for an explicit write"
    );
    assert!(line(&ui, 1).ends_with(", not counted"), "{}", line(&ui, 1));

    // The fold opens on a press, its records set in under it.
    act(&mut app, &format!("tables/history/run/{}", hex(4)), None);
    let shown = shape(&lines_of(&app));
    assert_eq!(shown[1..5], ["fold of 3 to 4", "6", "5", "4"]);

    // Selected, the record not counted says so, then what it wrote and had not seen.
    let tables = tables_mut(&mut app);
    tables.registry.selected = Some(op(1));
    tables.registry.opened = Some(Ok(Opened {
        cells: vec![OpenedCell {
            collection: "machine".into(),
            pk: MC.into(),
            column: Some("location".into()),
            what: "Location".into(),
            now: "rack a".into(),
            was: Vec::new(),
            unseen: vec![Valued {
                by: "m73".into(),
                words: "rack z".into(),
            }],
            since: Vec::new(),
        }],
    }));
    let lines = lines_of(&app);
    let at = lines
        .iter()
        .position(|line| key_of(line).as_deref() == Some(record_key(1).as_str()))
        .unwrap();
    assert_eq!(
        shape(&lines[at + 1..at + 4]),
        [
            "note Not counted: this record projects nothing",
            "wrote was",
            "wrote unseen"
        ]
    );
    let ui = render(&app);
    assert_eq!(
        said(&ui, &format!("tables/history/wrote/{}/0/unseen", hex(1))).as_deref(),
        Some("Machines \u{b7} m9 \u{b7} Location: had not seen rack z (m73); shows its row")
    );
    assert_eq!(
        said(&ui, &format!("tables/history/wrote/{}/0/was", hex(1))).as_deref(),
        Some("Machines \u{b7} m9 \u{b7} Location: was nothing, now rack a; shows its row")
    );
}

/// Lines are grouped under the day each record claims, in the causal order and never sorted
/// by a claim; a claim more than a day from both its neighbours' is shown as claimed, its date
/// on its line, and starts no day of its own.
/// NEGATIVE CONTROL: with `apart` never true (`… && false`), the record whose clock ran wrong
/// starts a day of its own: `` left: ["day WED 15 NOV 2023", "5", "4", "day THU 5 OCT 2023",
/// "3", "day TUE 14 NOV 2023", …] ``.
#[test]
fn days_group_the_lines_and_a_claim_far_from_its_neighbours_is_shown_as_claimed() {
    let records = vec![
        logged(
            5,
            AuthorKind::Explicit,
            "m55",
            T0 + DAY + 60_000,
            vec![location(MC, "e")],
        ),
        logged(
            4,
            AuthorKind::Explicit,
            "m73",
            T0 + DAY,
            vec![location(MC, "d")],
        ),
        // A clock forty days behind, between two records a day apart.
        logged(
            3,
            AuthorKind::Explicit,
            "m55",
            T0 - 40 * DAY,
            vec![location(MC, "c")],
        ),
        logged(
            2,
            AuthorKind::Explicit,
            "m73",
            T0 + 60_000,
            vec![location(MC, "b")],
        ),
        logged(1, AuthorKind::Explicit, "m55", T0, vec![location(MC, "a")]),
    ];
    let app = listing(records);
    let (later, first) = (
        super::super::day_words(T0 + DAY),
        super::super::day_words(T0),
    );
    assert_eq!(
        shape(&lines_of(&app)),
        [
            format!("day {later}"),
            "5".to_owned(),
            "4".to_owned(),
            "3 apart".to_owned(),
            format!("day {first}"),
            "2".to_owned(),
            "1".to_owned(),
            "note Start of the registry\u{2019}s records".to_owned(),
        ]
    );
    let ui = render(&app);
    let claimed = said(&ui, &record_key(3)).unwrap();
    assert!(
        claimed.contains(&format!(
            "claimed {}",
            super::super::super::stamp_words(T0 - 40 * DAY)
        )),
        "shown as claimed: {claimed}"
    );
}

/// The list is one Tab stop at Registry scope too, and Up and Down move from line to line.
/// NEGATIVE CONTROL: with `registry_node` not listing its line among the list's stops
/// (`stops.push` removed), Down leaves the list for the grid: `` left:
/// Some("tables/cell/mc-0000000000000009/nickname") right: Some("tables/history/record/0404…")
/// ``.
#[test]
fn the_registry_list_is_one_tab_stop_and_the_arrows_move_among_its_lines() {
    let records = (1..=5)
        .rev()
        .map(|n| {
            let by = if n % 2 == 0 { "m55" } else { "m73" };
            logged(
                n,
                AuthorKind::Explicit,
                by,
                T0 + u64::from(n),
                vec![location(MC, "r")],
            )
        })
        .collect();
    let mut app = listing(records);
    let ui = render(&app);
    let in_list: Vec<&str> = ui
        .focus_order
        .iter()
        .map(|stop| stop.as_str())
        .filter(|stop| {
            ["record/", "run/", "wrote/", "more", "asof/"]
                .iter()
                .any(|kind| stop.starts_with(&format!("tables/history/{kind}")))
        })
        .collect();
    assert_eq!(in_list, [record_key(5)], "one stop for the list");
    with_view_mut(&mut app, |view| {
        view.common.last_focus = Some(UiKey::new(record_key(5)));
        view.common.focus_visible = true;
    });
    let focus = |app: &App| {
        view_of(app)
            .common
            .last_focus
            .as_ref()
            .map(|key| key.as_str().to_owned())
    };
    app.dispatch_native_event(WID, AppEvent::StepFocus(1))
        .expect("the page takes the arrow");
    let _ = render(&app);
    assert_eq!(focus(&app), Some(record_key(4)));
    app.dispatch_native_event(WID, AppEvent::StepFocus(1))
        .expect("the page takes the arrow");
    let _ = render(&app);
    assert_eq!(focus(&app), Some(record_key(3)));
}

/// Machine and Find are this reader's filters: a record is kept where its machine row is the
/// one asked and its line's words hold Find's.
/// NEGATIVE CONTROLS: with the machine kept whatever it is (`is_none_or(|_| true)`), `` assertion
/// failed: !filter(Some("mc-000000000000000a"), "").keeps(…) ``; with Find kept whatever it
/// asks (`true ||`), `` assertion failed: !filter(None, "rack d").keeps(…) ``.
#[test]
fn the_machine_and_find_keep_what_a_line_says() {
    let mut record = logged(
        1,
        AuthorKind::Aterm,
        "m55",
        T0,
        vec![location(MC, "rack c")],
    );
    record.machine = Some(MC.into());
    let words = words_of(&record, "Machines \u{b7} m9", "Location \u{2192} rack c");
    let filter = |machine: Option<&str>, find: &str| Filter {
        machine: machine.map(str::to_owned),
        find: find.into(),
        ..Filter::default()
    };
    assert!(filter(Some(MC), "").keeps(&record, &words));
    assert!(!filter(Some("mc-000000000000000a"), "").keeps(&record, &words));
    assert!(filter(None, "rack c").keeps(&record, &words));
    assert!(
        filter(None, "aterm \u{b7} m55").keeps(&record, &words),
        "its kind and writer"
    );
    assert!(!filter(None, "rack d").keeps(&record, &words));
    assert!(!filter(None, "").reads(), "the store's filters alone");
}

/// Three location writes by the first Mac over the row the second wrote: four records.
fn written() -> aterm_db::table::fixture::Lab {
    let lab = two_macs();
    for place in ["rack b", "rack c", "rack d"] {
        Store::at(lab.paths[0].clone())
            .write(vec![vec![put("location", aterm_json::json!(place))]])
            .unwrap();
    }
    lab
}

fn opened_at(lab: &aterm_db::table::fixture::Lab) -> App {
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, "tables/history/show", None);
    act(&mut app, "tables/history/scope/registry", None);
    tables_mut(&mut app).settle_registry();
    app
}

/// What the store's read kept, each record by what it changed.
fn changed(app: &App) -> Vec<String> {
    app.native_runtime
        .tables()
        .unwrap()
        .registry
        .records
        .iter()
        .map(|record| {
            record
                .cells
                .iter()
                .map(|cell| cell.words.clone())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .collect()
}

/// The filters on a scratch registry: a kind keeps only its records (none here are aterm's,
/// which the list says), Find keeps the records whose line holds its words, the table filter
/// lists the tables in the list's place and keeps the one chosen's records, and the machine
/// filter lists the machine rows.
/// NEGATIVE CONTROL: with `Filter::keeps` ignoring Find (`true ||` before its test), Find keeps
/// every record: `` left: ["Location → rack d", "Location → rack c", "Location → rack b", …]
/// right: ["Location → rack c"] ``.
#[test]
fn the_registry_filters_keep_a_table_a_kind_a_machine_and_words() {
    let lab = written();
    let mut app = opened_at(&lab);
    assert_eq!(changed(&app).len(), 4);

    act(&mut app, "tables/history/kind/aterm", None);
    tables_mut(&mut app).settle_registry();
    assert!(changed(&app).is_empty());
    assert_eq!(shape(&lines_of(&app)), ["note No record the filters keep"]);
    act(&mut app, "tables/history/kind/explicit", None);
    tables_mut(&mut app).settle_registry();
    assert_eq!(changed(&app).len(), 4, "every write here is explicit");
    act(&mut app, "tables/history/kind/any", None);
    tables_mut(&mut app).settle_registry();

    act(
        &mut app,
        FIND_RECORDS,
        Some(SemanticInput::Text("Rack C".into())),
    );
    tables_mut(&mut app).settle_registry();
    assert_eq!(changed(&app), ["Location \u{2192} rack c"]);
    act(
        &mut app,
        FIND_RECORDS,
        Some(SemanticInput::Text(String::new())),
    );
    tables_mut(&mut app).settle_registry();
    assert_eq!(changed(&app).len(), 4);

    // The table filter's choices stand in the list's place: every table first, chosen.
    act(&mut app, "tables/history/filter/table", None);
    let choices = lines_of(&app);
    let words: Vec<(&str, bool)> = choices
        .iter()
        .filter_map(|line| match line {
            Line::Choice { words, chosen, .. } => Some((words.as_str(), *chosen)),
            _ => None,
        })
        .collect();
    assert_eq!(words[0], ("All tables", true));
    let at = |name: &str| words.iter().position(|(words, _)| *words == name).unwrap();
    let other = words
        .iter()
        .position(|(words, _)| *words != "All tables" && *words != "Machines")
        .unwrap();
    act(
        &mut app,
        &format!("tables/history/choose/table/{other}"),
        None,
    );
    tables_mut(&mut app).settle_registry();
    assert!(
        changed(&app).is_empty(),
        "no record wrote {}",
        words[other].0
    );
    act(&mut app, "tables/history/filter/table", None);
    act(
        &mut app,
        &format!("tables/history/choose/table/{}", at("Machines")),
        None,
    );
    tables_mut(&mut app).settle_registry();
    assert_eq!(changed(&app).len(), 4);
    assert_eq!(
        said(&render(&app), "tables/history/filter/table").as_deref(),
        Some("Table: Machines, the table filter")
    );

    // The machine filter offers the machine rows the page read.
    act(&mut app, "tables/history/filter/machine", None);
    let machines: Vec<String> = lines_of(&app)
        .iter()
        .filter_map(|line| match line {
            Line::Choice { words, .. } => Some(words.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(machines, ["All machines", "m9"]);
}

/// A re-read that brings a new record adds it above and keeps the selection, and the selected
/// record's line where it stood on the screen.
/// NEGATIVE CONTROL: with the drawer following the page's reads rather than History's
/// (`app.reads` for `app.registry.reads` in `drawer`), History's page, drawn after the page's
/// read, pushes the selected line down one line: `` the ringed line stays in place:
/// LogicalRect { …, y: 722.0, … } then LogicalRect { …, y: 748.0, … } ``.
#[test]
fn a_re_read_keeps_the_selection_and_the_ringed_line_in_place() {
    let lab = two_macs();
    for n in 0..12 {
        Store::at(lab.paths[0].clone())
            .write(vec![vec![put(
                "location",
                aterm_json::json!(format!("rack {n}")),
            )]])
            .unwrap();
    }
    let mut app = opened_at(&lab);
    // The twelve records fold; opened, the list is longer than the drawer's room.
    let run = lines_of(&app).iter().find_map(|line| match line {
        Line::Run { .. } => key_of(line),
        _ => None,
    });
    act(&mut app, &run.expect("a fold"), None);
    let selected = app.native_runtime.tables().unwrap().registry.records[4].operation;
    let key = format!(
        "tables/history/record/{}",
        aterm_codec::hex::encode_lower(&selected)
    );
    act(&mut app, &key, None);
    tables_mut(&mut app).settle_registry();
    let ui = render(&app);
    assert!(view_of(&app).history.limit.get() > 0.0, "the list scrolls");
    let before = super::super::super::native_tables_tests::rect(&ui, &key);

    Store::at(lab.paths[0].clone())
        .write(vec![vec![put("location", aterm_json::json!("rack new"))]])
        .unwrap();
    // The page's read answers first and is drawn; History's page comes after, in a frame of its
    // own, as it does on a person's screen.
    let tables = tables_mut(&mut app);
    tables.work.send(super::super::super::Work::Read).unwrap();
    tables.settle();
    let _ = render(&app);
    tables_mut(&mut app).settle_registry();
    let ui = render(&app);
    let tables = app.native_runtime.tables().unwrap();
    assert_eq!(
        tables.registry.records.len(),
        14,
        "the new record is listed"
    );
    assert_eq!(
        tables.registry.records[0].cells[0].words, "Location \u{2192} rack new",
        "above"
    );
    assert_eq!(
        tables.registry.selected,
        Some(selected),
        "the selection stays"
    );
    let after = super::super::super::native_tables_tests::rect(&ui, &key);
    assert!(
        (after.y - before.y).abs() < 0.01,
        "the ringed line stays in place: {before:?} then {after:?}"
    );
}

/// A bounded read under a filter that kept none of the newest records it looked at, with older
/// records left, says so and offers Read older records: the reader can go on looking. The head
/// says none is kept yet, never "Newest 0 records".
/// NEGATIVE CONTROL: with the foot drawn only under records (`if registry.more()` back to
/// `if !registry.records.is_empty()` around it), `` left: ["note No record the filters keep
/// among the newest read"] right: [… , "more"] ``.
#[test]
fn a_filter_that_kept_none_of_the_newest_still_reads_older_records() {
    let mut app = listing(Vec::new());
    let tables = tables_mut(&mut app);
    tables.registry.filter.find = "rack z".into();
    tables.registry.next = Some((T0, 0, op(9)));
    assert_eq!(
        shape(&lines_of(&app)),
        [
            "note No record the filters keep among the newest read",
            "more"
        ]
    );
    let tables = app.native_runtime.tables().unwrap();
    assert_eq!(count_words(&tables.registry), "No record kept yet");
    let ui = render(&app);
    assert_eq!(
        said(&ui, "tables/history/more").as_deref(),
        Some("Read older records")
    );
}

/// Records a quiet registry wrote days apart, in the causal order, each start their day: none is
/// shown as claimed, and one machine's records on three days are three lines, no fold across
/// days. A clock that ran ahead of the records caused after it is still shown as claimed.
/// NEGATIVE CONTROL: with `apart` judging the gap alone (`far` of each neighbour, the order
/// not asked), every record is apart and one machine's fold runs across the days: `` left:
/// ["fold of 3 to 1", "note Start …"] right: ["day FRI 24 NOV 2023", "3", "day SUN 19 NOV
/// 2023", "2", …] ``.
#[test]
fn a_quiet_registrys_days_are_days_and_only_a_clock_against_the_order_is_apart() {
    let quiet: Vec<Logged> = [(3, 10), (2, 5), (1, 0)]
        .into_iter()
        .rev()
        .map(|(n, days)| {
            logged(
                n,
                AuthorKind::Explicit,
                "m55",
                T0 + days * DAY,
                vec![location(MC, "r")],
            )
        })
        .rev()
        .collect();
    let app = listing(quiet);
    let day = |days: u64| format!("day {}", super::super::day_words(T0 + days * DAY));
    assert_eq!(
        shape(&lines_of(&app)),
        [
            day(10),
            "3".to_owned(),
            day(5),
            "2".to_owned(),
            day(0),
            "1".to_owned(),
            "note Start of the registry\u{2019}s records".to_owned(),
        ]
    );

    // A clock forty days ahead, caused after a record of the day before and before one of the
    // day after.
    let ahead = vec![
        logged(
            3,
            AuthorKind::Explicit,
            "m55",
            T0 + 2 * DAY,
            vec![location(MC, "c")],
        ),
        logged(
            2,
            AuthorKind::Explicit,
            "m73",
            T0 + 40 * DAY,
            vec![location(MC, "b")],
        ),
        logged(1, AuthorKind::Explicit, "m55", T0, vec![location(MC, "a")]),
    ];
    let app = listing(ahead);
    assert_eq!(
        shape(&lines_of(&app)),
        [
            day(2),
            "3".to_owned(),
            "2 apart".to_owned(),
            day(0),
            "1".to_owned(),
            "note Start of the registry\u{2019}s records".to_owned(),
        ]
    );
}

/// A key typed in Find leaves the lines read in place until the read it asks answers: the list
/// never empties for a key, and the answer then keeps what Find keeps.
/// NEGATIVE CONTROL: with `registry_filter` emptying the list again (`records.clear()`), ``
/// left: 0 right: 4 `` (the lines read, while Find's read is out).
#[test]
fn a_key_typed_in_find_leaves_the_lines_until_its_read_answers() {
    let lab = written();
    let mut app = opened_at(&lab);
    act(
        &mut app,
        FIND_RECORDS,
        Some(SemanticInput::Text("rack c".into())),
    );
    let tables = app.native_runtime.tables().unwrap();
    assert!(tables.registry.reading(), "Find's read is out");
    assert_eq!(tables.registry.records.len(), 4);
    assert!(
        !shape(&lines_of(&app))
            .iter()
            .any(|line| line.starts_with("note Reading")),
        "no line says it is reading in place of the list"
    );
    tables_mut(&mut app).settle_registry();
    assert_eq!(changed(&app), ["Location \u{2192} rack c"]);
}

/// The reader reads only the last of History's asks queued behind one another: the page takes
/// only the answer to its last ask. Every other ask is done, in the order asked.
/// NEGATIVE CONTROL: with `next_work` keeping a superseded ask (its `continue` made a `return
/// Some(work)`), `` left: ["history 1", "read", "history 2"] right: ["read", "history 2",
/// "record"] ``.
#[test]
fn the_reader_reads_only_the_last_of_historys_queued_asks() {
    use super::super::super::{Work, next_work};
    let ask = |seq: u64| {
        Work::History(Asked {
            seq,
            filter: Filter::default(),
            names: BTreeMap::new(),
            before: None,
            revision: None,
            through: None,
            limit: PAGE,
        })
    };
    let (tx, rx) = std::sync::mpsc::channel();
    for work in [ask(1), Work::Read, ask(2), Work::Record(op(1))] {
        tx.send(work).unwrap();
    }
    let mut queued = std::collections::VecDeque::new();
    let done: Vec<String> = std::iter::from_fn(|| {
        next_work(&rx, &mut queued, Some(std::time::Duration::from_millis(1)))
    })
    .take(3)
    .map(|work| match work {
        Work::History(asked) => format!("history {}", asked.seq),
        Work::Read => "read".to_owned(),
        Work::Record(_) => "record".to_owned(),
        _ => "other".to_owned(),
    })
    .collect();
    assert_eq!(done, ["read", "history 2", "record"]);
}

/// A line of what a selected record wrote to a table the rail does not list (a log, a carried
/// face) says so in its spoken name, and does not promise its row.
/// NEGATIVE CONTROL: with every such line taken as one the page shows (`shows` always true), ``
/// left: Some("Archived · x · Whole row: was nothing, now a row of 2 fields; shows its row") ``.
#[test]
fn a_line_of_a_table_the_page_does_not_show_does_not_promise_its_row() {
    let gone = LoggedCell {
        collection: "archived".into(),
        pk: "x".into(),
        column: None,
        words: "whole row".into(),
    };
    let mut app = listing(vec![logged(1, AuthorKind::Aterm, "m55", T0, vec![gone])]);
    let tables = tables_mut(&mut app);
    tables.registry.selected = Some(op(1));
    tables.registry.opened = Some(Ok(Opened {
        cells: vec![OpenedCell {
            collection: "archived".into(),
            pk: "x".into(),
            column: None,
            what: "Whole row".into(),
            now: "a row of 2 fields".into(),
            was: Vec::new(),
            unseen: Vec::new(),
            since: Vec::new(),
        }],
    }));
    let ui = render(&app);
    assert_eq!(
        said(&ui, &format!("tables/history/wrote/{}/0/was", hex(1))).as_deref(),
        Some(
            "Archived \u{b7} x \u{b7} Whole row: was nothing, now a row of 2 fields; a table the \
             page does not show"
        )
    );
}

/// An older page holds only records before the oldest listed: a record it holds that is listed
/// already, or newer, is not listed twice.
/// NEGATIVE CONTROL: with every record of the page put under the list (the filter on the oldest
/// listed taken out), `` left: [3, 2, 2, 1] right: [3, 2, 1] ``.
#[test]
fn an_older_page_lists_no_record_twice() {
    let mut registry = Registry::default();
    let record = |n: u8| logged(n, AuthorKind::Explicit, "m55", T0, Vec::new());
    registry.records = vec![record(3), record(2)];
    registry.asking = Some((7, Asking::Older));
    let took = registry.take_page(
        7,
        Ok(Paged {
            records: vec![record(2), record(1)],
            next: None,
            revision: [0; 32],
            moved: false,
            tables: Vec::new(),
        }),
    );
    assert_eq!(took, Took::Taken);
    let listed: Vec<u8> = registry.records.iter().map(|r| r.operation[0]).collect();
    assert_eq!(listed, [3, 2, 1]);
}

/// The table filter offers every table History can list records of, as its last page names
/// them ([`Paged::tables`]): the rail's, then those the rail does not list (a carried face),
/// whose records History lists too, and their lines say the table by that title.
/// NEGATIVE CONTROL: with `registry_tables` offering the rail's alone (its `registry.tables`
/// branch taken out), `` left: ["All tables", "Machines"] right: ["All tables", "Machines",
/// "Archived rows"] ``.
#[test]
fn the_table_filter_offers_every_table_history_lists() {
    let gone = LoggedCell {
        collection: "archived".into(),
        pk: "x".into(),
        column: None,
        words: "whole row".into(),
    };
    let mut app = listing(vec![logged(1, AuthorKind::Aterm, "m55", T0, vec![gone])]);
    tables_mut(&mut app).registry.tables = vec![
        ("machine".into(), "Machines".into()),
        ("archived".into(), "Archived rows".into()),
    ];
    let ui = render(&app);
    assert!(
        said(&ui, &record_key(1))
            .is_some_and(|line| line.ends_with(", Archived rows \u{b7} x: whole row")),
        "{:?}",
        said(&ui, &record_key(1))
    );
    act(&mut app, "tables/history/filter/table", None);
    let offered: Vec<String> = lines_of(&app)
        .iter()
        .filter_map(|line| match line {
            Line::Choice { words, .. } => Some(words.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(offered, ["All tables", "Machines", "Archived rows"]);
}

/// Write `place` as the first Mac's location: one more record.
fn write_location(lab: &aterm_db::table::fixture::Lab, place: &str) {
    Store::at(lab.paths[0].clone())
        .write(vec![vec![put("location", aterm_json::json!(place))]])
        .unwrap();
}

/// Let the page's reader read the registry again and take its answer, as a person's page does
/// when it moves: History reads again where it shows.
fn read_again(app: &mut App) {
    let tables = tables_mut(app);
    tables.work.send(super::super::super::Work::Read).unwrap();
    tables.settle();
}

/// The page opened at Registry scope, read in pages of two, then Read older records: every
/// record of [`written`] listed.
fn paged_in_twos(lab: &aterm_db::table::fixture::Lab) -> App {
    let mut app = App::headless_for_test();
    app.open_tables_tab_at(Some(lab.paths[0].clone())).unwrap();
    settle(&mut app);
    act(&mut app, "tables/sheet/machine", None);
    act(&mut app, "tables/history/show", None);
    tables_mut(&mut app).registry.page = 2;
    act(&mut app, "tables/history/scope/registry", None);
    tables_mut(&mut app).settle_registry();
    app
}

/// A re-read after Read older records keeps every line that showed, however far back they went:
/// it reads from the top through the oldest record listed, and new records come in above.
/// NEGATIVE CONTROL: with `registry_top` asking through no record (`through` `None`), the re-read
/// lists as many as were listed, the oldest gone: `` left: 4 right: 5 ``.
#[test]
fn a_re_read_after_read_older_keeps_every_line_that_showed() {
    let lab = written();
    let mut app = paged_in_twos(&lab);
    act(&mut app, "tables/history/more", None);
    tables_mut(&mut app).settle_registry();
    let before: Vec<[u8; 32]> = app
        .native_runtime
        .tables()
        .unwrap()
        .registry
        .records
        .iter()
        .map(|r| r.operation)
        .collect();
    assert_eq!(before.len(), 4);
    write_location(&lab, "rack new");
    read_again(&mut app);
    tables_mut(&mut app).settle_registry();
    let after: Vec<[u8; 32]> = app
        .native_runtime
        .tables()
        .unwrap()
        .registry
        .records
        .iter()
        .map(|r| r.operation)
        .collect();
    assert_eq!(after.len(), 5);
    assert_eq!(
        after[1..],
        before[..],
        "every line that showed, the new one above"
    );
}

/// An older page that finds the registry moved since the list was read is not put under the
/// list, nor in its place: the list is read again through its oldest record, then the older
/// page. Read older records pressed while a read is out waits for it.
/// NEGATIVE CONTROLS: with `take_page` taking a moved older page in the list's place (its
/// `Ok(page) if asking == Asking::Older && page.moved` arm never taken), `` the list read again,
/// then the page under it left: 2 right: 5 ``; with `registry_older` asking while a read is out
/// (its `reading()` wait never taken), `` the re-read stays the read out left: Some(Older)
/// right: Some(Top) ``.
#[test]
fn read_older_on_a_moved_registry_keeps_the_list_and_waits_for_a_read_out() {
    let lab = written();
    let mut app = paged_in_twos(&lab);
    // A record lands that the page has not read yet: the older page is asked at a revision the
    // registry has left.
    write_location(&lab, "rack new");
    act(&mut app, "tables/history/more", None);
    tables_mut(&mut app).settle_registry();
    let listed = |app: &App| app.native_runtime.tables().unwrap().registry.records.len();
    assert_eq!(
        listed(&app),
        5,
        "the list read again, then the page under it"
    );

    // A press while a re-read is out waits for it, and is answered after it.
    let lab = written();
    let mut app = paged_in_twos(&lab);
    tables_mut(&mut app).registry_top();
    act(&mut app, "tables/history/more", None);
    assert_eq!(
        app.native_runtime
            .tables()
            .unwrap()
            .registry
            .asking
            .map(|(_, asking)| asking),
        Some(Asking::Top),
        "the re-read stays the read out"
    );
    tables_mut(&mut app).settle_registry();
    assert_eq!(listed(&app), 4);
}

/// A read of the page that moved nothing History lists (the page read as of a record, a masked
/// number read again: the registry at History's revision) reads nothing again; one that finds
/// the registry moved while History is hidden reads it again once History shows, not before.
/// NEGATIVE CONTROLS: with `registry_moved` judging no revision (`revision.is_some_and(…)` made
/// `false`), `` nothing moved: nothing read again ``; with it reading while History is hidden
/// (its `watching` test removed), `` hidden: nothing read yet ``.
#[test]
fn history_reads_again_only_where_the_registry_moved_and_once_it_shows() {
    let lab = written();
    let mut app = opened_at(&lab);
    let tables = tables_mut(&mut app);
    let (snapshot, revision) = (tables.snapshot.clone().unwrap(), tables.registry.revision);
    tables.take(super::super::super::Reply::Read {
        read: Ok(snapshot),
        moved: true,
        revision,
    });
    assert!(
        !tables.registry.reading() && tables.registry.opening == 0,
        "nothing moved: nothing read again"
    );

    act(&mut app, "tables/history/hide", None);
    write_location(&lab, "rack new");
    read_again(&mut app);
    assert!(
        !app.native_runtime.tables().unwrap().registry.reading(),
        "hidden: nothing read yet"
    );
    act(&mut app, "tables/history/show", None);
    assert!(
        app.native_runtime.tables().unwrap().registry.reading(),
        "shown: read again"
    );
    tables_mut(&mut app).settle_registry();
    assert_eq!(changed(&app)[0], "Location \u{2192} rack new");
}

/// A read that failed is read again when History is shown again, not only once a filter or the
/// registry moves.
/// NEGATIVE CONTROL: with History read only where it never was or moved (`!registry.read` and
/// `registry.stale` alone), `` assertion failed:
/// app.native_runtime.tables().unwrap().registry.reading() ``.
#[test]
fn a_failed_read_is_read_again_when_history_shows_again() {
    let lab = written();
    let mut app = opened_at(&lab);
    let tables = tables_mut(&mut app);
    tables.registry.records.clear();
    tables.registry.why = Some("the store was locked".into());
    act(&mut app, "tables/history/scope/row", None);
    act(&mut app, "tables/history/scope/registry", None);
    assert!(app.native_runtime.tables().unwrap().registry.reading());
    tables_mut(&mut app).settle_registry();
    assert_eq!(changed(&app).len(), 4);
}

/// A record a person selected stays in sight when Read older records forms a fold around it:
/// the fold opens, as one they opened by it.
/// NEGATIVE CONTROL: with a selection not marking its record opened (`runs.insert` removed),
/// the fold closes over it: `` the three records of one machine fold, open: [Day(…), Run { …,
/// records: 3, …, open: false, … }, …] ``.
#[test]
fn read_older_does_not_fold_the_selected_record_out_of_sight() {
    let lab = written();
    let mut app = paged_in_twos(&lab);
    // The two newest, one machine's: no fold yet. The newest is selected.
    let newest = app.native_runtime.tables().unwrap().registry.records[0].operation;
    let key = format!(
        "tables/history/record/{}",
        aterm_codec::hex::encode_lower(&newest)
    );
    act(&mut app, &key, None);
    tables_mut(&mut app).settle_registry();
    act(&mut app, "tables/history/more", None);
    tables_mut(&mut app).settle_registry();
    let lines = lines_of(&app);
    assert!(
        matches!(
            lines.get(1),
            Some(Line::Run {
                records: 3,
                open: true,
                ..
            })
        ),
        "the three records of one machine fold, open: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| key_of(line).as_deref() == Some(key.as_str())),
        "the selected line shows"
    );
}

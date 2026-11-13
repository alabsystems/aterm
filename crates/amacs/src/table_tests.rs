// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{ALL_CRLF, MAIN, MIXED, STRAY, TSV};
use super::*;

/// One fixture's expected reading: each record's cells as raw text, and its terminator.
type Expected<'a> = &'a [(&'a [&'a str], &'a str)];

/// Whether `scan` reads `text` as `expected`, or what it read instead.
fn check_scan(
    scan: impl Fn(&str, Delimiter) -> Result<Table, TooLarge>,
    text: &str,
    delimiter: Delimiter,
    expected: Expected<'_>,
) -> Result<(), String> {
    let table = scan(text, delimiter).map_err(|error| format!("{error:?}"))?;
    let read = table
        .records
        .iter()
        .map(|record| {
            (
                record
                    .cells
                    .iter()
                    .map(|cell| &text[cell.raw.clone()])
                    .collect::<Vec<_>>(),
                &text[record.terminator.clone()],
            )
        })
        .collect::<Vec<_>>();
    let wanted = expected
        .iter()
        .map(|(cells, terminator)| (cells.to_vec(), *terminator))
        .collect::<Vec<_>>();
    if read != wanted {
        return Err(format!("read {read:#?}"));
    }
    for record in &table.records {
        let mut at = record.raw.start;
        for (index, cell) in record.cells.iter().enumerate() {
            if cell.raw.start != at {
                return Err(format!("cell starts at {} not {at}", cell.raw.start));
            }
            at = cell
                .raw
                .end
                .saturating_add(usize::from(index + 1 < record.cells.len()));
        }
        if record.cells.last().map(|cell| cell.raw.end) != Some(record.raw.end) {
            return Err("the last cell does not end its record".to_string());
        }
    }
    Ok(())
}

const MAIN_EXPECTED: Expected<'static> = &[
    (
        &[
            "computer",
            "AI login email",
            "services",
            "codes arrive in (my guess)",
            "why I guessed",
            "your correction (leave blank if my guess is right)",
        ],
        "\n",
    ),
    (
        &[
            "m1",
            "alice@example.test",
            "Claude",
            "email",
            "\"The sign-in page said \"\"check your inbox\"\", so email.\"",
            "",
        ],
        "\n",
    ),
    (
        &[
            "m2",
            "bob@example.test",
            "\"Claude, GitHub\"",
            "SMS",
            "\"Two reasons: first\nthe phone prompt, then the code\"",
            "",
        ],
        "\n",
    ),
    (
        &["m3", "carol@example.test", "Claude", "authenticator"],
        "\n",
    ),
    (&["m4", "dave@example.test", "", "", "", "", "extra"], "\n"),
];

const MIXED_EXPECTED: Expected<'static> = &[
    (&["computer", "email", "why", "fix"], "\r\n"),
    (&["m1", "erin@example.test", "\"one\nand two\"", ""], "\r\n"),
    (
        &["m2", "frank@example.test", "\"Said \"\"no\"\"\"", "later"],
        "",
    ),
];

const ALL_CRLF_EXPECTED: Expected<'static> = &[
    (&["computer", "email", "why", "fix"], "\r\n"),
    (
        &["m1", "gina@example.test", "\"line one\r\nline two\"", ""],
        "\r\n",
    ),
    (&["m2", "hal@example.test", "plain", ""], "\r\n"),
];

const STRAY_EXPECTED: Expected<'static> = &[
    (&["a", "b\"c", "d"], "\n"),
    (&["\"x\"y", "z"], "\n"),
    (&["\"never closes,\nstill open"], ""),
];

const TSV_EXPECTED: Expected<'static> = &[
    (
        &[
            "computer",
            "AI login email",
            "services",
            "why I guessed",
            "your correction",
        ],
        "\n",
    ),
    (
        &[
            "m1",
            "alice@example.test",
            "Claude, GitHub",
            "said \"inbox\", so email",
            "",
        ],
        "\n",
    ),
    (
        &["m2", "bob@example.test", "Claude", "\"Two\tparts\"", ""],
        "\n",
    ),
];

fn fixtures() -> [(&'static str, Delimiter, Expected<'static>); 5] {
    [
        (MAIN, Delimiter::Comma, MAIN_EXPECTED),
        (MIXED, Delimiter::Comma, MIXED_EXPECTED),
        (ALL_CRLF, Delimiter::Comma, ALL_CRLF_EXPECTED),
        (STRAY, Delimiter::Comma, STRAY_EXPECTED),
        (TSV, Delimiter::Tab, TSV_EXPECTED),
    ]
}

/// A scanner that ends a record at a line break inside quotes: the NEGATIVE CONTROL of T1.
fn scan_breaking_inside_quotes(text: &str, delimiter: Delimiter) -> Result<Table, TooLarge> {
    let mut records = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let mut cells = Vec::new();
        let mut at = start;
        for part in body.split(delimiter.char()) {
            cells.push(Cell {
                raw: at..at + part.len(),
                content: at..at + part.len(),
                after: at + part.len()..at + part.len(),
                quoted: part.starts_with('"'),
            });
            at += part.len() + 1;
        }
        records.push(Record {
            raw: start..start + body.len(),
            cells,
            terminator: start + body.len()..start + line.len(),
        });
        start += line.len();
    }
    Ok(Table {
        delimiter,
        records,
        problems: Vec::new(),
        columns: 0,
        widths: OnceLock::new(),
    })
}

/// T1: every fixture cell's raw bytes, every record's terminator, and every value.
#[test]
fn t1_scan_reads_every_fixture_cell() {
    for (text, delimiter, expected) in fixtures() {
        check_scan(Table::scan, text, delimiter, expected)
            .unwrap_or_else(|error| panic!("{text:?}: {error}"));
    }
    let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
    assert_eq!(table.columns(), 7);
    let value =
        |record: usize, column: usize| table.records[record].cells[column].value(MAIN).into_owned();
    assert_eq!(
        value(1, 4),
        "The sign-in page said \"check your inbox\", so email."
    );
    assert_eq!(value(2, 2), "Claude, GitHub");
    assert_eq!(
        value(2, 4),
        "Two reasons: first\nthe phone prompt, then the code"
    );
    assert_eq!(value(1, 5), "");
    let stray = Table::scan(STRAY, Delimiter::Comma).unwrap();
    assert_eq!(stray.records[0].cells[1].value(STRAY), "b\"c");
    assert_eq!(stray.records[1].cells[0].value(STRAY), "xy");
    assert_eq!(
        stray
            .problems
            .iter()
            .map(|problem| problem.kind)
            .collect::<Vec<_>>(),
        [ProblemKind::TextAfterQuote, ProblemKind::UnclosedQuote]
    );
    assert_eq!(stray.first_problem_line(STRAY), Some(2));
    let tsv = Table::scan(TSV, Delimiter::Tab).unwrap();
    assert_eq!(tsv.records[2].cells[3].value(TSV), "Two\tparts");
    let empty = Table::scan("", Delimiter::Comma).unwrap();
    assert_eq!(empty.records.len(), 1);
    assert_eq!(empty.records[0].cells.len(), 1);
    let blank_line = Table::scan("a\n\nb\n", Delimiter::Comma).unwrap();
    assert_eq!(
        blank_line.records.len(),
        3,
        "a blank line is a record of one empty cell"
    );

    // NEGATIVE CONTROL: a scanner that ends a record at a newline inside quotes fails.
    assert!(
        check_scan(
            scan_breaking_inside_quotes,
            MAIN,
            Delimiter::Comma,
            MAIN_EXPECTED
        )
        .is_err()
    );
    assert!(
        check_scan(
            scan_breaking_inside_quotes,
            MIXED,
            Delimiter::Comma,
            MIXED_EXPECTED
        )
        .is_err()
    );
}

#[test]
fn the_caps_open_a_table_as_text() {
    for delimiter in [Delimiter::Comma, Delimiter::Tab] {
        let separator = delimiter.char().to_string();
        for final_cell in ["x", "\"x\"", "\""] {
            let at_limit = format!("{}{final_cell}", separator.repeat(MAX_TABLE_COLUMNS - 1));
            let table = Table::scan(&at_limit, delimiter).unwrap();
            assert_eq!(table.columns(), MAX_TABLE_COLUMNS);
            let last = table.records[0].cells.last().unwrap();
            assert_eq!(&at_limit[last.raw.clone()], final_cell);
            assert_eq!(
                last.value(&at_limit),
                if final_cell == "\"" { "" } else { "x" }
            );
            assert_eq!(
                table.problems.len(),
                usize::from(final_cell == "\""),
                "an unfinished quote stays tolerated at the column limit"
            );

            let wide = format!("{}{final_cell}", separator.repeat(MAX_TABLE_COLUMNS));
            assert_eq!(
                Table::scan(&wide, delimiter).map(|table| table.columns()),
                Err(TooLarge::Columns),
                "{delimiter:?}, final cell {final_cell:?}"
            );
            if final_cell == "\"" {
                let mut store = crate::store::DocumentStore::for_test();
                let document = store.open("file:///synthetic/columns.csv".into(), wide.clone());
                let before = store.snapshot(document).unwrap();
                let mut workspace = crate::editor::EditorWorkspace::new();
                let mut view = workspace
                    .attach(&mut store, document, crate::store::DocumentViewId(1))
                    .unwrap();
                assert_eq!(
                    workspace.set_face(&store, &mut view, Some(delimiter)),
                    Err(TooLarge::Columns.reason())
                );
                assert!(view.grid().is_none());
                let after = store.snapshot(document).unwrap();
                assert_eq!(after.seq, before.seq);
                assert_eq!(after.text.as_ref(), wide.as_str());
            }
        }
    }
    let tall = "x\n".repeat(MAX_TABLE_RECORDS + 1);
    assert_eq!(
        Table::scan(&tall, Delimiter::Comma).unwrap_err(),
        TooLarge::Records
    );
    let long = "x".repeat(MAX_TABLE_BYTES + 1);
    assert!(matches!(
        Table::scan(&long, Delimiter::Comma),
        Err(TooLarge::Bytes(_))
    ));
    assert!(TooLarge::Bytes(5 << 20).reason().contains("5.0 MiB"));
    assert!(Table::scan(&"x\n".repeat(MAX_TABLE_RECORDS), Delimiter::Comma).is_ok());
}

#[test]
fn record_and_cell_at_find_the_caret() {
    let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
    for (r, record) in table.records.iter().enumerate() {
        for (c, cell) in record.cells.iter().enumerate() {
            for at in [cell.raw.start, cell.content.start, cell.raw.end] {
                assert_eq!(table.record_at(at), r, "{at}");
                if at != cell.raw.end || c + 1 == record.cells.len() || cell.raw.is_empty() {
                    assert_eq!(table.cell_at(r, at), c, "{r}:{c} at {at}");
                }
            }
            assert_eq!(table.record_at(table.caret_for(r, c)), r);
            assert_eq!(table.cell_at(r, table.caret_for(r, c)), c);
        }
        assert_eq!(table.record_at(record.terminator.start), r);
    }
    assert_eq!(table.record_at(MAIN.len()), table.records.len() - 1);
    assert_eq!(table.caret_for(3, 5), table.records[3].raw.end);
}

/// The encoder that always quotes, as jq's `@csv` does: the NEGATIVE CONTROL of T2.
fn encode_always_quoting(value: &str, _: Delimiter, _: bool) -> Cow<'_, str> {
    Cow::Owned(format!("\"{}\"", value.replace('"', "\"\"")))
}

/// Whether re-encoding every regular cell's own value with `encoder` writes its bytes back.
fn check_requote(encoder: impl Fn(&str, Delimiter, bool) -> Cow<'_, str>) -> Result<(), String> {
    for (text, delimiter, _) in fixtures() {
        let table = Table::scan(text, delimiter).unwrap();
        for record in &table.records {
            for cell in &record.cells {
                let raw = &text[cell.raw.clone()];
                let irregular = !cell.after.is_empty()
                    || (!cell.quoted && raw.contains('"'))
                    || table.problems.iter().any(|problem| cell.holds(problem.at));
                if irregular {
                    continue;
                }
                let value = cell.value(text);
                let written = encoder(&value, delimiter, cell.quoted);
                if written != raw {
                    return Err(format!("{raw:?} re-encodes as {written:?}"));
                }
            }
        }
    }
    Ok(())
}

/// T2: committing a cell's own value is no edit, and a regular cell's value re-encodes to
/// its own bytes.
#[test]
fn t2_a_cells_own_value_is_no_edit() {
    for (text, delimiter, _) in fixtures() {
        let table = Table::scan(text, delimiter).unwrap();
        for (r, record) in table.records.iter().enumerate() {
            for (c, cell) in record.cells.iter().enumerate() {
                assert_eq!(
                    edit_cell(&table, text, r, c, &cell.value(text)),
                    Ok(None),
                    "{text:?} {r}:{c}"
                );
            }
            for c in record.cells.len()..table.columns() {
                assert_eq!(
                    edit_cell(&table, text, r, c, ""),
                    Ok(None),
                    "{r}:{c} missing"
                );
            }
        }
    }
    check_requote(encode).unwrap();
    // NEGATIVE CONTROL: an always-quote encoder rewrites bare cells.
    assert!(check_requote(encode_always_quoting).is_err());
}

/// The values T3 puts in every cell.
const TRICKY: &[&str] = &[
    "a,b",
    "say \"hi\"",
    "two\nlines",
    "",
    " leading space",
    "中文字",
    "👩\u{200d}💻 ok",
    "tab\there",
    "plain",
];

/// A commit that rewrites the whole record in canonical quoting: the NEGATIVE CONTROL of T3.
fn edit_rewriting_the_record(
    table: &Table,
    text: &str,
    record: usize,
    column: usize,
    value: &str,
) -> Result<Option<TextEdit>, Unwritable> {
    let Some(row) = table.records.get(record) else {
        return Ok(None);
    };
    let mut values = row
        .cells
        .iter()
        .map(|cell| cell.value(text).into_owned())
        .collect::<Vec<_>>();
    if values.len() <= column {
        values.resize(column + 1, String::new());
    }
    if values[column] == value {
        return Ok(None);
    }
    values[column] = value.to_string();
    let insert = values
        .iter()
        .map(|value| encode(value, table.delimiter, false).into_owned())
        .collect::<Vec<_>>()
        .join(&table.delimiter.char().to_string());
    Ok(Some(TextEdit {
        range: row.raw.clone(),
        insert,
    }))
}

type CellEditor = fn(&Table, &str, usize, usize, &str) -> Result<Option<TextEdit>, Unwritable>;

/// Whether `editor` puts every tricky value in every cell (and one past a short row's end)
/// so a rescan reads it back, every other cell keeps its value, and every byte outside the
/// cell is unchanged.
fn check_locality(editor: CellEditor) -> Result<(), String> {
    for (text, delimiter, _) in fixtures() {
        let table = Table::scan(text, delimiter).unwrap();
        for (r, record) in table.records.iter().enumerate() {
            for c in 0..table.columns().max(record.cells.len()) {
                if table.unclosed(r) && c >= record.cells.len() {
                    // The open quote would swallow a cell added after it.
                    if editor(&table, text, r, c, "x") != Err(Unwritable::UnclosedRow) {
                        return Err(format!("{r}:{c} was added inside an open quote"));
                    }
                    continue;
                }
                if table.cell_unclosed(r, c) {
                    // The cell runs to the end of the text: writing it would drop the rest.
                    if editor(&table, text, r, c, "x") != Err(Unwritable::UnclosedCell) {
                        return Err(format!("{r}:{c} rewrote a cell that never closes"));
                    }
                    continue;
                }
                for value in TRICKY {
                    let Ok(edit) = editor(&table, text, r, c, value) else {
                        return Err(format!("{r}:{c} {value:?} was refused"));
                    };
                    let Some(edit) = edit else {
                        let current = table
                            .cell(r, c)
                            .map_or(String::new(), |cell| cell.value(text).into_owned());
                        if current != *value {
                            return Err(format!("{r}:{c} {value:?} made no edit"));
                        }
                        continue;
                    };
                    let own = table
                        .cell(r, c)
                        .map_or(record.raw.end..record.raw.end, |cell| cell.raw.clone());
                    if edit.range != own {
                        return Err(format!(
                            "{text:?} {r}:{c} {value:?}: edit covers {:?}, the cell is {own:?}",
                            edit.range
                        ));
                    }
                    let mut after = text.to_string();
                    after.replace_range(edit.range.clone(), &edit.insert);
                    if after[..edit.range.start] != text[..edit.range.start]
                        || after[edit.range.start + edit.insert.len()..] != text[edit.range.end..]
                    {
                        return Err(format!("{r}:{c} {value:?} moved bytes outside the cell"));
                    }
                    let rescan = Table::scan(&after, delimiter).unwrap();
                    let read = rescan
                        .cell(r, c)
                        .map(|cell| cell.value(&after).into_owned());
                    if read.as_deref() != Some(*value) {
                        return Err(format!("{r}:{c} {value:?} reads back as {read:?}"));
                    }
                    for (rr, other) in table.records.iter().enumerate() {
                        for (cc, cell) in other.cells.iter().enumerate() {
                            if (rr, cc) == (r, c) {
                                continue;
                            }
                            let kept = rescan.cell(rr, cc).map(|cell| cell.value(&after));
                            if kept.as_deref() != Some(&*cell.value(text)) {
                                return Err(format!(
                                    "{r}:{c} {value:?} changed {rr}:{cc} to {kept:?}"
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// T3: a cell edit changes that cell's bytes and nothing else.
#[test]
fn t3_a_commit_touches_only_its_cell() {
    check_locality(edit_cell).unwrap();
    // NEGATIVE CONTROL: rewriting the whole record in canonical quoting moves bytes.
    assert!(check_locality(edit_rewriting_the_record).is_err());
}

/// A commit to a missing cell that pads every row out to the header's width: the NEGATIVE
/// CONTROL of T4.
fn edit_padding_every_row(
    table: &Table,
    text: &str,
    record: usize,
    column: usize,
    value: &str,
) -> Result<Option<TextEdit>, Unwritable> {
    let Some(mut edit) = edit_cell(table, text, record, column, value)? else {
        return Ok(None);
    };
    if table.cell(record, column).is_some() {
        return Ok(Some(edit));
    }
    let header = table.records[0].cells.len();
    let mut padded = String::new();
    for (r, row) in table.records.iter().enumerate() {
        padded.push_str(&text[row.raw.clone()]);
        if r == record {
            padded.push_str(&edit.insert);
        } else {
            for _ in row.cells.len()..header {
                padded.push(table.delimiter.char());
            }
        }
        padded.push_str(&text[row.terminator.clone()]);
    }
    edit.range = 0..text.len();
    edit.insert = padded;
    Ok(Some(edit))
}

/// Whether `editor` fills m3's missing last cell, and a short row's missing cell, by padding
/// only that row.
fn check_ragged(editor: CellEditor) -> Result<(), String> {
    const RAGGED: &str = "a,b,c\n1\n2,3\n4,5,6\n";
    let table = Table::scan(RAGGED, Delimiter::Comma).unwrap();
    let edit = editor(&table, RAGGED, 1, 2, "x")
        .ok()
        .flatten()
        .ok_or("no edit")?;
    let mut after = RAGGED.to_string();
    after.replace_range(edit.range.clone(), &edit.insert);
    if after != "a,b,c\n1,,x\n2,3\n4,5,6\n" {
        return Err(format!("wrote {after:?}"));
    }
    let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
    let edit = editor(&table, MAIN, 3, 5, "fix, \"quoted\"")
        .ok()
        .flatten()
        .ok_or("no edit")?;
    let mut after = MAIN.to_string();
    after.replace_range(edit.range.clone(), &edit.insert);
    let expected = MAIN.replace(
        "m3,carol@example.test,Claude,authenticator\n",
        "m3,carol@example.test,Claude,authenticator,,\"fix, \"\"quoted\"\"\"\n",
    );
    if after != expected {
        return Err(format!("wrote {after:?}"));
    }
    if editor(&table, MAIN, 3, 4, "") != Ok(None) {
        return Err("an empty value in a missing cell made an edit".to_string());
    }
    Ok(())
}

/// T4: a missing cell pads only its own row, and an empty value there is no edit.
#[test]
fn t4_a_missing_cell_pads_only_its_row() {
    check_ragged(edit_cell).unwrap();
    // NEGATIVE CONTROL: padding every row to the header's width fails.
    assert!(check_ragged(edit_padding_every_row).is_err());
}

/// The edit `edit_cell` makes, applied to `text`.
fn applied(text: &str, record: usize, column: usize, value: &str) -> String {
    let table = Table::scan(text, Delimiter::Comma).unwrap();
    let mut after = text.to_string();
    if let Some(edit) = edit_cell(&table, text, record, column, value).unwrap() {
        after.replace_range(edit.range, &edit.insert);
    }
    after
}

/// A cell whose quote never closes holds every later row: it is not written, so no clear,
/// paste or commit on it can drop those rows (the review saw one Backspace empty the file).
#[test]
fn a_cell_that_never_closes_is_not_written() {
    const UNCLOSED: &str = concat!(
        "computer,email,why,fix\n",
        "m1,alice@example.test,\"5 inch screen, so SMS,\n",
        "m2,bob@example.test,plain,\n",
        "m3,carol@example.test,plain,\n",
    );
    let table = Table::scan(UNCLOSED, Delimiter::Comma).unwrap();
    assert_eq!(table.records.len(), 2, "the open quote swallows m2 and m3");
    assert!(table.cell_unclosed(1, 2));
    assert!(!table.cell_unclosed(1, 1));
    for value in ["", "x", "5 inch screen, so SMS"] {
        assert_eq!(
            edit_cell(&table, UNCLOSED, 1, 2, value),
            Err(Unwritable::UnclosedCell),
            "{value:?}"
        );
    }
    let own = table.cell(1, 2).unwrap().value(UNCLOSED).into_owned();
    assert_eq!(edit_cell(&table, UNCLOSED, 1, 2, &own), Ok(None));
    // The cells before it are still written in place.
    assert_eq!(
        applied(UNCLOSED, 1, 1, "erin@example.test"),
        UNCLOSED.replace("alice@", "erin@")
    );
}

/// Clearing a record's only cell writes `""`: a bare empty value would turn the row into a
/// blank line that CSV readers skip, or, as the last record with no final newline, into
/// the file's final line break (the row gone on the rescan).
#[test]
fn clearing_a_records_only_cell_keeps_the_row() {
    for (text, cleared) in [
        ("name\nalice\nbob", "name\nalice\n\"\""),
        ("name\nalice\nbob\n", "name\nalice\n\"\"\n"),
        ("name\nalice\nbob\n", "name\n\"\"\nbob\n"),
    ] {
        let record = if cleared.starts_with("name\n\"") {
            1
        } else {
            2
        };
        let after = applied(text, record, 0, "");
        assert_eq!(after, cleared, "{text:?}");
        let rescan = Table::scan(&after, Delimiter::Comma).unwrap();
        assert_eq!(rescan.records.len(), 3, "{after:?}");
        assert_eq!(rescan.cell(record, 0).unwrap().value(&after), "");
    }
    // A record of more cells keeps an empty value bare.
    assert_eq!(applied("a,b\n1,2\n", 1, 1, ""), "a,b\n1,\n");
    let tsv = "name\nbob";
    let table = Table::scan(tsv, Delimiter::Tab).unwrap();
    let edit = edit_cell(&table, tsv, 1, 0, "").unwrap().unwrap();
    assert_eq!(edit.insert, "\"\"");
}

#[test]
fn encode_quotes_only_what_needs_it() {
    let comma = Delimiter::Comma;
    assert_eq!(encode("plain", comma, false), "plain");
    assert_eq!(encode("", comma, false), "");
    assert_eq!(encode("", comma, true), "\"\"");
    assert_eq!(encode("plain", comma, true), "\"plain\"");
    assert_eq!(encode("a,b", comma, false), "\"a,b\"");
    assert_eq!(encode("say \"hi\"", comma, false), "\"say \"\"hi\"\"\"");
    assert_eq!(encode("a\nb", comma, false), "\"a\nb\"");
    assert_eq!(encode("a\rb", comma, false), "\"a\rb\"");
    assert_eq!(encode("a,b", Delimiter::Tab, false), "a,b");
    assert_eq!(encode("a\tb", Delimiter::Tab, false), "\"a\tb\"");
    assert_eq!(encode(" lead", comma, false), " lead");
}

#[test]
fn widths_clip_and_wrap_lay_out_cells() {
    let table = Table::scan(MAIN, Delimiter::Comma).unwrap();
    let widths = table.column_widths(MAIN);
    assert_eq!(widths.len(), 7);
    assert_eq!(widths[0], "computer".len());
    assert_eq!(widths[5], MAX_COLUMN_WIDTH, "the long header caps");
    assert_eq!(widths[6], "extra".len());
    assert!(
        widths
            .iter()
            .all(|w| (MIN_COLUMN_WIDTH..=MAX_COLUMN_WIDTH).contains(w))
    );

    assert_eq!(clip("Claude", 8), "Claude");
    assert_eq!(clip("Claude, GitHub", 8), "Claude,…");
    assert_eq!(clip("first\nsecond", 8), "first⏎");
    assert_eq!(clip("first line\nsecond", 6), "first…");
    assert_eq!(clip("中文字中文字", 5), "中文…");
    assert_eq!(clip("👩\u{200d}💻👩\u{200d}💻", 3), "👩\u{200d}💻…");
    assert_eq!(clip("a\tb", 5), "a b");
    assert_eq!(clip("anything", 0), "");
    assert_eq!(clip("ab", 1), "…");

    let value = "Two reasons: first\nthe phone prompt, then the code";
    let lines = wrap(value, 12);
    let shown = lines
        .iter()
        .map(|line| &value[line.clone()])
        .collect::<Vec<_>>();
    assert_eq!(
        shown,
        [
            "Two reasons: ",
            "first",
            "the phone ",
            "prompt, then ",
            "the code"
        ]
    );
    assert!(shown.iter().all(|line| text_width(line.trim_end()) <= 12));
    assert_eq!(wrap("", 10), vec![0..0]);
    assert_eq!(wrap("a\n", 10), [0..1, 2..2]);
    assert_eq!(wrap("a\r\nb", 10), [0..1, 3..4]);
    assert_eq!(wrap("abcdef", 2), [0..2, 2..4, 4..6]);
    assert_eq!(wrapped_caret(value, &lines, 0), (0, 0));
    assert_eq!(
        wrapped_caret(value, &lines, 13),
        (1, 0),
        "a soft break's end is the next line"
    );
    assert_eq!(
        wrapped_caret(value, &lines, 18),
        (1, 5),
        "the newline ends its line"
    );
    assert_eq!(wrapped_caret(value, &lines, value.len()), (4, 8));
}

/// A row inserter under test: the shipped [`insert_row`] or a rig.
type RowInserter = fn(&Table, &str, usize) -> Result<Option<TextEdit>, Unwritable>;

/// A row deleter under test: the shipped [`delete_row`] or a rig.
type RowDeleter = fn(&Table, &str, usize) -> Result<Option<TextEdit>, Unwritable>;

/// The inserter that ends every new row with `\n`, whatever the file's own breaks: the
/// NEGATIVE CONTROL of the row tests, which the mixed fixture's CRLF records catch.
fn insert_row_always_lf(
    table: &Table,
    _: &str,
    record: usize,
) -> Result<Option<TextEdit>, Unwritable> {
    let row = &table.records[record];
    let empty = table
        .delimiter
        .char()
        .to_string()
        .repeat(table.records[0].cells.len() - 1);
    Ok(Some(if row.terminator.is_empty() {
        TextEdit {
            range: row.raw.end..row.raw.end,
            insert: format!("\n{empty}"),
        }
    } else {
        TextEdit {
            range: row.terminator.end..row.terminator.end,
            insert: format!("{empty}\n"),
        }
    }))
}

/// The deleter that takes a row and its own break, even a last row with none: the NEGATIVE
/// CONTROL of the delete test, which leaves a file that had no final newline with one.
fn delete_row_and_its_break(
    table: &Table,
    _: &str,
    record: usize,
) -> Result<Option<TextEdit>, Unwritable> {
    let row = &table.records[record];
    Ok(Some(TextEdit {
        range: row.raw.start..row.terminator.end,
        insert: String::new(),
    }))
}

/// Each fixture's cells as values, and each record's line break, for comparing a rescan.
fn read_back(text: &str, table: &Table) -> Vec<(Vec<String>, String)> {
    table
        .records
        .iter()
        .map(|record| {
            (
                record
                    .cells
                    .iter()
                    .map(|cell| cell.value(text).into_owned())
                    .collect(),
                text[record.terminator.clone()].to_string(),
            )
        })
        .collect()
}

/// The fixtures a row operation runs on: every one the scan reads without a problem (the
/// CRLF, mixed and no-final-newline ones among them).
fn row_fixtures() -> Vec<(&'static str, Delimiter)> {
    fixtures()
        .into_iter()
        .filter(|(text, delimiter, _)| Table::scan(text, *delimiter).unwrap().problems.is_empty())
        .map(|(text, delimiter, _)| (text, delimiter))
        .collect()
}

/// Whether `insert` adds, after every record of every fixture, one row of the header's number
/// of empty cells ending with the line break of the row above, as one insertion at the
/// boundary that moves no other byte; after a last row with no break, the break before it is
/// the file's own and the text still ends without one.
fn check_insert_rows(insert: RowInserter) -> Result<(), String> {
    for (text, delimiter) in row_fixtures() {
        let table = Table::scan(text, delimiter).unwrap();
        let before = read_back(text, &table);
        let header = table.records[0].cells.len();
        for record in 0..table.records.len() {
            let edit = insert(&table, text, record)
                .map_err(|why| format!("{why:?}"))?
                .ok_or_else(|| format!("no row after {record}"))?;
            let row = &table.records[record];
            let boundary = if row.terminator.is_empty() {
                row.raw.end
            } else {
                row.terminator.end
            };
            if edit.range != (boundary..boundary) {
                return Err(format!("{text:?} row {record}: edit {:?}", edit.range));
            }
            let mut after = text.to_string();
            after.replace_range(edit.range.clone(), &edit.insert);
            let rescan = Table::scan(&after, delimiter).unwrap();
            let mut now = read_back(&after, &rescan);
            if now.len() != before.len() + 1 {
                return Err(format!("{after:?}: {} records", now.len()));
            }
            let (cells, terminator) = now.remove(record + 1);
            if cells.len() != header || cells.iter().any(|cell| !cell.is_empty()) {
                return Err(format!("{after:?}: the new row reads {cells:?}"));
            }
            let above = before[record].1.as_str();
            if terminator != above {
                return Err(format!(
                    "{text:?} row {record}: the new row ends {terminator:?}, the row above {above:?}"
                ));
            }
            let mut expected = before.clone();
            if above.is_empty() {
                let earlier = before[..record]
                    .iter()
                    .rev()
                    .map(|(_, terminator)| terminator.as_str())
                    .find(|terminator| !terminator.is_empty())
                    .unwrap_or("\n");
                expected[record].1 = earlier.to_string();
            }
            if now != expected {
                return Err(format!("{after:?}: the other rows read {now:?}"));
            }
        }
    }
    Ok(())
}

/// Tables with a blank line before a last row that has no line break, LF and CRLF: deleting
/// that row keeps the blank line, which needs its break.
const BLANK_BEFORE_LAST: [&str; 2] = ["a,b\n1,2\n\n3,4", "a,b\r\n1,2\r\n\r\n3,4"];

/// Whether `delete` takes each record of every fixture and nothing else: every other row
/// reads as before, with its own line break, the bytes it removes are the row's own (and, for
/// a last row with no line break, the break before it), and the text ends with a line break
/// exactly when it did, unless the row before a last row with none is a blank line (which
/// ends the text only with its break).
fn check_delete_rows(delete: RowDeleter) -> Result<(), String> {
    let blank_before_last = BLANK_BEFORE_LAST.map(|text| (text, Delimiter::Comma));
    for (text, delimiter) in row_fixtures().into_iter().chain(blank_before_last) {
        let table = Table::scan(text, delimiter).unwrap();
        let before = read_back(text, &table);
        for record in 0..table.records.len() {
            let edit = delete(&table, text, record)
                .map_err(|why| format!("{why:?}"))?
                .ok_or_else(|| format!("nothing deleted at {record}"))?;
            let row = &table.records[record];
            let floor = record
                .checked_sub(1)
                .map_or(row.raw.start, |at| table.records[at].terminator.start);
            if !edit.insert.is_empty()
                || edit.range.start < floor
                || edit.range.end > row.terminator.end
                || edit.range.start > row.raw.start
                || edit.range.end < row.raw.end
            {
                return Err(format!("{text:?} row {record}: edit {edit:?}"));
            }
            let mut after = text.to_string();
            after.replace_range(edit.range.clone(), "");
            let blank_above = row.terminator.is_empty()
                && record
                    .checked_sub(1)
                    .is_some_and(|at| table.records[at].raw.is_empty());
            if !blank_above && after.ends_with(['\r', '\n']) != text.ends_with(['\r', '\n']) {
                return Err(format!(
                    "{text:?} row {record}: the final break changed: {after:?}"
                ));
            }
            let rescan = Table::scan(&after, delimiter).unwrap();
            let now = read_back(&after, &rescan);
            let mut expected = before.clone();
            expected.remove(record);
            // A last row with no break takes the one above it, unless that row is blank.
            if let Some(last) = expected.last_mut()
                && row.terminator.is_empty()
                && !blank_above
            {
                last.1.clear();
            }
            if now != expected {
                return Err(format!("{after:?}: the rows read {now:?}"));
            }
        }
    }
    Ok(())
}

/// G2 rows: an added row has the header's number of empty cells and the line break of the
/// row above (a break before it when the file had no final newline), and a deleted row takes
/// only its own bytes, on the CRLF, mixed and no-final-newline fixtures. *Controls:* a rig
/// that always appends `\n` fails the mixed fixture, and one that leaves a final newline
/// fails the no-final-newline fixture.
#[test]
fn rows_are_added_and_deleted_in_their_own_bytes() {
    check_insert_rows(insert_row).unwrap();
    check_delete_rows(delete_row).unwrap();
    let control = check_insert_rows(insert_row_always_lf).unwrap_err();
    assert!(
        control.contains("\\r\\n"),
        "the mixed fixture's CRLF catches it: {control}"
    );
    let control = check_delete_rows(delete_row_and_its_break).unwrap_err();
    assert!(control.contains("final break"), "{control}");

    // The mixed file's last row has no line break: a row added after it brings the file's
    // CRLF before it, and the file still ends without one.
    let table = Table::scan(MIXED, Delimiter::Comma).unwrap();
    let edit = insert_row(&table, MIXED, 2).unwrap().unwrap();
    assert_eq!(edit.insert, "\r\n,,,");
    let edit = insert_row(&table, MIXED, 1).unwrap().unwrap();
    assert_eq!(edit.insert, ",,,\r\n");
    // A one-column table's empty row is `""`, not a blank line readers skip.
    let one = "name\nm1\n";
    let table = Table::scan(one, Delimiter::Comma).unwrap();
    assert_eq!(
        insert_row(&table, one, 0).unwrap().unwrap().insert,
        "\"\"\n"
    );
    // A row whose quote never closes holds every row after it: no row is added after it or
    // deleted with it, and the rows before it still are.
    let table = Table::scan(STRAY, Delimiter::Comma).unwrap();
    let last = table.records.len() - 1;
    assert_eq!(
        insert_row(&table, STRAY, last),
        Err(Unwritable::UnclosedRow)
    );
    assert_eq!(
        delete_row(&table, STRAY, last),
        Err(Unwritable::UnclosedRow)
    );
    assert!(insert_row(&table, STRAY, 0).unwrap().is_some());
    assert!(delete_row(&table, STRAY, 0).unwrap().is_some());
    assert_eq!(insert_row(&table, STRAY, last + 1), Ok(None));
    assert_eq!(delete_row(&table, STRAY, last + 1), Ok(None));
}

/// G2 review: a last row with no line break after a blank line goes alone, the blank line
/// and its break staying (LF and CRLF). *Control:* the deleter before this fix took the blank
/// line's break, so one delete took two rows; `check_delete_rows` with it fails on
/// [`BLANK_BEFORE_LAST`].
#[test]
fn deleting_a_last_row_keeps_the_blank_line_above_it() {
    check_delete_rows(delete_row).unwrap();
    for text in BLANK_BEFORE_LAST {
        let table = Table::scan(text, Delimiter::Comma).unwrap();
        let edit = delete_row(&table, text, 3).unwrap().unwrap();
        let mut after = text.to_string();
        after.replace_range(edit.range, "");
        let rescan = Table::scan(&after, Delimiter::Comma).unwrap();
        assert_eq!(rescan.records.len(), 3, "{after:?}");
        assert!(rescan.records[2].raw.is_empty(), "{after:?}");
    }
    let control = check_delete_rows(delete_row_taking_the_break_above).unwrap_err();
    assert!(control.contains("the rows read"), "{control}");
}

/// The deleter before the blank-line fix: a last row with no break always takes the break
/// above it. The control of [`deleting_a_last_row_keeps_the_blank_line_above_it`].
fn delete_row_taking_the_break_above(
    table: &Table,
    _: &str,
    record: usize,
) -> Result<Option<TextEdit>, Unwritable> {
    let row = &table.records[record];
    let range = match record.checked_sub(1).map(|at| &table.records[at]) {
        Some(previous) if row.terminator.is_empty() => previous.terminator.start..row.raw.end,
        _ => row.raw.start..row.terminator.end,
    };
    Ok(Some(TextEdit {
        range,
        insert: String::new(),
    }))
}

/// G2 review: deleting a row between a lone `\r` and a blank `\n` line is refused, as the two
/// breaks would read as one `\r\n` and the blank line would go too; the file's other rows
/// still delete. *Control:* deleting the row's own bytes there leaves two rows fewer.
#[test]
fn a_delete_that_would_join_a_cr_and_a_blank_lf_line_is_refused() {
    let text = "a,b\r1,2\n\n3,4\n";
    let table = Table::scan(text, Delimiter::Comma).unwrap();
    assert_eq!(table.records.len(), 4);
    assert_eq!(
        delete_row(&table, text, 1),
        Err(Unwritable::JoinsLineBreaks)
    );
    for record in [0, 2, 3] {
        let edit = delete_row(&table, text, record).unwrap().unwrap();
        let mut after = text.to_string();
        after.replace_range(edit.range, "");
        assert_eq!(
            Table::scan(&after, Delimiter::Comma).unwrap().records.len(),
            3,
            "row {record}: {after:?}"
        );
    }
    let own = table.records[1].raw.start..table.records[1].terminator.end;
    let mut joined = text.to_string();
    joined.replace_range(own, "");
    assert_eq!(joined, "a,b\r\n3,4\n");
    assert_eq!(
        Table::scan(&joined, Delimiter::Comma)
            .unwrap()
            .records
            .len(),
        2,
        "the control: the plain delete takes the blank line too"
    );
}

#[test]
fn the_memo_scans_once_per_revision() {
    let mut store = crate::store::DocumentStore::for_test();
    let document = store.open("mem://t.csv".into(), MAIN.into());
    let snapshot = store.snapshot(document).unwrap();
    let mut memo = TableMemo::default();
    let first = memo
        .table(document, snapshot.seq, Delimiter::Comma, &snapshot.text)
        .unwrap();
    let again = memo
        .table(document, snapshot.seq, Delimiter::Comma, &snapshot.text)
        .unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    let tab = memo
        .table(document, snapshot.seq, Delimiter::Tab, &snapshot.text)
        .unwrap();
    assert!(!Arc::ptr_eq(&first, &tab));
}

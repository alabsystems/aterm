// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Rendered;
use crate::{Column, Name, Row, Snapshot, Source, snapshot::PK};

fn column(name: &str) -> Column {
    Column {
        name: name.to_owned(),
        source: if name == PK {
            Source::Pk
        } else {
            Source::Schema
        },
        read_only: name == PK,
        refers: None,
        ty: None,
        null: None,
    }
}

fn row(pk: &str, title: &str, outcome: &str) -> Row {
    let mut row = Row {
        pk: pk.to_owned(),
        live: true,
        ..Row::default()
    };
    row.cells.insert("title".to_owned(), title.to_owned());
    if !outcome.is_empty() {
        row.cells.insert("outcome".to_owned(), outcome.to_owned());
    }
    row
}

fn snapshot(rows: Vec<Row>) -> Snapshot {
    Snapshot {
        name: Name::parse("lab/packages").unwrap(),
        kind: "table".to_owned(),
        ext: "tsv".to_owned(),
        rev: 3,
        complete: true,
        at_ms: 1,
        columns: ["title", "outcome", PK].into_iter().map(column).collect(),
        handles: Vec::new(),
        rows,
    }
}

/// A PATCHED RENDERING IS THE WHOLE ONE: rows replaced, added and taken out of a rendering give
/// the content, the file and the face one rendered from the patched rows whole gives, also
/// where the change moves which columns the face shows.
///
/// NEGATIVE CONTROL (run once, then restored): with `put` leaving a replaced row's widths on
/// the face's totals, the narrowed rows still count as wide and the face keeps `outcome` out:
/// `` left: ["title", "pk"] right: ["title", "outcome", "pk"] ``.
#[test]
fn a_patched_rendering_renders_as_the_whole_one() {
    let rows: Vec<Row> = (0..40)
        .map(|n| row(&format!("p{n:03}"), &format!("title {n}"), "short"))
        .collect();
    let held = Rendered::of(snapshot(rows.clone()));
    let wide = "w".repeat(usize::try_from(crate::MAX_FACE_BYTES).unwrap() / 2);
    let changed = vec![
        row("p005", "renamed\tquoted", "short"),
        row("p100", "added", &wide),
        row("p000", "first", &wide),
    ];
    let patched = held
        .patch(changed.clone(), &["p007".to_owned()], false, 9)
        .expect("rows in key order");
    let mut whole_rows = rows;
    whole_rows.retain(|row| !["p005", "p000", "p007"].contains(&row.pk.as_str()));
    whole_rows.extend(changed);
    whole_rows.sort_by(|a, b| a.pk.cmp(&b.pk));
    let mut whole = snapshot(whole_rows);
    whole.complete = false;
    whole.at_ms = 9;
    assert_eq!(patched.snapshot(), &whole);
    assert_eq!(patched.content(), whole.content());
    assert_eq!(patched.encode(), whole.encode());
    assert_eq!(patched.face(), whole.face());
    assert_eq!(
        patched.face_names(),
        ["title", PK],
        "two wide outcomes leave the column out"
    );
    // Narrowing the wide rows again brings the column back.
    let narrow = patched
        .patch(
            vec![row("p000", "first", "short"), row("p100", "added", "short")],
            &[],
            false,
            9,
        )
        .unwrap();
    assert_eq!(narrow.face_names(), ["title", "outcome", PK]);
    assert_eq!(narrow.face(), narrow.snapshot().face());
}

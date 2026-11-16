// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Cell, Row, Schema, kept};
use crate::fleet::command::print;
use crate::fleet::fixture;
use crate::fleet::fold::fold;
use crate::fleet::rows::{self, MachineRow};
use crate::typed::{Ty, grid, secret};
use aterm_json::Value;

const NOW: u64 = 1_800_000_000_000;

/// The fixture's own machine (m12) as `aterm db fleet machines` printed it before the relation
/// types left the fleet: each column, its TSV cell and its JSON value. A value prints as it is,
/// an unread cell as `?` (JSON `null`) and a known-empty one as nothing (JSON `null`).
const M12: [(&str, &str, &str); 46] = [
    ("machine", "mc-76c301532f85bec9", r#""mc-76c301532f85bec9""#),
    ("label", "m12", r#""m12""#),
    ("number", "12", "12"),
    ("status", "active", r#""active""#),
    ("os", "macOS 26.4", r#""macOS 26.4""#),
    ("chip", "M3 Ultra", r#""M3 Ultra""#),
    ("ram_gb", "256", "256"),
    (
        "uuid",
        "6212a9f04ec854d1351d3d53156ce79a",
        r#""6212a9f04ec854d1351d3d53156ce79a""#,
    ),
    ("host", "", "null"),
    ("reach", "up", r#""up""#),
    ("reach_why", "", "null"),
    ("reach_at", "1799999980000", "1799999980000"),
    ("how", "self", r#""self""#),
    ("proven", "true", "true"),
    ("first_seen", "?", "null"),
    ("last_seen", "1799999980000", "1799999980000"),
    ("build", "0.141.0", r#""0.141.0""#),
    ("sessions", "3", "3"),
    ("agents", "3", "3"),
    ("busy", "3", "3"),
    ("idle", "0", "0"),
    ("waiting", "0", "0"),
    ("walled", "0", "0"),
    ("stuck", "0", "0"),
    ("unknown", "0", "0"),
    ("agents_listed", "3", "3"),
    ("agents_at", "1799999980000", "1799999980000"),
    ("facts_at", "1799999980000", "1799999980000"),
    ("disk_free_mb", "40000", "40000"),
    ("cpu_used_x100", "?", "null"),
    ("cpu_cores", "?", "null"),
    ("cpu_at", "?", "null"),
    ("mem_used_mb", "?", "null"),
    ("mem_total_mb", "262144", "262144"),
    ("mem_at", "1800000000000", "1800000000000"),
    ("gpu_used_x100", "?", "null"),
    ("gpu_cores", "?", "null"),
    ("gpu_at", "?", "null"),
    ("disk_used_mb", "?", "null"),
    ("disk_total_mb", "?", "null"),
    ("disk_at", "?", "null"),
    ("net_used_kbps", "?", "null"),
    ("net_link_kbps", "?", "null"),
    ("net_at", "?", "null"),
    ("accounts_expected", "2", "2"),
    ("accounts_active", "4", "4"),
];

/// What precedes the rows of the fixture's JSON machines answer.
const JSON_HEAD: &str = r#"{"relation":"machines","sources":{"fleet_read":{"at":1799999980000,"state":"live","why":null},"journal":{"at":1800000000000,"state":"live","why":null},"network":{"at":1800000000000,"state":"live","why":null},"registry":{"at":1800000000000,"state":"live","why":null}},"unresolved_accounts":6,"shadowed":0,"#;

#[test]
fn a_fleet_relation_prints_by_the_shared_schema_types() {
    // The machines relation is a relation of the shared types: its schema a `relation::Schema`,
    // its row a `relation::Row` whose cells are `relation::Cell`s.
    let schema: &'static Schema = <MachineRow as Row>::SCHEMA;
    assert!(std::ptr::eq(schema, &rows::MACHINES));
    assert_eq!(schema.columns.len(), M12.len());

    let tables = fold(&fixture::inputs(NOW), NOW).tables;
    let row = tables
        .machines
        .iter()
        .find(|row| row.label.as_deref() == Some("m12"))
        .expect("the fixture's own machine");
    let cells: Vec<Cell> = row.cells();
    let at = |column: &str| schema.position(column).expect("a machines column");
    assert_eq!(cells[at("machine")], Cell::of("mc-76c301532f85bec9"));
    assert_eq!(cells[at("cpu_used_x100")], Cell::Unread);
    assert_eq!(cells[at("host")], Cell::Empty);

    // It prints exactly as it did: the same columns, cells and JSON, in the same order.
    let only = [grid::Where::parse("how=self").expect("a filter")];
    let tsv = print(&tables, "machines", &only, false).expect("machines print");
    assert!(tsv.read);
    let header: Vec<&str> = M12.iter().map(|(column, _, _)| *column).collect();
    let line: Vec<&str> = M12.iter().map(|(_, shown, _)| *shown).collect();
    assert_eq!(
        tsv.text,
        format!("{}\n{}\n", header.join("\t"), line.join("\t"))
    );

    let json = print(&tables, "machines", &only, true).expect("machines print");
    let members: Vec<String> = M12
        .iter()
        .map(|(column, _, value)| format!("\"{column}\":{value}"))
        .collect();
    assert_eq!(
        json.text,
        format!("{JSON_HEAD}\"rows\":[\n{{{}}}\n]}}\n", members.join(","))
    );
}

#[test]
fn a_cell_reads_the_same_on_every_face() {
    // A login another writer's build stored though it is no address: the text face withholds
    // it, and so does the JSON one. NEGATIVE CONTROL: with `Cell::json` giving a value as it is,
    // the JSON is the stored text and the second assertion fails.
    let stored = Cell::of(concat!("ghp", "_cccccccccccccccccccccccc"));
    assert_eq!(stored.shown(&Ty::Email), grid::REFUSED);
    assert_eq!(
        stored.json(&Ty::Email),
        Value::String(grid::REFUSED.to_owned())
    );

    // A sealed column's value shows as the mask on both faces, and its absence as nothing.
    let sealed = Cell::of("anything");
    assert_eq!(sealed.shown(&Ty::Secret), secret::MASK);
    assert_eq!(
        sealed.json(&Ty::Secret),
        Value::String(secret::MASK.to_owned())
    );
    assert_eq!(Cell::Value(Value::Null).json(&Ty::Secret), Value::Null);

    // A value of its type is itself; an unread cell is `?`, a known-empty one nothing, both null.
    let login = Cell::of("one@lab.example");
    assert_eq!(login.shown(&Ty::Email), "one@lab.example");
    assert_eq!(
        login.json(&Ty::Email),
        Value::String("one@lab.example".to_owned())
    );
    assert_eq!(Cell::Unread.shown(&Ty::Uint), "?");
    assert_eq!(Cell::Unread.json(&Ty::Uint), Value::Null);
    assert_eq!(Cell::Empty.shown(&Ty::Uint), "");
    assert_eq!(Cell::Empty.json(&Ty::Uint), Value::Null);
}

#[test]
fn a_relation_filters_by_its_own_columns() {
    let tables = fold(&fixture::inputs(NOW), NOW).tables;
    let only = |spec: &str| [grid::Where::parse(spec).expect("a filter")];
    let mine = kept(&tables.machines, &only("how=self")).expect("a machines column");
    assert_eq!(mine.len(), 1);
    assert_eq!(
        mine[0][rows::MACHINES.position("label").expect("label")],
        Cell::of("m12")
    );
    let refused = kept(&tables.machines, &only("nowhere=x")).expect_err("no such column");
    assert_eq!(refused.0, "machines: no such column to filter by");
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compiled plan rows: which plan rows a seed pass gives each provider, and which of a
//! plan's cells a seed leaves to the owner.

use super::*;
use crate::{alab::tests::apply, typed::Writer};
use std::collections::BTreeSet;

/// The plan keys of the subscription types the lab buys that this build seeds, and the two
/// keys earlier builds seeded beside them (`anthropic-max`, `openai-prolite`). A key is what an
/// account's `plan` names, so none of these may move.
const BOUGHT: [&str; 13] = [
    "anthropic-pro",
    "anthropic-max-5x",
    "anthropic-max-20x",
    "anthropic-team",
    "anthropic-enterprise",
    "anthropic-max",
    "openai-plus",
    "openai-pro",
    "openai-prolite",
    "google-ai-pro",
    "google-ai-ultra",
    "xai-supergrok",
    "xai-supergrok-heavy",
];

/// An owner's value for `col` of plan `pk`.
fn owner_value(pk: &str, col: &str, x: Value) -> Change {
    cell::change(
        &PLAN,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::owner(PLAN.version, x),
    )
    .unwrap()
}

/// A seed pass over an empty registry leaves every provider its plan rows: one free row, keyed
/// `<provider>-free`, and a row for each subscription type the lab buys, every row keyed by the
/// provider it names. A provider may have its free row alone. A seed writes only the columns
/// the schema gives a seed, in every collection, so a plan's `tier`, `price_cents` and
/// `currency` stay empty: they are the owner's cells, and the owner's values land there as
/// written and call for no further seed.
/// NEGATIVE CONTROLS: with `rows` also seeding each plan's currency (`scalar(p.pk, "currency",
/// json!("USD"))`), the seed writes an owner column: `` a seed wrote the owner's column
/// plan.currency of anthropic-enterprise ``; with the plan collection's `currency` given to
/// the seeds as well (`.writer(Writer::Seed)`), `` anthropic-enterprise.currency is the
/// owner's ``.
#[test]
fn every_provider_has_its_plan_rows_and_a_seed_writes_only_seed_columns() {
    let seeded = rows_for(&Cells::new()).unwrap();
    let cells = apply(&seeded);
    let plans = cell::project(&PLAN, &cells);
    for key in BOUGHT {
        assert!(plans.rows.contains_key(key), "no plan row is keyed {key}");
    }
    let mut free = BTreeSet::new();
    for (pk, row) in &plans.rows {
        let provider = row
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{pk} names no provider"));
        assert!(keys::PROVIDERS.contains(&provider), "{pk} names {provider}");
        assert!(
            pk.strip_prefix(provider)
                .is_some_and(|slug| slug.starts_with('-')),
            "{pk} is not keyed by its provider, {provider}"
        );
        if row.get("period") == Some(&json!("free")) {
            free.insert(pk.clone());
        }
    }
    let free_rows: BTreeSet<String> = keys::PROVIDERS
        .iter()
        .map(|provider| format!("{provider}-free"))
        .collect();
    assert_eq!(free, free_rows, "one free row for each provider");

    // Only seed columns are seeded, in every collection.
    for collection in [&PROVIDER, &CLI, &PLAN] {
        for (pk, row) in &cell::project(collection, &cells).rows {
            for col in row.fields.keys() {
                assert_eq!(
                    collection.column(col).map(|column| column.writer),
                    Some(Writer::Seed),
                    "a seed wrote the owner's column {}.{col} of {pk}",
                    collection.name
                );
            }
        }
    }
    // A plan's tier and price are the owner's cells whatever the schema calls them.
    for (pk, row) in &plans.rows {
        for col in ["tier", "price_cents", "currency"] {
            assert!(row.get(col).is_none(), "{pk}.{col} is the owner's");
        }
    }

    // The owner's tier and price land in those empty cells and read as written.
    let owned = [
        ("tier", json!("20x")),
        ("price_cents", json!(20_000)),
        ("currency", json!("USD")),
    ];
    let mut records = seeded;
    records.push(
        owned
            .iter()
            .map(|(col, x)| owner_value("anthropic-max-20x", col, x.clone()))
            .collect(),
    );
    let cells = apply(&records);
    let row = &cell::project(&PLAN, &cells).rows["anthropic-max-20x"];
    for (col, x) in &owned {
        assert!(
            matches!(row.fields.get(*col), Some(Field::Value(_))),
            "{col} is one value, not a conflict"
        );
        assert_eq!(row.get(col), Some(x));
    }
    assert!(rows_for(&cells).unwrap().is_empty());
}

/// A seed pass over a registry an earlier build seeded adds the rows that build lacked and
/// leaves every cell it wrote: the earlier build's `anthropic-max` is `current` there, this
/// build compiles it `legacy`, and the cell stays `current`, one value and no conflict, for
/// the owner to change.
/// NEGATIVE CONTROL: with `held` answering false for a scalar cell that holds a value, the
/// pass writes its own status over the earlier one: `left: Some(String("legacy"))`.
#[test]
fn a_seed_pass_over_an_earlier_builds_rows_adds_rows_and_rewrites_no_cell() {
    let earlier = vec![vec![
        cell::change(
            &PLAN,
            CellKey {
                pk: "anthropic-max",
                col: "status",
                elem: None,
            },
            &CellValue::automatic(PLAN.version, json!("current")),
        )
        .unwrap(),
    ]];
    let mut records = earlier.clone();
    records.extend(rows_for(&apply(&earlier)).unwrap());
    let cells = apply(&records);
    let plans = cell::project(&PLAN, &cells);
    let row = &plans.rows["anthropic-max"];
    assert!(
        matches!(row.fields.get("status"), Some(Field::Value(_))),
        "the earlier build's status is one value, not a conflict"
    );
    assert_eq!(row.get("status"), Some(&json!("current")));
    // The rest of that row and the rows the earlier build lacked are there.
    assert_eq!(row.get("name"), Some(&json!("Claude Max")));
    assert!(plans.rows.contains_key("anthropic-max-20x"));
    assert!(rows_for(&cells).unwrap().is_empty());
}

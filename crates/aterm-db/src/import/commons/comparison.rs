// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded, counts-only comparison. A matching row is evidence about these two
//! documents, never an alias, replacement, account or login authorization.

use super::*;
use std::collections::BTreeSet;

const SOURCE_LIMIT: usize = 64;
const DETAIL_LIMIT: usize = 64;
const ROW_FIELDS: [&str; 6] = [
    "email",
    "email_column",
    "machine_slot_hint",
    "machine_column",
    "providers",
    "unclassified_columns",
];

pub(super) fn report(current: &Value, snapshot: &Snapshot) -> Result<Value> {
    report_with_limits(current, snapshot, SOURCE_LIMIT, DETAIL_LIMIT)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "ConflictCapped",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "Duplicate",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "Complete",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "SourcesCapped",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "Withheld",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "Opaque",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "Rejected",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "RowsCapped",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportComparison",
        action = "Conflict",
        project = "aterm_db::import::commons::comparison::tests::transition"
    )
)]
fn report_with_limits(
    current: &Value,
    snapshot: &Snapshot,
    source_limit: usize,
    detail_limit: usize,
) -> Result<Value> {
    let current_key = field(current, "source_sha256")?;
    validate(current_key, current)?;
    let history_complete = snapshot.history_complete();
    let source_candidates: usize = snapshot
        .cells
        .values()
        .map(|cell| cell.candidates.len())
        .sum();
    let conflicted_sources = snapshot
        .cells
        .values()
        .filter(|cell| cell.candidates.len() > 1)
        .count();
    let mut comparisons = Vec::new();
    let current_rows = rows_by_record(current);
    for ((collection, key), cell) in &snapshot.cells {
        if collection != "sources" || cell.stopped {
            return Err(Error::new("invalid import comparison source"));
        }
        for candidate in &cell.candidates {
            if comparisons.len() == source_limit {
                break;
            }
            let stored = &candidate.value;
            validate(key, stored)?;
            let op = snapshot
                .operations
                .iter()
                .find(|op| op.header.id == candidate.operation)
                .ok_or_else(|| Error::new("import comparison provenance is unavailable"))?;
            let mut comparison = compare_rows(current, &current_rows, stored, detail_limit);
            let object = comparison.as_object_mut().unwrap();
            object.insert("source_sha256".into(), json!(key));
            object.insert("source_digest_matches".into(), json!(key == current_key));
            object.insert("exact_candidate_match".into(), json!(stored == current));
            object.insert("conflicted".into(), json!(cell.candidates.len() != 1));
            object.insert("stored".into(), summary(stored));
            object.insert(
                "provenance".into(),
                json!({"operation":hex::encode_lower(&candidate.operation),
                    "actor":hex::encode_lower(&op.header.actor),
                    "authored_at_unix_ms":op.authored_ms,"time_source":"signed-author-claim",
                    "received_at_unix_ms":op.receipt.received_ms}),
            );
            comparisons.push(comparison);
        }
    }
    Ok(
        json!({"schema":1,"kind":"account-import-comparison","authority":"none",
        "current":summary(current),"table":hex::encode_lower(&snapshot.scope.digest()),
        "retained":snapshot.retained,"withheld":snapshot.withheld,
        "history_scope":"locally-retained","history_complete":history_complete,"source_candidates":source_candidates,
        "conflicted_sources":conflicted_sources,
        "source_limit":source_limit,"sources_truncated":comparisons.len() < source_candidates,
        "comparisons":comparisons}),
    )
}

fn summary(value: &Value) -> Value {
    let rows = value["rows"].as_array().unwrap();
    let providers = rows
        .iter()
        .flat_map(|row| row["providers"].as_array().unwrap());
    let (mut present, mut absent) = (0, 0);
    for provider in providers {
        if provider["presence_marker"].as_bool() == Some(true) {
            present += 1;
        } else {
            absent += 1;
        }
    }
    json!({"source_sha256":value["source_sha256"],"records":value["records"],"rows":rows.len(),
        "email_values":rows.iter().filter(|row|!row["email"].is_null()).count(),
        "machine_slot_hints":rows.iter().filter(|row|!row["machine_slot_hint"].is_null()).count(),
        "presence_true":present,"presence_false":absent,
        "unclassified_cells":rows.iter().map(|row|row["unclassified_columns"].as_array().unwrap().len()).sum::<usize>()})
}

fn rows_by_record(value: &Value) -> BTreeMap<u64, &Value> {
    value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["record"].as_u64().unwrap(), row))
        .collect()
}

fn compare_rows(
    current: &Value,
    current_rows: &BTreeMap<u64, &Value>,
    stored: &Value,
    detail_limit: usize,
) -> Value {
    let stored_rows = rows_by_record(stored);
    let records: BTreeSet<_> = current_rows.keys().chain(stored_rows.keys()).collect();
    let (mut only_current, mut only_stored, mut identical, mut shared_identities) = (0, 0, 0, 0);
    let mut changed_fields: BTreeMap<_, usize> = ROW_FIELDS.map(|field| (field, 0)).into();
    let mut details = Vec::new();
    let mut changed_rows = 0;
    for record in records {
        let difference = match (current_rows.get(record), stored_rows.get(record)) {
            (Some(current), Some(stored)) => {
                if (!current["email"].is_null() || !current["machine_slot_hint"].is_null())
                    && current["email"] == stored["email"]
                    && current["machine_slot_hint"] == stored["machine_slot_hint"]
                    && current["email_column"] == stored["email_column"]
                    && current["machine_column"] == stored["machine_column"]
                {
                    shared_identities += 1;
                }
                let fields: Vec<_> = ROW_FIELDS
                    .into_iter()
                    .filter(|field| current[*field] != stored[*field])
                    .collect();
                if fields.is_empty() {
                    identical += 1;
                    continue;
                }
                for field in &fields {
                    *changed_fields.get_mut(field).unwrap() += 1;
                }
                json!({"record":record,"changed_fields":fields})
            }
            (Some(_), None) => {
                only_current += 1;
                json!({"record":record,"change":"only-current"})
            }
            (None, Some(_)) => {
                only_stored += 1;
                json!({"record":record,"change":"only-stored"})
            }
            (None, None) => unreachable!(),
        };
        changed_rows += 1;
        if details.len() < detail_limit {
            details.push(difference);
        }
    }
    json!({"records_equal":current["records"] == stored["records"],
        "only_current_rows":only_current,"only_stored_rows":only_stored,
        "identical_rows":identical,"shared_identity_rows":shared_identities,
        "changed_fields":changed_fields,"changed_rows":changed_rows,
        "row_detail_limit":detail_limit,"row_details_truncated":details.len() < changed_rows,
        "row_details":details})
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reference and unique findings (design §6). A table is one causal domain, so no foreign key
//! or unique constraint is atomic against a concurrent writer and nothing cascades: the writer
//! checks against its own view, and every reader re-checks the converged rows and reports
//! what it finds. A finding names collections, keys and columns, never a value, so an address
//! never reaches a report.

use super::{Collection, Ty, cell::Projection};
use std::collections::BTreeMap;

/// Something a reader reports for the owner.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Finding {
    /// A register holds different concurrent values.
    Conflict {
        collection: String,
        pk: String,
        column: String,
    },
    /// A reference names a key its collection does not hold.
    Dangling {
        collection: String,
        pk: String,
        column: String,
        target: String,
    },
    /// Live rows share the values of a unique column set.
    Unique {
        collection: String,
        columns: Vec<String>,
        pks: Vec<String>,
    },
    /// A value this reader cannot interpret: a newer build's column or word.
    Raw {
        collection: String,
        pk: String,
        column: String,
    },
}

fn refs(ty: &Ty, value: &aterm_json::Value, out: &mut Vec<(String, String)>) {
    match ty {
        Ty::Ref(target) => {
            if let Some(pk) = value.as_str() {
                out.push(((*target).to_owned(), pk.to_owned()));
            }
        }
        Ty::List(inner, _) => {
            for item in value.as_array().into_iter().flatten() {
                refs(inner, item, out);
            }
        }
        Ty::Struct(members) => {
            for member in *members {
                if let Some(field) = value.get(member.name) {
                    refs(&member.ty, field, out);
                }
            }
        }
        _ => {}
    }
}

/// Every finding over the projected `tables`, keyed by collection name, against their compiled
/// `schemas`. A reference into a collection that is not among `tables` is not checked.
pub fn findings(schemas: &[&Collection], tables: &BTreeMap<&str, &Projection>) -> Vec<Finding> {
    let mut found = Vec::new();
    for schema in schemas {
        let Some(projection) = tables.get(schema.name) else {
            continue;
        };
        for (pk, row) in &projection.rows {
            for column in &row.raw {
                found.push(Finding::Raw {
                    collection: schema.name.to_owned(),
                    pk: pk.clone(),
                    column: column.clone(),
                });
            }
            for (name, field) in &row.fields {
                let conflicted = match field {
                    super::cell::Field::Set(elements) => elements.values().any(|e| e.conflicted()),
                    other => other.conflicted(),
                };
                if conflicted {
                    found.push(Finding::Conflict {
                        collection: schema.name.to_owned(),
                        pk: pk.clone(),
                        column: name.clone(),
                    });
                }
                let Some(column) = schema.column(name) else {
                    continue;
                };
                let mut targets = Vec::new();
                match (column.ty, field) {
                    (Ty::Set(Ty::Ref(target)), _) => {
                        for elem in field.members() {
                            targets.push(((*target).to_owned(), elem.to_owned()));
                        }
                    }
                    (ty, super::cell::Field::Conflict { candidates, .. }) => {
                        for candidate in candidates {
                            refs(&ty, &candidate.value.x, &mut targets);
                        }
                    }
                    (ty, _) => {
                        if let Some(value) = field.value() {
                            refs(&ty, value, &mut targets);
                        }
                    }
                }
                for (target, key) in targets {
                    if let Some(other) = tables.get(target.as_str())
                        && !other.rows.contains_key(&key)
                    {
                        found.push(Finding::Dangling {
                            collection: schema.name.to_owned(),
                            pk: pk.clone(),
                            column: name.clone(),
                            target,
                        });
                    }
                }
            }
        }
        for unique in schema.unique {
            let mut groups = BTreeMap::<Vec<String>, Vec<String>>::new();
            for (pk, row) in projection.rows.iter().filter(|(_, row)| row.live()) {
                let values: Option<Vec<String>> = unique
                    .iter()
                    .map(|column| {
                        row.fields
                            .get(*column)
                            .filter(|field| !field.conflicted())
                            .and_then(super::cell::Field::value)
                            .filter(|value| !value.is_null())
                            .and_then(|value| aterm_json::to_string(value).ok())
                    })
                    .collect();
                if let Some(values) = values {
                    groups.entry(values).or_default().push(pk.clone());
                }
            }
            for pks in groups.into_values().filter(|pks| pks.len() > 1) {
                found.push(Finding::Unique {
                    collection: schema.name.to_owned(),
                    columns: unique.iter().map(|c| (*c).to_owned()).collect(),
                    pks,
                });
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

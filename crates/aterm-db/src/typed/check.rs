// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reference and unique findings (design §6). A table is one causal domain, so no foreign key
//! or unique constraint is atomic against a concurrent writer and nothing cascades: an owner
//! write is refused when a reference it writes names a row its own view, with the write landed,
//! does not hold ([`missing_row`]), and every reader re-checks the converged rows and reports
//! what it finds ([`findings`]), a reference a concurrent write left dangling among them. A
//! finding or refusal names collections, keys and columns, never a value, so an address never
//! reaches a report.

use super::{
    Collection, Kind, Refused, Table, Ty,
    cell::{CellKey, CellValue, Cells, Projection},
};
use crate::database::Change;
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
    /// A row whose `protected` cell is true holds a `Secret` cell ([`protected`]): a concurrent
    /// write or an older build put it there; readers never reveal it and the owner clears it.
    Protected {
        collection: String,
        pk: String,
        column: String,
    },
}

/// Whether a row is protected (design §4.7, "protection is data, not source"): any candidate
/// of its `protected` cell says `true`, so a concurrent unprotect never uncovers a secret.
pub fn protected(row: &super::cell::Row) -> bool {
    use super::cell::Field;
    match row.fields.get("protected") {
        Some(Field::Value(candidate)) => candidate.value.x == aterm_json::Value::Bool(true),
        Some(Field::Conflict { candidates, .. }) => candidates
            .iter()
            .any(|c| c.value.x == aterm_json::Value::Bool(true)),
        _ => false,
    }
}

fn refs<'a>(ty: &Ty, value: &'a aterm_json::Value, out: &mut Vec<(&'static str, &'a str)>) {
    match ty {
        Ty::Ref(target) => {
            if let Some(pk) = value.as_str() {
                out.push((*target, pk));
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

/// Whether `collection` holds a row keyed `pk` once `rows` land on `cells`: a cell of that row
/// already held ([`super::grid::holds_row`], a retired or ended row included, as [`findings`]
/// reads it) or one `rows` write.
fn holds(collection: &Collection, cells: &Cells, rows: &[Vec<Change>], pk: &str) -> bool {
    super::grid::holds_row(collection, cells, pk)
        || rows.iter().flatten().any(|change| {
            change.collection == collection.name
                && change.value.is_some()
                && CellKey::parse(&change.key).is_ok_and(|key| key.pk == pk)
        })
}

/// The first reference an owner write's `rows` (one record each) writes into `table` that names
/// a key its target cells collection would not hold once they land on `cells`, the table as the
/// writer reads it: a `Ref` cell's value, a `Ref` inside a list or struct, or a `Set(Ref)`
/// element the write adds. A row the same write makes may be named. Only what `rows` write is
/// judged, so a reference already dangling in a column the write leaves alone, which a
/// concurrent write may have made so, never holds the write back; [`findings`] still reports
/// it. A row `seeded` says every build of the table compiles (a seed a pass has not written
/// here yet) may be named too; [`findings`] reports the reference until a pass writes it. A
/// reference into a collection that is not a cells collection of `table` is not judged, as
/// [`findings`] does not judge one.
pub fn missing_row(
    table: &Table,
    rows: &[Vec<Change>],
    cells: &Cells,
    seeded: fn(&str, &str) -> bool,
) -> Option<Refused> {
    for change in rows.iter().flatten() {
        let Some(schema) = table
            .collection(&change.collection)
            .filter(|c| c.kind == Kind::Cells)
        else {
            continue;
        };
        let (Some(value), Ok(key)) = (&change.value, CellKey::parse(&change.key)) else {
            continue;
        };
        let (Some(column), Ok(value)) = (schema.column(key.col), CellValue::decode(value)) else {
            continue;
        };
        let mut targets = Vec::new();
        match (column.ty, key.elem) {
            (Ty::Set(Ty::Ref(target)), Some(elem)) => {
                if value.x.as_bool() == Some(true) {
                    targets.push((*target, elem));
                }
            }
            (_, Some(_)) => {}
            (ty, None) => refs(&ty, &value.x, &mut targets),
        }
        for &(target, pk) in &targets {
            let judged = table.collection(target).filter(|c| c.kind == Kind::Cells);
            if judged
                .is_some_and(|target| !holds(target, cells, rows, pk) && !seeded(target.name, pk))
            {
                return Some(Refused {
                    collection: schema.name,
                    pk: key.pk.to_owned(),
                    column: key.col.to_owned(),
                    rule: format!("no such {target} row"),
                });
            }
        }
    }
    None
}

/// Every finding over the projected `tables`, keyed by collection name, against their compiled
/// `schemas`. A reference into a collection that is not among `tables` is not checked.
pub fn findings(schemas: &[&Collection], tables: &BTreeMap<&str, &Projection>) -> Vec<Finding> {
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum UniqueVal<'a> {
        Str(&'a str),
        Json(String),
    }

    fn unique_val<'a>(row: &'a super::cell::Row, column: &str) -> Option<UniqueVal<'a>> {
        let value = row
            .fields
            .get(column)
            .filter(|field| !field.conflicted())
            .and_then(|_| row.get(column))
            .filter(|value| !value.is_null())?;
        match value {
            aterm_json::Value::String(s) => Some(UniqueVal::Str(s.as_str())),
            other => aterm_json::to_string(other).ok().map(UniqueVal::Json),
        }
    }

    let mut found = Vec::new();
    let mut targets = Vec::new();
    for schema in schemas {
        let Some(projection) = tables.get(schema.name) else {
            continue;
        };
        for (pk, row) in &projection.rows {
            if protected(row) {
                for column in schema.columns.iter().filter(|c| matches!(c.ty, Ty::Secret)) {
                    if row
                        .fields
                        .get(column.name)
                        .is_some_and(|field| match field {
                            super::cell::Field::Value(c) => !c.value.x.is_null(),
                            _ => true,
                        })
                    {
                        found.push(Finding::Protected {
                            collection: schema.name.to_owned(),
                            pk: pk.clone(),
                            column: column.name.to_owned(),
                        });
                    }
                }
            }
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
                if conflicted || row.claims.get(name).is_some_and(Option::is_none) {
                    found.push(Finding::Conflict {
                        collection: schema.name.to_owned(),
                        pk: pk.clone(),
                        column: name.clone(),
                    });
                }
                let Some(column) = schema.column(name) else {
                    continue;
                };
                targets.clear();
                match (column.ty, field) {
                    (Ty::Set(Ty::Ref(target)), super::cell::Field::Set(elements)) => {
                        for (elem, f) in elements {
                            if f.present() {
                                targets.push((*target, elem.as_str()));
                            }
                        }
                    }
                    (Ty::Set(Ty::Ref(_)), _) => {}
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
                for &(target, key) in &targets {
                    if let Some(other) = tables.get(target)
                        && !other.rows.contains_key(key)
                    {
                        found.push(Finding::Dangling {
                            collection: schema.name.to_owned(),
                            pk: pk.clone(),
                            column: name.clone(),
                            target: target.to_owned(),
                        });
                    }
                }
            }
        }
        for unique in schema.unique {
            if let [column] = *unique {
                let mut groups = BTreeMap::<UniqueVal<'_>, Vec<&str>>::new();
                for (pk, row) in projection.rows.iter().filter(|(_, row)| row.live()) {
                    if let Some(val) = unique_val(row, column) {
                        groups.entry(val).or_default().push(pk.as_str());
                    }
                }
                for pks in groups.into_values().filter(|pks| pks.len() > 1) {
                    found.push(Finding::Unique {
                        collection: schema.name.to_owned(),
                        columns: vec![(*column).to_owned()],
                        pks: pks.into_iter().map(str::to_owned).collect(),
                    });
                }
            } else {
                let mut groups = BTreeMap::<Vec<UniqueVal<'_>>, Vec<&str>>::new();
                for (pk, row) in projection.rows.iter().filter(|(_, row)| row.live()) {
                    let values: Option<Vec<UniqueVal<'_>>> = unique
                        .iter()
                        .map(|column| unique_val(row, column))
                        .collect();
                    if let Some(values) = values {
                        groups.entry(values).or_default().push(pk.as_str());
                    }
                }
                for pks in groups.into_values().filter(|pks| pks.len() > 1) {
                    found.push(Finding::Unique {
                        collection: schema.name.to_owned(),
                        columns: unique.iter().map(|c| (*c).to_owned()).collect(),
                        pks: pks.into_iter().map(str::to_owned).collect(),
                    });
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

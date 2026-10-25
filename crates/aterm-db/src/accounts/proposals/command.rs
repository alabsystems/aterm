// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::database::{Cell, decode_hex};
use aterm_codec::hex;
use aterm_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn run(command: &str, options: BTreeMap<&str, &str>) -> Result<Value> {
    run_at(
        command,
        options,
        Paths::live().map_err(crate::table::failure)?,
    )
}

pub(super) fn run_at(
    command: &str,
    mut options: BTreeMap<&str, &str>,
    paths: Paths,
) -> Result<Value> {
    let write = if command == "accounts-write" {
        let request = decode_hex::<16>(take(&mut options, "--request")?)?;
        let changes: Value = aterm_json::from_str(take(&mut options, "--changes")?)
            .map_err(|_| Error::new("invalid proposal changes JSON"))?;
        let payload = aterm_json::to_vec(&json!({"v":1, "kind":"write", "request":hex::encode_lower(&request), "changes":changes}))
            .map_err(|_| Error::new("invalid proposal changes"))?;
        let Command::Write { changes, .. } = decode(&payload)? else {
            unreachable!()
        };
        Some((request, changes))
    } else {
        None
    };
    let revert = if command == "accounts-revert" {
        Some((
            decode_hex(take(&mut options, "--request")?)?,
            decode_hex(take(&mut options, "--operation")?)?,
        ))
    } else {
        None
    };
    let collection = if matches!(command, "accounts-get" | "accounts-scan") {
        Some(take(&mut options, "--collection")?)
    } else {
        None
    };
    let key = if command == "accounts-get" {
        Some(take(&mut options, "--key")?)
    } else {
        None
    };
    if !options.is_empty() {
        return Err(Error::new("unknown account proposal option"));
    }
    let db = Proposals::at(paths);
    if command == "accounts-check" {
        return db.check();
    }
    if command == "accounts-init" {
        let scope = db.initialize()?;
        return Ok(json!({"table":hex::encode_lower(&scope.digest()), "kind":"account-proposals"}));
    }
    if let Some((request, changes)) = write {
        return Ok(receipt_json(db.write(request, changes)?));
    }
    if let Some((request, operation)) = revert {
        return Ok(receipt_json(db.revert(request, operation)?));
    }
    let snapshot = db.snapshot()?;
    let table = hex::encode_lower(&snapshot.scope.digest());
    if command == "accounts-history" {
        return Ok(
            json!({"table":table, "retained":snapshot.retained, "withheld":snapshot.withheld, "operations":snapshot.operations.iter().map(|o| json!({
            "operation":hex::encode_lower(&o.header.id), "actor":hex::encode_lower(&o.header.actor),
            "roster":hex::encode_lower(&o.roster), "epoch":o.epoch,
            "sequence":o.header.sequence, "previous":o.header.previous.map(|id|hex::encode_lower(&id)),
            "parents":o.header.parents.iter().map(|id|hex::encode_lower(id)).collect::<Vec<_>>(),
            "authored_at_unix_ms":o.authored_ms, "time_source":"signed-author-claim",
            "received_at_unix_ms":o.receipt.received_ms,"local_journal_offset":o.receipt.offset,
            "receipt_operation":hex::encode_lower(&o.receipt.id),
            "request":o.request.map(|id|hex::encode_lower(&id)),"reverts":o.reverts.map(|id|hex::encode_lower(&id)),
            "available":o.available,"catalog_eligible":o.catalog_eligible,
            "eligible":o.eligible,"duplicate_of":o.duplicate_of.map(|id|hex::encode_lower(&id)),"refusal":o.refusal
        })).collect::<Vec<_>>()}),
        );
    }
    let collection = collection.ok_or_else(|| Error::new("unknown account proposal command"))?;
    if let Some(key) = key {
        return Ok(json!({"table":table,"collection":collection,"key":key,
            "cell":snapshot.cells.get(&(collection.into(),key.into())).map(cell_json)}));
    }
    Ok(
        json!({"table":table,"collection":collection,"rows":snapshot.cells.iter()
        .filter(|((c,_),_)|c==collection).map(|((_,key),cell)|json!({"key":key,"cell":cell_json(cell)})).collect::<Vec<_>>()}),
    )
}
fn take<'a>(options: &mut BTreeMap<&str, &'a str>, key: &str) -> Result<&'a str> {
    options
        .remove(key)
        .ok_or_else(|| Error::new("missing account proposal option"))
}
fn receipt_json(receipt: Receipt) -> Value {
    json!({"operation":hex::encode_lower(&receipt.id),"received_at_unix_ms":receipt.received_ms,"local_journal_offset":receipt.offset})
}
fn cell_json(cell: &Cell) -> Value {
    json!({"stopped":cell.stopped,"candidates":cell.candidates.iter().map(|c|json!({"operation":hex::encode_lower(&c.operation),"value":c.value})).collect::<Vec<_>>()})
}

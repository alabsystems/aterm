// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inert CSV candidate documents on a separate default Commons table. One
//! bounded source is one atomic operation; there is no partial import state.

use crate::{
    Error, Result,
    catalog::Descriptor,
    database::{Change, Command, decode_hex, field},
    documents::log::{Log, Snapshot},
    table::store::Receipt,
};
use aterm_codec::hex;
use aterm_json::{Value, json};
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::{collections::BTreeMap, path::Path};

fn descriptor(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "accounts", "imports", 1)
}

/// Parsed input remains a proposal. No record from this table can activate a
/// credential, assign a machine name, merge mailboxes or grant spending rights.
pub struct Imports {
    paths: Paths,
}
impl Imports {
    pub fn at(paths: Paths) -> Self {
        Self { paths }
    }
    /// Validate the complete source before any application mutation. Repeating
    /// a request with different bytes is refused by the shared metadata log.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbImportAtomic",
            action = "Reject",
            project = "aterm_db::import::commons::tests::atomic_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbImportAtomic",
            action = "Commit",
            project = "aterm_db::import::commons::tests::atomic_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbImportAtomic",
            action = "Retry",
            project = "aterm_db::import::commons::tests::atomic_transition"
        )
    )]
    pub fn apply(&self, path: &Path, request: [u8; 16]) -> Result<Value> {
        let value = super::candidate_document(path)?;
        let source = field(&value, "source_sha256")?.to_owned();
        let command = Command::Write {
            request,
            changes: vec![Change {
                collection: "sources".into(),
                key: source.clone(),
                value: Some(value.clone()),
            }],
        };
        let payload = command.encode()?;
        if decode(&payload)? != command {
            return Err(Error::new("import encoding would change its meaning"));
        }
        let receipt = crate::documents::application::with_store(
            &self.paths,
            true,
            descriptor,
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                if let Some(id) = log.retry(session.actor(), &command)? {
                    return store
                        .receipt(session, &id)?
                        .ok_or_else(|| Error::new("import receipt is unavailable"));
                }
                store.commit_catalog(session, &payload)
            },
        )?;
        Ok(report(&value, receipt, request))
    }
    pub fn snapshot(&self) -> Result<Snapshot> {
        crate::documents::application::with_store(
            &self.paths,
            false,
            descriptor,
            |session, store, scope| Ok(Log::read_with(session, store, scope, decode)?.snapshot()),
        )
    }
}

fn decode(payload: &[u8]) -> Result<Command> {
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("import payload is not canonical"));
    }
    let Command::Write { changes, .. } = &command else {
        return Err(Error::new(
            "import documents cannot authorize other commands",
        ));
    };
    if changes.len() != 1 || changes[0].collection != "sources" {
        return Err(Error::new(
            "import needs exactly one complete source document",
        ));
    }
    let value = changes[0]
        .value
        .as_ref()
        .ok_or_else(|| Error::new("import source cannot be deleted"))?;
    validate(&changes[0].key, value)?;
    Ok(command)
}

fn keys(value: &Value, names: &[&str]) -> Result<()> {
    if !value.as_object().is_some_and(|object| {
        object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
    }) {
        return Err(Error::new("invalid import document fields"));
    }
    Ok(())
}
fn column(value: &Value) -> Result<u64> {
    value
        .as_u64()
        .filter(|c| (1..=super::MAX_COLUMNS as u64).contains(c))
        .ok_or_else(|| Error::new("invalid import source column"))
}
fn validate(key: &str, value: &Value) -> Result<()> {
    keys(value, &["schema", "source_sha256", "records", "rows"])?;
    if value["schema"].as_u64() != Some(1) || field(value, "source_sha256")? != key {
        return Err(Error::new("invalid import source identity"));
    }
    if hex::encode_lower(&decode_hex::<32>(key)?) != key {
        return Err(Error::new("import source digest is not canonical"));
    }
    let records = value["records"]
        .as_u64()
        .filter(|n| *n <= super::MAX_ROWS as u64)
        .ok_or_else(|| Error::new("invalid import record count"))?;
    let rows = value["rows"]
        .as_array()
        .filter(|rows| rows.len() as u64 <= records)
        .ok_or_else(|| Error::new("invalid import rows"))?;
    let mut previous = 1;
    for row in rows {
        keys(
            row,
            &[
                "record",
                "email",
                "email_column",
                "machine_slot_hint",
                "machine_column",
                "providers",
                "unclassified_columns",
            ],
        )?;
        let record = row["record"]
            .as_u64()
            .filter(|n| *n > previous && *n <= records + 1)
            .ok_or_else(|| Error::new("invalid import row order"))?;
        previous = record;
        column(&row["email_column"])?;
        if !row["email"].is_null() {
            let email = field(row, "email")?;
            crate::contacts::validate("email-addresses", &json!({"address":email}))?;
        }
        if !row["machine_column"].is_null() {
            column(&row["machine_column"])?;
        }
        if !row["machine_slot_hint"].is_null() {
            crate::machines::Slot::parse(field(row, "machine_slot_hint")?)?;
            column(&row["machine_column"])?;
        }
        let providers = row["providers"]
            .as_array()
            .filter(|providers| providers.len() <= super::MAX_COLUMNS)
            .ok_or_else(|| Error::new("invalid import provider hints"))?;
        let mut previous_column = 0;
        for provider in providers {
            keys(provider, &["provider", "column", "presence_marker"])?;
            let at = column(&provider["column"])?;
            if at <= previous_column
                || !super::PROVIDERS
                    .iter()
                    .any(|(_, name)| Some(*name) == provider["provider"].as_str())
                || provider["presence_marker"].as_bool().is_none()
            {
                return Err(Error::new("invalid import provider classification"));
            }
            previous_column = at;
        }
        let unresolved = row["unclassified_columns"]
            .as_array()
            .filter(|columns| columns.len() <= super::MAX_COLUMNS)
            .ok_or_else(|| Error::new("invalid unclassified source coordinates"))?;
        let mut previous_column = 0;
        for coordinate in unresolved {
            let at = column(coordinate)?;
            if at <= previous_column {
                return Err(Error::new("invalid unclassified source order"));
            }
            previous_column = at;
        }
    }
    Ok(())
}

fn report(value: &Value, receipt: Receipt, request: [u8; 16]) -> Value {
    let rows = value["rows"].as_array().unwrap();
    json!({"source_sha256":value["source_sha256"],"request":hex::encode_lower(&request),
        "operation":hex::encode_lower(&receipt.id),"received_at_unix_ms":receipt.received_ms,
        "local_journal_offset":receipt.offset,"source_records":value["records"],"candidate_rows":rows.len(),
        "email_candidates":rows.iter().filter(|row|!row["email"].is_null()).count(),
        "provider_hints":rows.iter().map(|row|row["providers"].as_array().unwrap().len()).sum::<usize>(),
        "unclassified_source_cells":rows.iter().map(|row|row["unclassified_columns"].as_array().unwrap().len()).sum::<usize>(),
        "raw_source_modified":false,"raw_unclassified_values_replicated":false,"credentials_activated":false})
}

pub(crate) fn run(command: &str, mut options: BTreeMap<&str, &str>) -> Result<Value> {
    let db = Imports::at(Paths::live().map_err(crate::table::failure)?);
    if command == "accounts-import-csv" {
        let path = options
            .remove("--file")
            .ok_or_else(|| Error::new("missing import file"))?;
        let request = decode_hex(
            options
                .remove("--request")
                .ok_or_else(|| Error::new("missing import request"))?,
        )?;
        if !options.is_empty() {
            return Err(Error::new("unknown import option"));
        }
        return db.apply(Path::new(path), request);
    }
    if command != "accounts-imports" || !options.is_empty() {
        return Err(Error::new("unknown import command or option"));
    }
    let snapshot = db.snapshot()?;
    Ok(json!({"table":hex::encode_lower(&snapshot.scope.digest()),
        "retained":snapshot.retained,"withheld":snapshot.withheld,
        "sources":snapshot.cells.iter().map(|((_,key),cell)|json!({"source_sha256":key,
            "candidates":cell.candidates.iter().map(|candidate|json!({
                "operation":hex::encode_lower(&candidate.operation),"value":candidate.value
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "operations":snapshot.operations.iter().map(|op|json!({
            "operation":hex::encode_lower(&op.header.id),"actor":hex::encode_lower(&op.header.actor),
            "sequence":op.header.sequence,"parents":op.header.parents.iter().map(|id|hex::encode_lower(id)).collect::<Vec<_>>(),
            "authored_at_unix_ms":op.authored_ms,"time_source":"signed-author-claim",
            "received_at_unix_ms":op.receipt.received_ms,"receipt_operation":hex::encode_lower(&op.receipt.id),
            "eligible":op.eligible,"available":op.available,"refusal":op.refusal
        })).collect::<Vec<_>>()}))
}

#[cfg(test)]
mod tests;

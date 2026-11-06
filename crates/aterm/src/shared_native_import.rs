// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit intake of one inert imported coordinate. The source stays intact;
//! the derived ordinary proposal and its provenance confer no execution rights.

use super::*;
use aterm_db::import::commons::Imports;

pub(super) struct Imported {
    pub key: String,
    pub value: Value,
    pub origin: Value,
}

pub(super) fn resolve(
    paths: &Paths,
    selector: &str,
    deadline: Instant,
) -> Result<Option<Imported>, String> {
    let Some(rest) = selector.strip_prefix("import:") else {
        return Ok(None);
    };
    let mut parts = rest.split(':');
    let (Some(source_str), Some(record_str), Some(column_str), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("import selection needs exact source, record and column".into());
    };
    let source = super::super::digest::<32>(source_str)?;
    if source_str.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err("import source digest is not canonical".into());
    }
    let record: u64 = record_str.parse().map_err(|_| "invalid import record")?;
    let column: u64 = column_str.parse().map_err(|_| "invalid import column")?;
    let canonical_pos = |s: &str| {
        s.as_bytes()
            .first()
            .is_some_and(|b| matches!(b, b'1'..=b'9'))
    };
    if !canonical_pos(record_str) || !canonical_pos(column_str) || record < 2 || column == 0 {
        return Err("import coordinates must be canonical positive integers".into());
    }
    let snapshot = Imports::at_until(paths.clone(), deadline)
        .snapshot()
        .map_err(|e| e.to_string())?;
    if !snapshot.history_complete() {
        return Err("native intake import history is incomplete; wait for sync repair".into());
    }
    let cell = snapshot
        .cells
        .get(&("sources".into(), source_str.into()))
        .filter(|cell| !cell.stopped && cell.candidates.len() == 1)
        .ok_or("import source is absent or conflicted")?;
    let candidate = &cell.candidates[0];
    let row = candidate.value["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["record"].as_u64() == Some(record))
        })
        .ok_or("import record is absent")?;
    let provider = row["providers"]
        .as_array()
        .and_then(|providers| {
            providers
                .iter()
                .find(|provider| provider["column"].as_u64() == Some(column))
        })
        .ok_or("import provider coordinate is absent")?;
    if provider["presence_marker"].as_bool() != Some(true) {
        return Err("import coordinate does not indicate this account".into());
    }
    let provider = provider["provider"]
        .as_str()
        .and_then(Provider::of)
        .ok_or("import provider has no native API-key adapter")?;
    let table = snapshot.scope.digest();
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/native-import-proposal/1\0");
    hash.update(table);
    hash.update(source);
    hash.update(record.to_le_bytes());
    hash.update(column.to_le_bytes());
    let key = aterm_codec::hex::encode_lower(&hash.finalize());
    let table_hex = aterm_codec::hex::encode_lower(&table);
    let op_hex = aterm_codec::hex::encode_lower(&candidate.operation);
    let origin = json!({"selector":selector,"table":table_hex,"operation":op_hex});
    let value = json!({"provider":provider.name(),"method":"api-key","email":row["email"],"source":{
        "import":{"kind":"source-ref","value":format!("{table_hex}:{selector}")},
        "import-operation":{"kind":"source-ref","value":op_hex}
    }});
    aterm_db::accounts::validate(&value).map_err(|e| e.to_string())?;
    Ok(Some(Imported { key, value, origin }))
}

pub(super) fn current(
    paths: &Paths,
    key: &str,
    origin: &Value,
    deadline: Instant,
) -> Result<(), String> {
    if origin.is_null() {
        return Ok(());
    }
    let selector = origin["selector"]
        .as_str()
        .ok_or("invalid imported intake origin")?;
    if origin.as_object().is_none_or(|value| value.len() != 3) {
        return Err("invalid imported intake origin".into());
    }
    let imported = resolve(paths, selector, deadline)?.ok_or("invalid imported intake selector")?;
    if imported.key != key || imported.origin != *origin {
        return Err("import source changed after key intake".into());
    }
    Ok(())
}

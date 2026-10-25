// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded spreadsheet import. Common identities become encrypted proposals;
//! ambiguous provider notes, payment information and alias/rename relationships
//! remain source references for explicit classification. The original is untouched.

use crate::{
    Error, Result, crypto,
    database::{Change, Command},
    local::Database,
};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

const MAX_CSV: usize = 1024 * 1024;
const MAX_ROWS: usize = 4096;
const MAX_COLUMNS: usize = 64;
const MAX_CELL: usize = 8192;
const PROVIDERS: &[(&str, &str)] = &[
    ("claude", "anthropic"),
    ("open ai", "openai"),
    ("google", "google"),
    ("meta", "meta"),
    ("kimi", "kimi"),
    ("x", "xai"),
];

struct Csv {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    digest: [u8; 32],
}
fn load(path: &Path) -> Result<Csv> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_CSV + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CSV {
        return Err(Error::new("CSV exceeds import capacity"));
    }
    let digest = Sha256::digest(&bytes);
    let mut records = parse(&bytes)?;
    if records.is_empty() {
        return Err(Error::new("CSV has no header"));
    }
    let mut headers = records.remove(0);
    if let Some(first) = headers.first_mut() {
        *first = first.trim_start_matches('\u{feff}').to_owned();
    }
    Ok(Csv {
        headers,
        rows: records,
        digest,
    })
}
fn parse(bytes: &[u8]) -> Result<Vec<Vec<String>>> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut records = Vec::new();
    let mut row = Vec::new();
    let mut cell = Vec::new();
    let (mut quoted, mut closed, mut start) = (false, false, true);
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        at += 1;
        if quoted {
            if byte == b'"' {
                if bytes.get(at) == Some(&b'"') {
                    cell.push(b'"');
                    at += 1;
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                cell.push(byte);
            }
        } else if byte == b'"' && start {
            quoted = true;
            start = false;
        } else if byte == b',' {
            finish_cell(&mut row, &mut cell)?;
            closed = false;
            start = true;
        } else if byte == b'\n' || byte == b'\r' {
            if byte == b'\r' && bytes.get(at) == Some(&b'\n') {
                at += 1;
            }
            finish_cell(&mut row, &mut cell)?;
            records.push(std::mem::take(&mut row));
            closed = false;
            start = true;
        } else {
            if closed || byte == b'"' {
                return Err(Error::new("invalid CSV quoting"));
            }
            cell.push(byte);
            start = false;
        }
        if cell.len() > MAX_CELL || row.len() > MAX_COLUMNS || records.len() > MAX_ROWS {
            return Err(Error::new("CSV row, column or cell exceeds capacity"));
        }
    }
    if quoted {
        return Err(Error::new("unterminated quoted CSV cell"));
    }
    if !row.is_empty() || !cell.is_empty() || closed || !start {
        finish_cell(&mut row, &mut cell)?;
        records.push(row);
    }
    if records.len() > MAX_ROWS {
        return Err(Error::new("CSV has too many records"));
    }
    Ok(records)
}
fn finish_cell(row: &mut Vec<String>, cell: &mut Vec<u8>) -> Result<()> {
    if row.len() >= MAX_COLUMNS {
        return Err(Error::new("CSV has too many columns"));
    }
    row.push(String::from_utf8(std::mem::take(cell)).map_err(|_| Error::new("CSV is not UTF-8"))?);
    Ok(())
}
fn provider(header: &str) -> Option<&'static str> {
    let header = header.trim().to_ascii_lowercase();
    PROVIDERS
        .iter()
        .find_map(|(h, p)| (*h == header).then_some(*p))
}
fn nonempty(cell: &str) -> bool {
    !cell.trim().is_empty()
}
fn possible_phone(cell: &str) -> bool {
    cell.bytes().filter(u8::is_ascii_digit).count() >= 7
}
fn source(digest: &[u8; 32], record: usize, column: usize) -> Value {
    json!({"sha256":hex::encode_lower(digest),"record":record,"column":column})
}
fn object_id(domain: &[u8], parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    for part in parts {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hex::encode_lower(&hash.finalize())
}

/// Inspect shapes/counts only. Never prints cells, addresses, phones or payment values.
pub fn inspect(path: &Path) -> Result<Value> {
    let csv = load(path)?;
    let mut columns = Vec::new();
    let (mut populated, mut providers, mut phone_cells, mut alias_markers) = (0, 0, 0, 0);
    for (column, header) in csv.headers.iter().enumerate() {
        let count = csv
            .rows
            .iter()
            .filter_map(|row| row.get(column))
            .filter(|v| nonempty(v))
            .count();
        let safe_header =
            if header.len() <= 64 && !header.contains('@') && !header.chars().any(char::is_control)
            {
                header.clone()
            } else {
                format!("column-{}", column + 1)
            };
        columns.push(json!({"column":column+1,"header":safe_header,"nonempty":count}));
    }
    for row in &csv.rows {
        if row.iter().any(|v| nonempty(v)) {
            populated += 1;
        }
        for (column, cell) in row.iter().enumerate() {
            if !nonempty(cell) {
                continue;
            }
            if csv
                .headers
                .get(column)
                .is_some_and(|h| provider(h).is_some())
            {
                providers += 1;
            }
            if possible_phone(cell) {
                phone_cells += 1;
            }
            let lower = cell.to_ascii_lowercase();
            if lower.contains("alias") || lower.contains("forward") {
                alias_markers += 1;
            }
        }
    }
    Ok(
        json!({"source_sha256":hex::encode_lower(&csv.digest),"records":csv.rows.len(),"populated_records":populated,"columns":columns,"provider_cells":providers,"possible_phone_cells":phone_cells,"explicit_alias_or_forward_markers":alias_markers,"source_values_printed":false}),
    )
}

/// Import unambiguous email identities and inert provider/name proposals. All
/// other meaningful cells retain exact source coordinates for later classification.
/// A start marker binds the caller's request to the exact file; chunk requests are
/// deterministic, so restart resumes instead of duplicating partially saved work.
pub fn apply(dir: &Path, path: &Path, request: [u8; 16]) -> Result<Value> {
    let csv = load(path)?;
    let email_col = csv
        .headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("email"))
        .ok_or_else(|| Error::new("CSV has no email column"))?;
    let label_col = csv
        .headers
        .iter()
        .position(|h| h.trim().eq_ignore_ascii_case("computer"));
    let mut changes = BTreeMap::<(String, String), Change>::new();
    let (mut rejected, mut account_count, mut unresolved_cells) = (0usize, 0usize, 0usize);
    for (index, row) in csv.rows.iter().enumerate() {
        if row.iter().all(|v| !nonempty(v)) {
            continue;
        }
        let record = index + 2;
        let Some(email) = row.get(email_col).map(|s| s.trim()).filter(|s| nonempty(s)) else {
            rejected += 1;
            continue;
        };
        let email_id = object_id(b"aterm-db/email-address/1", &[email.as_bytes()]);
        let email_value = json!({"address":email});
        if crate::contacts::validate("email-addresses", &email_value).is_err() {
            rejected += 1;
            continue;
        }
        changes.insert(
            ("email-addresses".into(), email_id.clone()),
            Change {
                collection: "email-addresses".into(),
                key: email_id.clone(),
                value: Some(email_value),
            },
        );
        let label = label_col
            .and_then(|column| row.get(column))
            .map(|s| s.trim())
            .filter(|s| crate::machines::Slot::parse(s).is_ok());
        let slot = label.and_then(|s| crate::machines::Slot::parse(s).ok());
        let mut source_cells = Vec::new();
        for (column, cell) in row.iter().enumerate() {
            if nonempty(cell) && column != email_col {
                source_cells.push(source(&csv.digest, record, column + 1));
                unresolved_cells += 1;
            }
        }
        for (column, cell) in row.iter().enumerate() {
            let Some(provider) = csv.headers.get(column).and_then(|h| provider(h)) else {
                continue;
            };
            if !nonempty(cell) {
                continue;
            }
            let key = object_id(
                b"aterm-db/import-account/1",
                &[
                    &csv.digest,
                    &(record as u64).to_le_bytes(),
                    &(column as u64).to_le_bytes(),
                ],
            );
            let value = json!({"provider":provider,"email_id":email_id,"label_hint":label,"machine_name_hint":slot.as_ref().map(|s|s.machine.canonical()),"slot_hint":slot.as_ref().map(|s|s.number),"preferred_auth_hint":if provider=="google"{"email-link-if-offered"}else{"unclassified"},"provider_marker":if cell.trim()=="1"{"presence-proposal"}else{"unclassified"},"source":source(&csv.digest,record,column+1),"unclassified_source_cells":source_cells,"status":"needs-classification","mailbox_link":"unconfirmed"});
            changes.insert(
                ("account-proposals".into(), key.clone()),
                Change {
                    collection: "account-proposals".into(),
                    key,
                    value: Some(value),
                },
            );
            account_count += 1;
        }
    }
    let changes: Vec<_> = changes.into_values().collect();
    // Plan every bounded chunk before the first mutation. A wide private note
    // becomes only a source coordinate, but many coordinates can still be large.
    let mut chunks = Vec::new();
    let mut chunk = Vec::new();
    for change in &changes {
        chunk.push(change.clone());
        let candidate = Command::Write {
            request: chunk_request(request, chunks.len() as u64),
            changes: chunk.clone(),
        };
        if chunk.len() > crate::database::MAX_WRITES || candidate.encode()?.len() > crypto::MAX_BODY
        {
            let last = chunk
                .pop()
                .ok_or_else(|| Error::new("empty import chunk"))?;
            if chunk.is_empty() {
                return Err(Error::new(
                    "one imported proposal exceeds transaction capacity",
                ));
            }
            chunks.push(std::mem::take(&mut chunk));
            chunk.push(last);
            if (Command::Write {
                request: chunk_request(request, chunks.len() as u64),
                changes: chunk.clone(),
            })
            .encode()?
            .len()
                > crypto::MAX_BODY
            {
                return Err(Error::new(
                    "one imported proposal exceeds transaction capacity",
                ));
            }
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    let mut db = Database::open(dir)?;
    let import_key = hex::encode_lower(&request);
    let digest = hex::encode_lower(&csv.digest);
    let start = Command::Write {
        request: chunk_request(request, u64::MAX),
        changes: vec![Change {
            collection: "imports".into(),
            key: import_key.clone(),
            value: Some(
                json!({"source_sha256":digest,"state":"importing","changes":changes.len()}),
            ),
        }],
    };
    db.commit(start)?;
    for (index, chunk) in chunks.into_iter().enumerate() {
        db.commit(Command::Write {
            request: chunk_request(request, index as u64),
            changes: chunk,
        })?;
    }
    db.commit(Command::Write{request:chunk_request(request,u64::MAX-1),changes:vec![Change{collection:"imports".into(),key:import_key,value:Some(json!({"source_sha256":digest,"state":"complete","changes":changes.len(),"proposals":account_count,"unclassified_cells":unresolved_cells}))}]})?;
    Ok(
        json!({"source_sha256":digest,"provider_proposals":account_count,"email_identities":changes.iter().filter(|c|c.collection=="email-addresses").count(),"unclassified_source_cells":unresolved_cells,"records_without_an_unambiguous_email":rejected,"raw_source_modified":false,"raw_unclassified_values_replicated":false,"mailbox_aliases_inferred":false,"request":hex::encode_lower(&request)}),
    )
}
fn chunk_request(request: [u8; 16], index: u64) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(b"aterm-db/import-chunk/1");
    hash.update(request);
    hash.update(index.to_le_bytes());
    let digest = hash.finalize();
    let mut result = [0; 16];
    result.copy_from_slice(&digest[..16]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_quotes_multiline_unicode_and_empty_columns_preserve_positions() {
        let table = parse("a,,c\r\n\"two, fields\",\"a\"\"b\nü\",\r\n".as_bytes()).unwrap();
        assert_eq!(
            table,
            vec![vec!["a", "", "c"], vec!["two, fields", "a\"b\nü", ""]]
        );
        for bad in ["a,\"not closed", "\"a\"trailing", "a\"b"] {
            assert!(parse(bad.as_bytes()).is_err());
        }
    }
    #[test]
    fn private_source_import_is_restartable_without_guessing_aliases_or_copying_notes() {
        let root = crate::tests::tmp();
        let path = root.path().join("accounts.csv");
        std::fs::write(&path,"m,email,computer,google,notes\n7,owner+two@example.invalid,m3.2,phone required,password PRIVATE-CELL\nunknown,owner@example.invalid,m100,email link,alias mapping unconfirmed\n").unwrap();
        let dir = root.path().join("db");
        crate::local::init(&dir, "accounts").unwrap();
        let report = apply(&dir, &path, [1; 16]).unwrap();
        assert_eq!(report["provider_proposals"].as_u64(), Some(2));
        let first = crate::tests::wires(&dir).len();
        apply(&dir, &path, [1; 16]).unwrap();
        assert_eq!(crate::tests::wires(&dir).len(), first);
        let db = Database::open(&dir).unwrap();
        let view = db.view();
        assert_eq!(
            view.cells
                .keys()
                .filter(|(c, _)| c == "email-addresses")
                .count(),
            2
        );
        for cell in view.cells.values() {
            for c in &cell.candidates {
                assert!(
                    !aterm_json::to_string(&c.value)
                        .unwrap()
                        .contains("PRIVATE-CELL")
                );
            }
        }
        let proposals: Vec<_> = view
            .cells
            .iter()
            .filter(|((collection, _), _)| collection == "account-proposals")
            .map(|(_, cell)| &cell.candidates[0].value)
            .collect();
        assert!(
            proposals
                .iter()
                .any(|p| p["machine_name_hint"] == json!("m3") && p["slot_hint"] == json!(2))
        );
        assert_eq!(
            inspect(&path).unwrap()["source_values_printed"],
            json!(false)
        );
    }
}

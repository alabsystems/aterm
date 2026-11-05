// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded spreadsheet import. Common identities become encrypted proposals;
//! ambiguous provider notes, payment information and alias/rename relationships
//! remain source references for explicit classification. The original is untouched.

use crate::{
    Error, Result, crypto,
    database::{Change, Command},
    local::Database,
    typed::keys,
};
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::{Value, json};
use std::{collections::BTreeMap, fs::OpenOptions, io::Read, path::Path};

const MAX_CSV: usize = 1024 * 1024;
/// The most records a sheet holds, its header included.
pub(crate) const MAX_ROWS: usize = 4096;
/// The most columns a sheet's record holds.
pub(crate) const MAX_COLUMNS: usize = 64;
/// The most bytes one cell holds.
pub(crate) const MAX_CELL: usize = 8192;
/// The largest xlsx workbook a sheet is read from: its other tabs and styles come with it.
pub(crate) const MAX_WORKBOOK: usize = 32 * 1024 * 1024;
/// The sheet's provider columns and the one provider word each names, shared with the typed
/// lab model (`typed::keys::PROVIDERS`).
const PROVIDERS: &[(&str, &str)] = &[
    ("claude", keys::ANTHROPIC),
    ("open ai", keys::OPENAI),
    ("google", keys::GOOGLE),
    ("meta", keys::META),
    ("kimi", keys::KIMI),
    ("x", keys::XAI),
];

/// The provider words the sheet's columns name.
#[cfg(test)]
pub(crate) fn provider_words() -> impl Iterator<Item = &'static str> {
    PROVIDERS.iter().map(|(_, word)| *word)
}

pub mod commons;

struct Csv {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    digest: [u8; 32],
}
impl Csv {
    fn column(&self, name: &str) -> Result<Option<usize>> {
        let mut matches = self
            .headers
            .iter()
            .enumerate()
            .filter(|(_, header)| header.trim().eq_ignore_ascii_case(name))
            .map(|(index, _)| index);
        let column = matches.next();
        if matches.next().is_some() {
            return Err(Error::new("CSV has ambiguous identity columns"));
        }
        Ok(column)
    }
}
/// `O_NONBLOCK | O_CLOEXEC`: 0o4000 | 0o2_000_000 on Linux, 0x0004 | 0x0100_0000 on Darwin/BSDs.
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_NONBLOCK_CLOEXEC: i32 = 0o4000 | 0o2_000_000;
/// `O_NONBLOCK | O_CLOEXEC`: 0o4000 | 0o2_000_000 on Linux, 0x0004 | 0x0100_0000 on Darwin/BSDs.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const O_NONBLOCK_CLOEXEC: i32 = 0x0004 | 0x0100_0000;

/// Read a CSV file's bytes within the import's capacity: a regular file only, opened without
/// blocking on a FIFO, at most 1 MiB.
///
/// # Errors
/// It cannot be opened, is not a regular file, or is larger than the import reads.
pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    read_within(path, MAX_CSV)
}

/// [`read_bounded`] with `max` bytes the most read: an xlsx workbook ([`MAX_WORKBOOK`]).
///
/// # Errors
/// It cannot be opened, is not a regular file, or is larger than `max`.
pub(crate) fn read_within(path: &Path, max: usize) -> Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::new("a sheet's path is not a regular file"));
    }
    if meta.len() > max as u64 {
        return Err(Error::new("a sheet exceeds import capacity"));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take((max + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::new("a sheet exceeds import capacity"));
    }
    Ok(bytes)
}

fn load(path: &Path) -> Result<Csv> {
    let bytes = read_bounded(path)?;
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
/// Read CSV records within the import's row, column and cell caps; quotes, CRLF and a BOM
/// are handled as the sheets write them.
///
/// # Errors
/// Malformed quoting, non-UTF-8 text, or a cap passed.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Vec<String>>> {
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
/// The provider word a sheet header names (`claude`, `Open AI`, `google`, `meta`, `kimi`, `x`).
pub(crate) fn provider(header: &str) -> Option<&'static str> {
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

/// Prepare an inert, compact source document. Raw unclassified cells and
/// headers stay in the source file; only their one-based coordinates travel.
fn candidate_document(path: &Path) -> Result<Value> {
    let csv = load(path)?;
    let email_column = csv
        .column("email")?
        .ok_or_else(|| Error::new("CSV has no email column"))?;
    let machine_column = csv.column("computer")?;
    let mut rows = Vec::new();
    for (index, row) in csv.rows.iter().enumerate() {
        if row.iter().all(|cell| !nonempty(cell)) {
            continue;
        }
        let email = row.get(email_column).map(|s| s.trim()).filter(|email| {
            crate::contacts::validate("email-addresses", &json!({"address":email})).is_ok()
        });
        let machine_slot = machine_column
            .and_then(|column| row.get(column))
            .map(|s| s.trim())
            .filter(|label| crate::machines::Slot::parse(label).is_ok());
        let mut providers = Vec::new();
        let mut unresolved = Vec::new();
        for (column, cell) in row.iter().enumerate() {
            if !nonempty(cell) {
                continue;
            }
            if let Some(provider) = csv.headers.get(column).and_then(|h| provider(h))
                && matches!(cell.trim(), "0" | "1")
            {
                providers.push(json!({"provider":provider,"column":column+1,"presence_marker":cell.trim()=="1"}));
            }
            if !(column == email_column && email.is_some()
                || Some(column) == machine_column && machine_slot.is_some())
            {
                unresolved.push(column + 1);
            }
        }
        rows.push(
            json!({"record":index+2,"email":email,"email_column":email_column+1,
            "machine_slot_hint":machine_slot,"machine_column":machine_column.map(|c|c+1),
            "providers":providers,"unclassified_columns":unresolved}),
        );
    }
    Ok(
        json!({"schema":1,"source_sha256":hex::encode_lower(&csv.digest),
        "records":csv.rows.len(),"rows":rows}),
    )
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
/// Ambiguous identity headers are rejected before opening or changing the database.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbImportAtomic",
        action = "Reject",
        project = "aterm_db::import::commons::tests::atomic_transition"
    )
)]
pub fn apply(dir: &Path, path: &Path, request: [u8; 16]) -> Result<Value> {
    let csv = load(path)?;
    let email_col = csv
        .column("email")?
        .ok_or_else(|| Error::new("CSV has no email column"))?;
    let label_col = csv.column("computer")?;
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
    fn only_binary_provider_markers_make_presence_claims() {
        let root = crate::tests::tmp();
        let path = root.path().join("markers.csv");
        for (cell, expected) in [
            ("1", Some(true)),
            (" 1 ", Some(true)),
            ("0", Some(false)),
            (" 0 ", Some(false)),
            ("phone required", None),
            ("email link", None),
            ("true", None),
            ("false", None),
            ("01", None),
            ("1.0", None),
            ("0.0", None),
        ] {
            std::fs::write(&path, format!("email,google\nowner@example.test,{cell}\n")).unwrap();
            let value = candidate_document(&path).unwrap();
            let row = &value["rows"][0];
            let providers = row["providers"].as_array().unwrap();
            if let Some(present) = expected {
                assert_eq!(
                    providers,
                    &[json!({"provider":"google","column":2,"presence_marker":present})],
                    "{cell}"
                );
            } else {
                assert!(providers.is_empty(), "{cell} is unclassified, not absent");
            }
            assert_eq!(row["unclassified_columns"], json!([2]));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("email,google\nowner@example.test,{cell}\n")
            );
        }
    }

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
    fn identity_columns_keep_source_positions_and_allow_no_machine_column() {
        for (headers, email, machine) in [
            (vec!["notes", " EMAIL ", "Computer", "notes"], 1, Some(2)),
            (vec!["email", "google", "notes"], 0, None),
            (vec!["computer", "email"], 1, Some(0)),
        ] {
            let csv = Csv {
                headers: headers.into_iter().map(str::to_owned).collect(),
                rows: Vec::new(),
                digest: [0; 32],
            };
            assert_eq!(csv.column("email").unwrap(), Some(email));
            assert_eq!(csv.column("computer").unwrap(), machine);
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
        assert!(inspect(root.path()).is_err());
    }
}

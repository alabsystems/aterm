// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The grid round trip without a store: exports read back after edits made exactly as amacs's
//! grid commits them (`amacs::table::edit_cell`, `insert_row`, `delete_row`), against hand-built
//! causal histories. Every address is synthetic (`example.invalid`).

use super::*;
use crate::{
    alab::registry::{ACCOUNT, ADDRESS, MACHINE, MAILBOX, PREFERENCE, REGISTRY},
    typed::keys,
};
use astream_wire::sync::{Actor, Header, Index};

/// A hand-built causal history: each write names the writes it observed.
pub(crate) struct Book {
    index: Index,
    next: u64,
    heads: BTreeMap<u8, (u64, Digest)>,
    ops: Vec<(Digest, Vec<Change>)>,
}

fn actor(n: u8) -> Actor {
    [n; 48]
}

impl Book {
    pub(crate) fn new() -> Self {
        Self {
            index: Index::new(1024),
            next: 0,
            heads: BTreeMap::new(),
            ops: Vec::new(),
        }
    }

    /// Append `changes` by `writer`, observing its own previous write and `seen`.
    pub(crate) fn write(&mut self, writer: u8, seen: &[Digest], changes: Vec<Change>) -> Digest {
        self.next += 1;
        let mut id = [0; 32];
        id[..8].copy_from_slice(&self.next.to_be_bytes());
        let previous = self.heads.get(&writer).map(|(_, id)| *id);
        let sequence = self.heads.get(&writer).map_or(1, |(s, _)| s + 1);
        let mut parents: BTreeSet<Digest> = seen.iter().copied().collect();
        parents.extend(previous);
        self.index
            .insert(Header {
                id,
                actor: actor(writer),
                sequence,
                previous,
                parents: parents.into_iter().collect(),
            })
            .unwrap();
        self.heads.insert(writer, (sequence, id));
        self.ops.push((id, changes));
        id
    }

    /// Every write so far: what an append that observes everything names.
    pub(crate) fn all(&self) -> Vec<Digest> {
        self.ops.iter().map(|(id, _)| *id).collect()
    }

    pub(crate) fn cells(&self) -> Cells {
        let mut cells = Cells::new();
        for id in self.index.project().ordered {
            let (_, changes) = self.ops.iter().find(|op| op.0 == id).unwrap();
            crate::documents::apply(
                &|later, earlier| self.index.observes(later, earlier),
                &mut cells,
                id,
                changes,
                true,
            );
        }
        cells
    }
}

pub(crate) fn put(collection: &Collection, pk: &str, col: &str, x: Value) -> Change {
    cell::change(
        collection,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::owner(1, x),
    )
    .unwrap()
}

/// The owner's add of `member` to the set column `col` (a text set's text, else a word or key).
pub(crate) fn elem(collection: &Collection, pk: &str, col: &str, member: &str) -> Change {
    cell::member_change(collection, pk, col, member, true, |x| {
        CellValue::owner(1, x)
    })
    .unwrap()
}

/// A machine row `m9` keyed `pk`.
pub(crate) fn machine(pk: &str) -> Vec<Change> {
    vec![
        put(&MACHINE, pk, "label", json!("m9")),
        put(&MACHINE, pk, "status", json!("active")),
        put(&MACHINE, pk, "location", json!("rack a")),
        elem(&MACHINE, pk, "clis", "claude"),
        elem(&MACHINE, pk, "clis", "codex"),
        put(&MACHINE, pk, "row", json!("live")),
    ]
}

pub(crate) const MC: &str = "mc-0000000000000009";

/// The grid column named `name` (its header without `=`).
pub(crate) fn column(text: &str, name: &str) -> usize {
    let sheet = Sheet::scan(text, Delimiter::Tab).unwrap();
    (0..sheet.records[0].cells.len())
        .find(|&at| {
            sheet
                .cell(0, at)
                .unwrap()
                .value(text)
                .trim_start_matches(READ_ONLY)
                == name
        })
        .unwrap()
}

/// The record whose pk is `pk`.
pub(crate) fn record(text: &str, pk: &str) -> usize {
    let sheet = Sheet::scan(text, Delimiter::Tab).unwrap();
    let at = column(text, PK);
    (1..sheet.records.len())
        .find(|&r| sheet.cell(r, at).is_some_and(|c| c.value(text) == pk))
        .unwrap()
}

/// `text` after the owner types `value` into one cell, as amacs's grid commits it.
pub(crate) fn type_in(text: &str, row: usize, name: &str, value: &str) -> String {
    let sheet = Sheet::scan(text, Delimiter::Tab).unwrap();
    let edit = amacs::table::edit_cell(&sheet, text, row, column(text, name), value)
        .unwrap()
        .unwrap();
    let mut out = text.to_owned();
    out.replace_range(edit.range, &edit.insert);
    out
}

/// `text` with one row deleted (C-S-Backspace).
pub(crate) fn delete(text: &str, row: usize) -> String {
    let sheet = Sheet::scan(text, Delimiter::Tab).unwrap();
    let edit = amacs::table::delete_row(&sheet, text, row)
        .unwrap()
        .unwrap();
    let mut out = text.to_owned();
    out.replace_range(edit.range, &edit.insert);
    out
}

/// `text` with an empty row added after `row` (C-o); the new row's index is `row + 1`.
pub(crate) fn add_row(text: &str, row: usize) -> String {
    let sheet = Sheet::scan(text, Delimiter::Tab).unwrap();
    let edit = amacs::table::insert_row(&sheet, text, row)
        .unwrap()
        .unwrap();
    let mut out = text.to_owned();
    out.replace_range(edit.range, &edit.insert);
    out
}

fn no_birth(_: &Collection, _: &BTreeMap<String, Value>) -> Result<String> {
    Err(Error::new("no births here"))
}

fn registry_birth(
    cells: &Cells,
) -> impl Fn(&Collection, &BTreeMap<String, Value>) -> Result<String> {
    move |collection, values| crate::alab::registry::birth(collection, values, cells)
}

/// An export shows every address as it is, and a save of the file as exported asks for nothing:
/// no cell is written for a cell whose text did not change.
/// NEGATIVE CONTROL: with `show` abbreviating an address, the export lacks it:
/// `` assertion failed: shown.text.contains("ann.lee@example.invalid") ``.
#[test]
fn an_unchanged_save_asks_for_nothing_and_addresses_show_as_they_are() {
    let mut book = Book::new();
    let pk = keys::address_pk("ann.lee@example.invalid").unwrap();
    book.write(
        1,
        &[],
        vec![
            put(&ADDRESS, &pk, "addr", json!("ann.lee@example.invalid")),
            put(&ADDRESS, &pk, "domain", json!("example.invalid")),
            put(&ADDRESS, &pk, "origin", json!("owner")),
            put(&ADDRESS, &pk, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let shown = export(&REGISTRY, &ADDRESS, &cells, &[]).unwrap();
    assert!(shown.text.contains("ann.lee@example.invalid"));
    assert!(!shown.text.contains("…@…"));
    let plan = super::plan(&REGISTRY, &ADDRESS, &shown.base, &shown.text, &no_birth).unwrap();
    assert!(plan.is_empty(), "{plan:?}");
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    let exported = export(&REGISTRY, &MACHINE, &book.cells(), &[]).unwrap();
    assert_eq!(
        super::plan(
            &REGISTRY,
            &MACHINE,
            &exported.base,
            &exported.text,
            &no_birth
        ),
        Ok(Plan::default())
    );
}

/// An owner's edit that a peer's import happened to match after the export is still written:
/// the cell holds the owner's value only as the import's, so the save makes it the owner's and
/// a later import with another value leaves it.
/// NEGATIVE CONTROL: with `settle` counting a matching imported cell as unchanged (its `holds &&
/// imported` arm removed), nothing is written: `` assertion `left == right` failed left: 1
/// right: 0 `` (the unchanged count).
#[test]
fn an_owner_edit_an_import_matched_meanwhile_is_still_written() {
    let source = "sr-0011223344556677";
    let mut book = Book::new();
    let first = book.write(1, &[], machine(MC));
    let exported = export(&REGISTRY, &MACHINE, &book.cells(), &[]).unwrap();
    // A peer's import writes the very value the owner is about to type.
    book.write(
        2,
        &[first],
        vec![
            cell::change(
                &MACHINE,
                CellKey {
                    pk: MC,
                    col: "status",
                    elem: None,
                },
                &CellValue::imported(MACHINE.version, json!("parked"), source),
            )
            .unwrap(),
        ],
    );
    let row = record(&exported.text, MC);
    let saved = type_in(&exported.text, row, "status", "parked");
    let plan = super::plan(&REGISTRY, &MACHINE, &exported.base, &saved, &no_birth).unwrap();
    let settled = settle(&MACHINE, &plan, &book.cells());
    assert_eq!(settled.unchanged, 0);
    assert!(settled.stale.is_empty());
    let keys: Vec<&str> = settled
        .rows
        .iter()
        .flatten()
        .map(|c| c.key.as_str())
        .collect();
    assert_eq!(keys, [format!("{MC}.status")]);
    book.write(1, &book.all(), settled.rows.concat());
    let projection = cell::project(&MACHINE, &book.cells());
    assert!(matches!(
        cell::import_step(
            &MACHINE,
            projection.rows[MC].fields.get("status"),
            &json!("active")
        ),
        cell::ImportStep::Finding(_)
    ));
}

/// A save writes a changed cell only while its candidates are still the exported ones (design §8,
/// compare-before-write): a peer's edit to `status` after the export is never overwritten — that
/// cell comes back as `⚠ mine ‖ theirs` — while the owner's `location` edit goes ahead. Leaving the
/// reopened cell asks for nothing; editing it to one value writes that value over the peer's.
/// NEGATIVE CONTROL: with `settle` writing every edit whatever its register holds now (the
/// candidate comparison removed), the owner's `status` overwrites the peer's:
/// `` assertion failed: settled.stale.len() == 1 ``.
#[test]
fn an_edit_replaces_only_the_candidates_the_export_showed() {
    let mut book = Book::new();
    let first = book.write(1, &[], machine(MC));
    let exported = export(&REGISTRY, &MACHINE, &book.cells(), &[]).unwrap();
    // A peer edits status after the export, having seen the row.
    book.write(
        2,
        &[first],
        vec![put(&MACHINE, MC, "status", json!("parked"))],
    );
    let row = record(&exported.text, MC);
    let saved = type_in(
        &type_in(&exported.text, row, "status", "lost"),
        row,
        "location",
        "rack b",
    );
    let plan = super::plan(&REGISTRY, &MACHINE, &exported.base, &saved, &no_birth).unwrap();
    assert_eq!(plan.rows.len(), 1);
    let settled = settle(&MACHINE, &plan, &book.cells());
    assert!(settled.stale.len() == 1);
    assert_eq!(
        settled.stale,
        [Stale {
            pk: MC.into(),
            column: "status".into(),
            mine: "lost".into()
        }]
    );
    let keys: Vec<&str> = settled
        .rows
        .iter()
        .flatten()
        .map(|c| c.key.as_str())
        .collect();
    assert_eq!(keys, [format!("{MC}.location")]);
    book.write(1, &book.all(), settled.rows.concat());
    let now = book.cells();
    let projection = cell::project(&MACHINE, &now);
    assert_eq!(projection.rows[MC].get("status"), Some(&json!("parked")));
    assert_eq!(projection.rows[MC].get("location"), Some(&json!("rack b")));
    // Reopened: the stale cell shows both values, mine first.
    let reopened = reopen(
        export(&REGISTRY, &MACHINE, &now, &[]).unwrap().base,
        &settled.stale,
    );
    let text = render(&reopened);
    assert!(text.contains("⚠ lost ‖ parked"), "{text}");
    assert_eq!(stale_line(&text, &reopened, &settled.stale), 2);
    assert!(
        super::plan(&REGISTRY, &MACHINE, &reopened, &text, &no_birth)
            .unwrap()
            .is_empty()
    );
    let resolved = type_in(&text, record(&text, MC), "status", "lost");
    let plan = super::plan(&REGISTRY, &MACHINE, &reopened, &resolved, &no_birth).unwrap();
    let settled = settle(&MACHINE, &plan, &now);
    assert!(settled.stale.is_empty());
    book.write(1, &book.all(), settled.rows.concat());
    assert_eq!(
        cell::project(&MACHINE, &book.cells()).rows[MC].get("status"),
        Some(&json!("lost"))
    );
}

/// A conflicted cell exports as `⚠ a ‖ b`; leaving it asks for nothing, and editing it to one
/// value writes over both candidates, so it reads as that one value afterwards.
/// NEGATIVE CONTROL: with the baseline keeping only a register's first candidate (`registers`
/// truncating each digest list to one), the resolving edit expects one candidate where the
/// register holds two, so it would be held back as stale: `` left: 1 right: 2 ``.
#[test]
fn a_conflicted_cell_shows_both_values_and_one_value_resolves_it() {
    let mut book = Book::new();
    let first = book.write(1, &[], machine(MC));
    book.write(1, &[], vec![put(&MACHINE, MC, "location", json!("rack b"))]);
    book.write(
        2,
        &[first],
        vec![put(&MACHINE, MC, "location", json!("rack c"))],
    );
    let cells = book.cells();
    let exported = export(&REGISTRY, &MACHINE, &cells, &[]).unwrap();
    assert!(
        exported.text.contains("⚠ rack b ‖ rack c"),
        "{}",
        exported.text
    );
    assert!(
        super::plan(
            &REGISTRY,
            &MACHINE,
            &exported.base,
            &exported.text,
            &no_birth
        )
        .unwrap()
        .is_empty()
    );
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "location",
        "rack c",
    );
    let plan = super::plan(&REGISTRY, &MACHINE, &exported.base, &saved, &no_birth).unwrap();
    assert_eq!(plan.rows[0].1[0].expected.len(), 2);
    let settled = settle(&MACHINE, &plan, &cells);
    assert!(settled.stale.is_empty());
    book.write(1, &book.all(), settled.rows.concat());
    let field = cell::project(&MACHINE, &book.cells()).rows[MC].fields["location"].clone();
    assert!(!field.conflicted());
    assert_eq!(field.value(), Some(&json!("rack c")));
}

/// A read-only column (`=`: the key, import coordinates, bounded JSON) refuses an edit, as does a
/// value that is not of its column's type; nothing is planned, and no problem quotes what the owner
/// typed.
/// NEGATIVE CONTROL: with the read-only check of an existing row's cells removed, a well-formed
/// source coordinate typed into `=src` is planned as an edit: `` called `Result::unwrap_err()` on an
/// `Ok` value ``.
#[test]
fn read_only_columns_and_bad_values_refuse_and_name_no_value() {
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    let exported = export(&REGISTRY, &MACHINE, &book.cells(), &[]).unwrap();
    assert!(exported.text.starts_with("label\taliases\tstatus"));
    assert!(exported.text.contains("\t=src\t=notes_src\t=pk\n"));
    let row = record(&exported.text, MC);
    let coordinate = r#"{"source":"sr-0123456789abcdef","row":3,"col":4}"#;
    let problems = plan(
        &REGISTRY,
        &MACHINE,
        &exported.base,
        &type_in(&exported.text, row, "src", coordinate),
        &no_birth,
    )
    .unwrap_err();
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].rule, "this column is read-only (=)");
    assert_eq!((problems[0].record, problems[0].line), (row, row + 1));
    let typed_pk = plan(
        &REGISTRY,
        &MACHINE,
        &exported.base,
        &type_in(&exported.text, row, PK, "mc-1111111111111111"),
        &no_birth,
    )
    .unwrap_err();
    assert!(typed_pk[0].rule.contains("read-only"), "{typed_pk:?}");
    let bad = plan(
        &REGISTRY,
        &MACHINE,
        &exported.base,
        &type_in(&exported.text, row, "status", "sideways"),
        &no_birth,
    )
    .unwrap_err();
    assert_eq!(bad[0].column, column(&exported.text, "status"));
    assert!(bad[0].rule.starts_with("machine.status: "), "{bad:?}");
    let secret = plan(
        &REGISTRY,
        &MACHINE,
        &exported.base,
        &type_in(&exported.text, row, "note", "password hunter2"),
        &no_birth,
    )
    .unwrap_err();
    for problem in [&problems[0], &typed_pk[0], &bad[0], &secret[0]] {
        for typed in [
            "sideways",
            "hunter2",
            "sr-0123456789abcdef",
            "mc-1111111111111111",
        ] {
            assert!(!problem.rule.contains(typed), "{problem:?}");
        }
    }
    let header = type_in(&exported.text, 0, "status", "state");
    assert_eq!(
        super::plan(&REGISTRY, &MACHINE, &exported.base, &header, &no_birth).unwrap_err()[0].record,
        0
    );
}

/// A deleted grid row is retired, so its history stays; a deleted preference ends instead (the
/// table names preferences among the collections whose rows end).
/// NEGATIVE CONTROL: with the deleted-rows pass of `plan` removed, deleting the row plans nothing:
/// `` assertion `left == right` failed  left: [] right: ["mc-0000000000000009"] ``.
#[test]
fn a_deleted_row_retires_and_a_deleted_preference_ends() {
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    let account = keys::account_pk("claude", "ann.lee@example.invalid").unwrap();
    let machine_pk = keys::machine_pk("m3");
    let preference = keys::preference_pk(&machine_pk, &account).unwrap();
    book.write(
        1,
        &[],
        vec![
            put(&PREFERENCE, &preference, "machine", json!(machine_pk)),
            put(&PREFERENCE, &preference, "account", json!(account)),
            put(&PREFERENCE, &preference, "why", json!("owner")),
            put(&PREFERENCE, &preference, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let machines = export(&REGISTRY, &MACHINE, &cells, &[]).unwrap();
    let saved = delete(&machines.text, record(&machines.text, MC));
    let retire = super::plan(&REGISTRY, &MACHINE, &machines.base, &saved, &no_birth).unwrap();
    assert_eq!(retire.retired, [MC]);
    let settled = settle(&MACHINE, &retire, &cells);
    assert_eq!(settled.rows.len(), 1);
    assert_eq!(settled.rows[0][0].key, format!("{MC}.row"));
    assert_eq!(
        settled.rows[0][0].value.as_ref().unwrap()["x"],
        json!("retired")
    );
    let preferences = export(&REGISTRY, &PREFERENCE, &cells, &[]).unwrap();
    let ended = plan(
        &REGISTRY,
        &PREFERENCE,
        &preferences.base,
        &delete(&preferences.text, record(&preferences.text, &preference)),
        &no_birth,
    )
    .unwrap();
    assert_eq!(ended.rows[0].1[0].x, json!("ended"));
    // A row already retired that is deleted again asks for nothing.
    book.write(1, &book.all(), settled.rows.concat());
    let after = export(&REGISTRY, &MACHINE, &book.cells(), &[]).unwrap();
    let again = delete(&after.text, record(&after.text, MC));
    assert!(
        super::plan(&REGISTRY, &MACHINE, &after.base, &again, &no_birth)
            .unwrap()
            .is_empty()
    );
}

/// A preference's key comes from its pair, so a saved edit of either cell is refused, naming the
/// way to say it: end the row and add another. Its other cells stay editable.
/// NEGATIVE CONTROL: with `.fixed()` off `PREFERENCE`'s `machine`, the edit plans: `` called
/// `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn a_preferences_pair_is_not_edited() {
    let account = keys::account_pk("claude", "ann.lee@example.invalid").unwrap();
    let machine_pk = keys::machine_pk("m3");
    let preference = keys::preference_pk(&machine_pk, &account).unwrap();
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&PREFERENCE, &preference, "machine", json!(machine_pk)),
            put(&PREFERENCE, &preference, "account", json!(account)),
            put(&PREFERENCE, &preference, "why", json!("owner")),
            put(&PREFERENCE, &preference, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let sheet = export(&REGISTRY, &PREFERENCE, &cells, &[]).unwrap();
    let other = keys::machine_pk("m5");
    let edited = sheet.text.replace(&machine_pk, &other);
    let problems = plan(&REGISTRY, &PREFERENCE, &sheet.base, &edited, &no_birth).unwrap_err();
    assert!(
        format!("{problems:?}").contains("end this row"),
        "{problems:?}"
    );
    let ranked = sheet.text.replacen(
        &format!("{machine_pk}\t{account}\t\t"),
        &format!("{machine_pk}\t{account}\t2\t"),
        1,
    );
    assert_ne!(ranked, sheet.text);
    assert!(plan(&REGISTRY, &PREFERENCE, &sheet.base, &ranked, &no_birth).is_ok());
}

/// A new grid row with its pk left empty is born under the key its natural columns derive (an
/// address's digest; a mailbox's random key), every cell and its `live` lifecycle in one record; a
/// new row that repeats an exported row, or misses a required value, is refused.
/// NEGATIVE CONTROL: with the birth's `live` lifecycle left out, the new row's record lacks it:
/// `` assertion failed: keys.contains(&format!("{born}.row")) ``.
#[test]
fn a_new_row_is_born_under_its_derived_key_in_one_record() {
    let mut book = Book::new();
    let ann = keys::address_pk("ann.lee@example.invalid").unwrap();
    book.write(
        1,
        &[],
        vec![
            put(&ADDRESS, &ann, "addr", json!("ann.lee@example.invalid")),
            put(&ADDRESS, &ann, "domain", json!("example.invalid")),
            put(&ADDRESS, &ann, "origin", json!("owner")),
            put(&ADDRESS, &ann, "row", json!("live")),
        ],
    );
    let cells = book.cells();
    let birth = registry_birth(&cells);
    let exported = export(&REGISTRY, &ADDRESS, &cells, &[]).unwrap();
    let mut text = add_row(&exported.text, 1);
    for (name, value) in [
        ("addr", "Bob.Ray@Example.invalid"),
        ("domain", "example.invalid"),
        ("origin", "owner"),
    ] {
        text = type_in(&text, 2, name, value);
    }
    let plan = super::plan(&REGISTRY, &ADDRESS, &exported.base, &text, &birth).unwrap();
    let born = keys::address_pk("bob.ray@example.invalid").unwrap();
    assert_eq!(plan.born, std::slice::from_ref(&born));
    let settled = settle(&ADDRESS, &plan, &cells);
    assert_eq!(settled.rows.len(), 1);
    let keys: Vec<String> = settled.rows[0].iter().map(|c| c.key.clone()).collect();
    assert!(keys.contains(&format!("{born}.row")));
    assert!(keys.contains(&format!("{born}.addr")));
    let addr = settled.rows[0]
        .iter()
        .find(|c| c.key.ends_with(".addr"))
        .unwrap();
    assert_eq!(
        addr.value.as_ref().unwrap()["x"],
        json!("bob.ray@example.invalid")
    );
    // The same address again is the exported row, not a second one.
    let mut twice = add_row(&exported.text, 1);
    for (name, value) in [
        ("addr", "ann.lee@example.invalid"),
        ("domain", "example.invalid"),
        ("origin", "owner"),
    ] {
        twice = type_in(&twice, 2, name, value);
    }
    let problems = super::plan(&REGISTRY, &ADDRESS, &exported.base, &twice, &birth).unwrap_err();
    assert_eq!(problems[0].column, column(&twice, PK));
    // A required column left empty is refused at that cell.
    let missing = type_in(&add_row(&exported.text, 1), 2, "addr", "cy@example.invalid");
    let problems = super::plan(&REGISTRY, &ADDRESS, &exported.base, &missing, &birth).unwrap_err();
    assert!(
        problems
            .iter()
            .any(|p| p.column == column(&missing, "domain")),
        "{problems:?}"
    );
    // A mailbox's key derives from its login's address key, a member-readable row's.
    let login = keys::address_pk("box@example.invalid").unwrap();
    let mailboxes = export(&REGISTRY, &MAILBOX, &cells, &[]).unwrap();
    let mut text = add_row(&mailboxes.text, 0);
    for (name, value) in [
        ("label", "proton-1"),
        ("service", "proton"),
        ("code_path", "proton-api"),
        ("login", login.as_str()),
    ] {
        text = type_in(&text, 1, name, value);
    }
    let plan = super::plan(&REGISTRY, &MAILBOX, &mailboxes.base, &text, &birth).unwrap();
    assert_eq!(plan.born, [keys::mailbox_pk(&login).unwrap()]);
}

/// A secret cell shows as the name of its store entry and is read-only: a new row's password,
/// or one typed over a reference, is refused, so a value typed into a grid never reaches a
/// change.
/// NEGATIVE CONTROL: with `read_only` leaving `SecretRef` editable, the typed-over reference is
/// read as JSON and refused for that instead: the `read-only` assertion fails.
#[test]
fn a_secret_cell_shows_its_entry_name_and_takes_no_typed_value() {
    let mut book = Book::new();
    let ann = keys::address_pk("ann.lee@example.invalid").unwrap();
    let entry = format!("alab-address-password-{ann}");
    book.write(
        1,
        &[],
        vec![
            put(&ADDRESS, &ann, "addr", json!("ann.lee@example.invalid")),
            put(&ADDRESS, &ann, "domain", json!("example.invalid")),
            put(&ADDRESS, &ann, "origin", json!("owner")),
            put(&ADDRESS, &ann, "row", json!("live")),
            put(
                &ADDRESS,
                &ann,
                "password",
                crate::typed::secret::reference("test-vault", &entry),
            ),
        ],
    );
    let cells = book.cells();
    let birth = registry_birth(&cells);
    let exported = export(&REGISTRY, &ADDRESS, &cells, &[]).unwrap();
    assert!(exported.text.contains(&entry), "{}", exported.text);
    assert!(exported.base.header.iter().any(|h| h == "=password"));
    let over = type_in(&exported.text, 1, "password", "pw-for-ann-01");
    let refused = super::plan(&REGISTRY, &ADDRESS, &exported.base, &over, &birth).unwrap_err();
    assert!(
        refused.iter().any(|p| p.rule.contains("read-only")),
        "{refused:?}"
    );
    assert!(refused.iter().all(|p| !p.rule.contains("pw-for-ann-01")));
    let mut text = add_row(&exported.text, 1);
    for (name, value) in [
        ("addr", "bob.ray@example.invalid"),
        ("domain", "example.invalid"),
        ("origin", "owner"),
        ("password", "pw-for-bob-01"),
    ] {
        text = type_in(&text, 2, name, value);
    }
    let refused = super::plan(&REGISTRY, &ADDRESS, &exported.base, &text, &birth).unwrap_err();
    assert!(
        refused.iter().all(|p| !p.rule.contains("pw-for-bob-01")),
        "{refused:?}"
    );
}

/// A born row is all or nothing: when a peer made the row under the same key after the plan
/// was read, none of the new row is written into it, not even the cells the peer left empty,
/// and each cell it does not already hold is stale.
/// NEGATIVE CONTROL: with `settle` ignoring whether the table holds the born row, the cells the
/// peer left empty are written into its row: `` left: 1 right: 0 ``.
#[test]
fn a_born_row_a_peer_made_meanwhile_is_written_all_or_nothing() {
    let mut book = Book::new();
    let cells = book.cells();
    let birth = registry_birth(&cells);
    let exported = export(&REGISTRY, &ADDRESS, &cells, &[]).unwrap();
    let mut text = add_row(&exported.text, 0);
    for (name, value) in [
        ("addr", "bob.ray@example.invalid"),
        ("domain", "example.invalid"),
        ("origin", "owner"),
    ] {
        text = type_in(&text, 1, name, value);
    }
    let plan = super::plan(&REGISTRY, &ADDRESS, &exported.base, &text, &birth).unwrap();
    let born = keys::address_pk("bob.ray@example.invalid").unwrap();
    assert_eq!(plan.born, std::slice::from_ref(&born));
    // A peer births the same address meanwhile, with another origin and no lifecycle yet.
    book.write(
        2,
        &[],
        vec![
            put(&ADDRESS, &born, "addr", json!("bob.ray@example.invalid")),
            put(&ADDRESS, &born, "origin", json!("harness")),
        ],
    );
    let settled = settle(&ADDRESS, &plan, &book.cells());
    assert_eq!(settled.rows.len(), 0);
    assert!(settled.born.is_empty());
    assert_eq!(settled.unchanged, 1);
    let mut stale: Vec<&str> = settled.stale.iter().map(|s| s.column.as_str()).collect();
    stale.sort_unstable();
    assert_eq!(stale, ["domain", "origin", "row"]);
}

/// A set cell writes only the members that changed: an added member as present, a removed one as
/// absent, each its own register; the members left alone are not written.
/// NEGATIVE CONTROL: with removals dropped from the set diff, the removed member is never written:
/// `` left: ["mc-0000000000000009.clis.agy"] right: ["mc-0000000000000009.clis.agy",
/// "mc-0000000000000009.clis.codex"] ``.
#[test]
fn a_set_cell_writes_only_the_members_that_changed() {
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    let cells = book.cells();
    let exported = export(&REGISTRY, &MACHINE, &cells, &[]).unwrap();
    assert!(exported.text.contains("\tclaude, codex\t"));
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "clis",
        "agy, claude",
    );
    let plan = super::plan(&REGISTRY, &MACHINE, &exported.base, &saved, &no_birth).unwrap();
    let edits = &plan.rows[0].1;
    let keys: Vec<&str> = edits.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(keys, [format!("{MC}.clis.agy"), format!("{MC}.clis.codex")]);
    assert_eq!(
        edits.iter().map(|e| e.x.clone()).collect::<Vec<_>>(),
        [json!(true), json!(false)]
    );
    assert!(edits[0].expected.is_empty() && edits[1].expected.len() == 1);
    book.write(
        1,
        &book.all(),
        settle(&MACHINE, &plan, &cells).rows.concat(),
    );
    assert_eq!(
        cell::project(&MACHINE, &book.cells()).rows[MC].fields["clis"].members(),
        ["agy", "claude"].into_iter().collect()
    );
}

crate::att_collection!(
    /// A row with two free-text sets.
    TEXT_SETS = Collection {
        name: "machine",
        version: 1,
        kind: crate::typed::Kind::Cells,
        writers: crate::typed::Writers::Owner,
        prefix: "mc",
        derive: crate::typed::Derive::BirthSha256,
        columns: &[
            Column::new("hosts", Ty::Set(&Ty::Text(253))),
            Column::new("nickname_aliases", Ty::Set(&Ty::Text(64))),
            Column::new("row", Ty::Enum(&["live", "retired"])),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

const SETS: Table = Table {
    application: "test",
    role: "sets",
    collections: &[&TEXT_SETS],
    self_quota: 0,
    ends: &[],
    faces: &[],
};

/// A text set one of whose members holds a comma (`Studio, upstairs`) shows as a JSON list, and
/// a save of that JSON list, one member added, writes only the added member; a set of plain
/// members stays comma-separated.
/// NEGATIVE CONTROL: with `show_field` joining a set's members with commas whatever they hold,
/// the save reads `Studio` and `upstairs` as two members: the `written` assertion fails.
#[test]
fn a_text_set_member_with_a_comma_shows_and_saves_as_a_json_list() {
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&TEXT_SETS, MC, "row", json!("live")),
            elem(&TEXT_SETS, MC, "hosts", "a.lan"),
            elem(&TEXT_SETS, MC, "hosts", "b.lan"),
            elem(&TEXT_SETS, MC, "nickname_aliases", "Studio, upstairs"),
            elem(&TEXT_SETS, MC, "nickname_aliases", "The Studio"),
        ],
    );
    let cells = book.cells();
    let exported = export(&SETS, &TEXT_SETS, &cells, &[]).unwrap();
    assert!(
        exported
            .text
            .contains(r#""[""Studio, upstairs"",""The Studio""]""#),
        "{}",
        exported.text
    );
    assert!(
        exported.text.contains("\na.lan, b.lan\t"),
        "{}",
        exported.text
    );
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "nickname_aliases",
        r#"["Studio, upstairs", "The Studio", "Upstairs Mac"]"#,
    );
    let plan = super::plan(&SETS, &TEXT_SETS, &exported.base, &saved, &no_birth).unwrap();
    let written: Vec<&str> = plan.rows[0].1.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(
        written,
        [format!(
            "{MC}.nickname_aliases.{}",
            keys::text_elem("Upstairs Mac")
        )]
    );
}

/// The baseline an export writes beside its file reads back as it was, candidates and set
/// elements included, and an account's grid keeps its JSON column read-only.
/// NEGATIVE CONTROL: with `to_json` leaving set elements out, the read-back baseline differs:
/// `` assertion `left == right` failed ``.
#[test]
fn the_baseline_reads_back_as_it_was_stored() {
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    let exported = export(&REGISTRY, &MACHINE, &book.cells(), &[]).unwrap();
    assert!(!exported.base.rows[0].1["clis"].elements.is_empty());
    assert_eq!(
        Base::from_json(&exported.base.to_json()).unwrap(),
        exported.base
    );
    let accounts = layout(&ACCOUNT, &Projection::default());
    let billing = accounts.iter().find(|c| c.name == "billing").unwrap();
    assert_eq!(billing.header(), "=billing");
}

/// A baseline says nothing of masking now that an address is ordinary data: `to_json` writes no
/// `reveal`, and `from_json` still reads the baseline of a build that masked addresses, its
/// `reveal` true or false, as the same baseline; a `reveal` that is no boolean is no baseline.
/// Such a build's masked export still saves: its cells are compared by what they showed, so the
/// file as exported asks for nothing.
/// NEGATIVE CONTROL: with `from_json` holding every baseline to the six fields `to_json` writes
/// (`exact(value, &FIELDS[..6])`), an older build's baseline is refused: `` reveal=false:
/// Error("not a grid baseline") ``.
#[test]
fn a_baseline_writes_no_reveal_and_an_older_builds_still_reads() {
    let mut book = Book::new();
    let pk = keys::address_pk("ann.lee@example.invalid").unwrap();
    book.write(
        1,
        &[],
        vec![
            put(&ADDRESS, &pk, "addr", json!("ann.lee@example.invalid")),
            put(&ADDRESS, &pk, "domain", json!("example.invalid")),
            put(&ADDRESS, &pk, "origin", json!("owner")),
            put(&ADDRESS, &pk, "row", json!("live")),
        ],
    );
    let exported = export(&REGISTRY, &ADDRESS, &book.cells(), &[]).unwrap();
    let stored = exported.base.to_json();
    assert!(stored.get("reveal").is_none(), "{stored}");
    assert_eq!(Base::from_json(&stored).unwrap(), exported.base);
    let with = |reveal: Value| {
        let mut older = stored.clone();
        older
            .as_object_mut()
            .unwrap()
            .insert("reveal".into(), reveal);
        older
    };
    for reveal in [false, true] {
        let read = Base::from_json(&with(json!(reveal)));
        assert_eq!(
            read.unwrap_or_else(|e| panic!("reveal={reveal}: {e:?}")),
            exported.base
        );
    }
    for reveal in [json!(1), json!("true"), Value::Null, json!([true])] {
        assert!(
            Base::from_json(&with(reveal.clone())).is_err(),
            "reveal={reveal}"
        );
    }
    // What a build that masked addresses exported: the mask in the file and in the baseline.
    let masked = |text: &str| text.replace("ann.lee@example.invalid", "an…@…");
    let older = masked(&aterm_json::to_string(&with(json!(false))).unwrap());
    let base = Base::from_json(&aterm_json::from_str::<Value>(&older).unwrap()).unwrap();
    let file = masked(&exported.text);
    assert!(file.contains("an…@…") && !file.contains("ann.lee"));
    let plan = super::plan(&REGISTRY, &ADDRESS, &base, &file, &no_birth).unwrap();
    assert!(plan.is_empty(), "{plan:?}");
}

/// Malformed baselines (empty headers, missing `=pk`, duplicate headers or rows, unknown fields)
/// are refused without panicking, and `--where` filters validate and canonicalize set elements.
/// NEGATIVE CONTROLS: without `header.last() == Some("=pk")`, `plan` panics on `width - 1` when
/// `header` is empty; without typed set element canonicalization, `Where::parse("aliases=M1")`
/// fails to match `"m1"`.
#[test]
fn malformed_baselines_and_set_filters_fail_closed_and_canonicalize() {
    let mut book = Book::new();
    let mut m = machine(MC);
    m.push(elem(&MACHINE, MC, "aliases", "m1"));
    book.write(1, &[], m);
    let cells = book.cells();
    let exported = export(&REGISTRY, &MACHINE, &cells, &[]).unwrap();

    let mut empty_header = exported.base.clone();
    empty_header.header.clear();
    assert!(
        super::plan(
            &REGISTRY,
            &MACHINE,
            &empty_header,
            &exported.text,
            &no_birth
        )
        .is_err()
    );

    let valid_json = exported.base.to_json();
    for header in [
        json!([]),
        json!(["label", "status"]),
        json!(["label", "=label", "=pk"]),
    ] {
        let mut bad = valid_json.clone();
        bad.as_object_mut().unwrap().insert("header".into(), header);
        assert!(Base::from_json(&bad).is_err());
    }

    let mut extra_field = valid_json.clone();
    extra_field
        .as_object_mut()
        .unwrap()
        .insert("extra".into(), json!(1));
    assert!(Base::from_json(&extra_field).is_err());

    let mut dup_row = valid_json;
    let first_row = dup_row["rows"][0].clone();
    dup_row
        .as_object_mut()
        .unwrap()
        .get_mut("rows")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(first_row);
    assert!(Base::from_json(&dup_row).is_err());

    assert!(Where::parse("=active").is_err());
    let proj = cell::project(&MACHINE, &cells);
    let row = &proj.rows[MC];
    assert!(
        Where::parse("aliases=M1")
            .unwrap()
            .matches(&MACHINE, MC, row)
            .unwrap()
    );
    assert!(
        !Where::parse("aliases=")
            .unwrap()
            .matches(&MACHINE, MC, row)
            .unwrap()
    );
    assert!(
        Where::parse("aliases=not-a-label")
            .unwrap()
            .matches(&MACHINE, MC, row)
            .is_err()
    );
}

/// The present members of `row`'s set column `col` of `collection` ([`cell::members`]).
fn members<'a>(collection: &Collection, row: &'a cell::Row, col: &str) -> BTreeSet<&'a str> {
    cell::members(collection.column(col).unwrap(), row.fields.get(col))
}

/// A change at `key` of `collection` storing `value` as it is (`None`: a revert's absence).
fn stored(collection: &Collection, key: String, value: Option<Value>) -> Change {
    Change {
        collection: collection.name.to_owned(),
        key,
        value,
    }
}

/// A set of words or keys reads as every earlier build reads it: an add beside a revert's null,
/// or beside a candidate this build cannot decode, is a member (the column read-only), and a
/// text alone at an element, even one its key digests, is none. A text set's element holding an
/// add and a revert's null is a member and its column stays editable; an element beside a
/// candidate it cannot decode never takes its member, its column shown raw.
/// NEGATIVE CONTROLS: with `cell::project` skipping such an element under every set (no
/// `text_set` gate), the reverted `claude` leaves the set of keys: the first `clis` members
/// assertion fails; with `check_elem_value` refusing a text set's null, `hosts` turns raw: the
/// first `hosts` assertion fails; with `cell::members` reading a set of keys as a text set,
/// `gemini` joins `clis`: the third `clis` members assertion fails.
#[test]
fn a_set_element_beside_a_null_or_unreadable_candidate_reads_as_before() {
    let key = |col: &str, elem: &str| format!("{MC}.{col}.{elem}");
    let undecodable = json!({"r": 1, "x": true, "z": 1});

    // A revert's null concurrent with each add.
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    book.write(2, &[], vec![stored(&MACHINE, key("clis", "claude"), None)]);
    let row = cell::project(&MACHINE, &book.cells())
        .rows
        .remove(MC)
        .unwrap();
    assert!(row.raw.contains("clis"));
    assert_eq!(
        members(&MACHINE, &row, "clis"),
        BTreeSet::from(["claude", "codex"])
    );

    // A candidate no build of this version decodes, beside an add.
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    book.write(
        2,
        &[],
        vec![stored(
            &MACHINE,
            key("clis", "codex"),
            Some(undecodable.clone()),
        )],
    );
    let row = cell::project(&MACHINE, &book.cells())
        .rows
        .remove(MC)
        .unwrap();
    assert!(row.raw.contains("clis"));
    assert_eq!(
        members(&MACHINE, &row, "clis"),
        BTreeSet::from(["claude", "codex"])
    );
    let gemini = keys::text_elem("gemini");
    book.write(
        3,
        &[],
        vec![stored(
            &MACHINE,
            key("clis", &gemini),
            Some(CellValue::owner(1, json!("gemini")).encode()),
        )],
    );
    let row = cell::project(&MACHINE, &book.cells())
        .rows
        .remove(MC)
        .unwrap();
    assert_eq!(
        members(&MACHINE, &row, "clis"),
        BTreeSet::from(["claude", "codex"])
    );

    // A text set: the null beside an add keeps the member; the unreadable element goes.
    let a = keys::text_elem("a.lan");
    let b = keys::text_elem("b.lan");
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&TEXT_SETS, MC, "row", json!("live")),
            elem(&TEXT_SETS, MC, "hosts", "a.lan"),
        ],
    );
    book.write(2, &[], vec![stored(&TEXT_SETS, key("hosts", &a), None)]);
    let row = cell::project(&TEXT_SETS, &book.cells())
        .rows
        .remove(MC)
        .unwrap();
    assert!(!row.raw.contains("hosts"), "{:?}", row.raw);
    assert_eq!(
        members(&TEXT_SETS, &row, "hosts"),
        BTreeSet::from(["a.lan"])
    );
    book.write(1, &[], vec![elem(&TEXT_SETS, MC, "hosts", "b.lan")]);
    book.write(
        3,
        &[],
        vec![stored(&TEXT_SETS, key("hosts", &b), Some(undecodable))],
    );
    let row = cell::project(&TEXT_SETS, &book.cells())
        .rows
        .remove(MC)
        .unwrap();
    assert!(row.raw.contains("hosts"));
    assert_eq!(
        members(&TEXT_SETS, &row, "hosts"),
        BTreeSet::from(["a.lan"])
    );
}

/// A text set member that starts as a conflict does (`⚠ check`) shows the set as a JSON list,
/// so the shown set reads back as its members: clearing the cell removes that member's own
/// register, and a set shown as it is is never read as a reopened conflict.
/// NEGATIVE CONTROLS: with `show_members` not listing a `⚠ ` member as JSON, the export shows
/// it plainly: the `shown` assertion fails (the removal still holds: the strip takes `⚠ ` only
/// from a reopened `mine ‖ theirs`); with that and the strip taking any `⚠ ` prefix (as both
/// did), clearing it writes a removal of `check`, a register that never held a member: the
/// `written` assertion fails.
#[test]
fn a_text_set_member_that_starts_as_a_conflict_does_reads_back_as_itself() {
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&TEXT_SETS, MC, "row", json!("live")),
            elem(&TEXT_SETS, MC, "nickname_aliases", "⚠ check"),
        ],
    );
    let exported = export(&SETS, &TEXT_SETS, &book.cells(), &[]).unwrap();
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "nickname_aliases",
        "",
    );
    let plan = super::plan(&SETS, &TEXT_SETS, &exported.base, &saved, &no_birth).unwrap();
    let written: Vec<&str> = plan.rows[0].1.iter().map(|e| e.key.as_str()).collect();
    assert_eq!(
        written,
        [format!(
            "{MC}.nickname_aliases.{}",
            keys::text_elem("⚠ check")
        )]
    );
    assert!(
        exported.text.contains(r#""[""⚠ check""]""#),
        "shown: {}",
        exported.text
    );
}

/// A save's add settles against what each candidate adds as a projection reads it: a string a
/// text set's key does not digest, or one under a set of keys, adds nothing, so an owner's add
/// beside one is stale (reopened for the owner), never counted as already held and dropped; so
/// is an add, or a removal, of a text set's member whose element also holds a candidate no build
/// of this version decodes, which a projection shows raw, confirming neither.
/// NEGATIVE CONTROLS: with `settle` counting any string as an add (as it did), the first two
/// adds count as unchanged: their `stale` assertions fail; with `settle` reading an undecodable
/// candidate as an absence, the third add counts as unchanged: its assertion fails; with
/// `cell::element_holds` reading such an element as absent (as it did), the removal counts as
/// unchanged and is lost: the last assertion fails.
#[test]
fn a_save_settles_a_set_add_against_what_the_projection_reads() {
    // A text set.
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&TEXT_SETS, MC, "row", json!("live")),
            elem(&TEXT_SETS, MC, "hosts", "a.lan"),
        ],
    );
    let exported = export(&SETS, &TEXT_SETS, &book.cells(), &[]).unwrap();
    book.write(
        2,
        &[],
        vec![stored(
            &TEXT_SETS,
            format!("{MC}.hosts.{}", keys::text_elem("c.lan")),
            Some(CellValue::owner(1, json!("b.lan")).encode()),
        )],
    );
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "hosts",
        "a.lan, c.lan",
    );
    let plan = super::plan(&SETS, &TEXT_SETS, &exported.base, &saved, &no_birth).unwrap();
    let settled = settle(&TEXT_SETS, &plan, &book.cells());
    assert_eq!((settled.unchanged, settled.stale.len()), (0, 1));

    // A set of keys.
    let mut book = Book::new();
    book.write(1, &[], machine(MC));
    let exported = export(&REGISTRY, &MACHINE, &book.cells(), &[]).unwrap();
    book.write(
        2,
        &[],
        vec![stored(
            &MACHINE,
            format!("{MC}.clis.gemini"),
            Some(CellValue::owner(1, json!("gemini")).encode()),
        )],
    );
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "clis",
        "claude, codex, gemini",
    );
    let plan = super::plan(&REGISTRY, &MACHINE, &exported.base, &saved, &no_birth).unwrap();
    let settled = settle(&MACHINE, &plan, &book.cells());
    assert_eq!((settled.unchanged, settled.stale.len()), (0, 1));

    // A text set's element a peer adds beside a candidate this build cannot decode.
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&TEXT_SETS, MC, "row", json!("live")),
            elem(&TEXT_SETS, MC, "hosts", "a.lan"),
        ],
    );
    let exported = export(&SETS, &TEXT_SETS, &book.cells(), &[]).unwrap();
    book.write(2, &[], vec![elem(&TEXT_SETS, MC, "hosts", "c.lan")]);
    book.write(
        3,
        &[],
        vec![stored(
            &TEXT_SETS,
            format!("{MC}.hosts.{}", keys::text_elem("c.lan")),
            Some(json!({"r": 1, "x": "c.lan", "z": 1})),
        )],
    );
    let saved = type_in(
        &exported.text,
        record(&exported.text, MC),
        "hosts",
        "a.lan, c.lan",
    );
    let plan = super::plan(&SETS, &TEXT_SETS, &exported.base, &saved, &no_birth).unwrap();
    let settled = settle(&TEXT_SETS, &plan, &book.cells());
    assert_eq!((settled.unchanged, settled.stale.len()), (0, 1));

    // The owner removes a text set's member while a peer stores, at its element, a candidate
    // this build cannot decode.
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&TEXT_SETS, MC, "row", json!("live")),
            elem(&TEXT_SETS, MC, "hosts", "a.lan"),
            elem(&TEXT_SETS, MC, "hosts", "c.lan"),
        ],
    );
    let exported = export(&SETS, &TEXT_SETS, &book.cells(), &[]).unwrap();
    book.write(
        2,
        &[],
        vec![stored(
            &TEXT_SETS,
            format!("{MC}.hosts.{}", keys::text_elem("c.lan")),
            Some(json!({"r": 1, "x": "c.lan", "z": 1})),
        )],
    );
    let saved = type_in(&exported.text, record(&exported.text, MC), "hosts", "a.lan");
    let plan = super::plan(&SETS, &TEXT_SETS, &exported.base, &saved, &no_birth).unwrap();
    assert_eq!(plan.rows[0].1[0].x, Value::Bool(false));
    let settled = settle(&TEXT_SETS, &plan, &book.cells());
    assert_eq!((settled.unchanged, settled.stale.len()), (0, 1));
}

/// A text set shown raw (one of its elements unreadable) lists its members as a JSON list when
/// one holds a comma, as its editable form does, so `Paris, France` never reads as two.
/// NEGATIVE CONTROL: with `show_raw` joining a set's members with commas (as it did), the
/// export shows `Paris, France, Rome`: the assertion fails.
#[test]
fn a_raw_text_set_shows_a_member_with_a_comma_as_one() {
    let mut book = Book::new();
    book.write(
        1,
        &[],
        vec![
            put(&TEXT_SETS, MC, "row", json!("live")),
            elem(&TEXT_SETS, MC, "nickname_aliases", "Paris, France"),
            elem(&TEXT_SETS, MC, "nickname_aliases", "Rome"),
            stored(
                &TEXT_SETS,
                format!("{MC}.nickname_aliases.{}", keys::text_elem("x")),
                Some(CellValue::owner(1, json!("y")).encode()),
            ),
        ],
    );
    let cells = book.cells();
    let row = cell::project(&TEXT_SETS, &cells).rows.remove(MC).unwrap();
    assert!(row.raw.contains("nickname_aliases"));
    let exported = export(&SETS, &TEXT_SETS, &cells, &[]).unwrap();
    assert!(
        exported.text.contains(r#""[""Paris, France"",""Rome""]""#),
        "{}",
        exported.text
    );
}

/// A version-1 account label outside the handle grammar (`Team A`: version 1's label was free
/// text) reads raw under version 2's `Handle`, and stays the owner's to replace: the export shows
/// it raw but not read-only, and a save that types a handle over it writes that handle over the
/// candidate the export showed. A label a newer build wrote (at a version above this build's)
/// stays read-only, and the save refuses an edit of it.
/// NEGATIVE CONTROL: with `replaceable` answering `false`, the export marks the v1 label
/// read-only: `` assertion failed: !seen(&ann).raw ``.
#[test]
fn an_older_builds_label_the_handle_type_refuses_is_the_owners_to_replace() {
    let ann = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let bob = keys::account_pk("claude", "bob@example.invalid").unwrap();
    let birth = |pk: &str, login: &str| {
        vec![
            put(&ACCOUNT, pk, "provider", json!("anthropic")),
            put(&ACCOUNT, pk, "realm", json!("claude")),
            put(
                &ACCOUNT,
                pk,
                "login",
                json!(keys::address_pk(login).unwrap()),
            ),
            put(&ACCOUNT, pk, "status", json!("active")),
            put(&ACCOUNT, pk, "row", json!("live")),
        ]
    };
    // As the build that wrote it stored it, unchecked by this build's type.
    let label_at = |pk: &str, r: u32, text: &str| Change {
        collection: ACCOUNT.name.to_owned(),
        key: format!("{pk}.label"),
        value: Some(CellValue::owner(r, json!(text)).encode()),
    };
    let mut book = Book::new();
    let mut changes = birth(&ann, "ann@example.invalid");
    changes.extend(birth(&bob, "bob@example.invalid"));
    changes.push(label_at(&ann, 1, "Team A"));
    changes.push(label_at(&bob, ACCOUNT.version + 1, "Team B"));
    book.write(1, &[], changes);
    let cells = book.cells();
    let projection = cell::project(&ACCOUNT, &cells);
    for pk in [&ann, &bob] {
        assert!(projection.rows[pk].raw.contains("label"), "{pk}");
    }
    let exported = export(&REGISTRY, &ACCOUNT, &cells, &[]).unwrap();
    let seen = |pk: &str| {
        exported
            .base
            .rows
            .iter()
            .find(|(row, _)| row == pk)
            .map(|(_, seen)| seen["label"].clone())
            .unwrap()
    };
    assert_eq!(seen(&ann).shown, "\"Team A\"");
    assert!(!seen(&ann).raw);
    assert!(seen(&bob).raw);
    let saved = type_in(
        &exported.text,
        record(&exported.text, &ann),
        "label",
        "team-a",
    );
    let plan = super::plan(&REGISTRY, &ACCOUNT, &exported.base, &saved, &no_birth).unwrap();
    let edits: Vec<(&str, &str, &Value, &[Digest])> = plan
        .rows
        .iter()
        .flat_map(|(_, edits)| edits)
        .map(|edit| {
            (
                edit.pk.as_str(),
                edit.column.as_str(),
                &edit.x,
                edit.expected.as_slice(),
            )
        })
        .collect();
    assert_eq!(
        edits,
        [(
            ann.as_str(),
            "label",
            &json!("team-a"),
            seen(&ann).candidates.as_slice()
        )]
    );
    let saved = type_in(
        &exported.text,
        record(&exported.text, &bob),
        "label",
        "team-b",
    );
    let problems = super::plan(&REGISTRY, &ACCOUNT, &exported.base, &saved, &no_birth).unwrap_err();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].rule.contains("newer build"), "{problems:?}");
}

/// A day shows as its date and reads back from it or from its number, on every surface that
/// shows a cell; a date that names no day from 1970 through 9999 is refused by the date rule,
/// and a day past 9999 shows as its number, never as a date made up past the calendar's end.
/// NEGATIVE CONTROL: with `read` parsing a day as a whole number only (its `date_day` dropped),
/// `2026-11-14` is refused: `` left: Err("this column holds a date, as 2026-11-14") right:
/// Ok(Number(Number(Unsigned(20771)))) ``.
#[test]
fn a_day_shows_as_its_date_and_reads_back_from_it() {
    let day = &Ty::Day;
    assert_eq!(show(day, &json!(20_771)), "2026-11-14");
    assert_eq!(show(day, &json!(0)), "1970-01-01");
    assert_eq!(read(day, "2026-11-14"), Ok(json!(20_771)));
    assert_eq!(read(day, " 20771 "), Ok(json!(20_771)));
    let last = read(day, "9999-12-31").unwrap();
    assert_eq!(show(day, &last), "9999-12-31");
    let past = last.as_u64().unwrap() + 1;
    assert_eq!(show(day, &json!(past)), past.to_string());
    assert_eq!(show(day, &json!(u64::MAX)), u64::MAX.to_string());
    assert_eq!(read(day, &past.to_string()), Ok(json!(past)));
    for days in (0..=last.as_u64().unwrap()).step_by(997) {
        assert_eq!(read(day, &show(day, &json!(days))), Ok(json!(days)));
    }
    for none in [
        "2026-02-30",
        "2026-13-01",
        "11/14/2026",
        "1969-12-31",
        "10000-01-01",
        "9000000000000000000-01-01",
        "2026-1-14",
        "+026-11-14",
    ] {
        assert_eq!(
            read(day, none),
            Err("this column holds a date, as 2026-11-14"),
            "{none}"
        );
    }
}

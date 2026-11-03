// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ATT/1 tests. The pure ones build causal histories by hand (an astream-wire `Index` and the
//! Commons projection kernel, `documents::apply`, exactly as `Log` applies them); the Commons
//! ones run real two-Mac labs. Every address is synthetic (`example.invalid`).

use super::{
    cell::{self, CellKey, CellValue, Cells, Field, ImportStep},
    check::{self, Finding},
    guard, keys,
    owned::{self, Op, SchemaState},
    *,
};
use crate::database::Change;
use astream_wire::sync::{Actor, Digest, Header, Index};
use aterm_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const LIFE: &[&str] = &["live", "retired"];
const STATUS: &[&str] = &["pending", "active", "paused", "broken", "banned"];
const SAFE_FIRST: &[&str] = &["banned", "broken", "paused", "pending", "active"];

crate::att_collection!(
    pub(super) ACCOUNT = Collection {
        name: "account",
        version: 2,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ac",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("login", Ty::Ref("address")),
            Column::new("status", Ty::Enum(STATUS)).on_conflict(OnConflict::Prefer(SAFE_FIRST)),
            Column::new("plan", Ty::Ref("plan")).null(),
            Column::new(
                "services",
                Ty::Set(&Ty::Enum(&["codex", "chatgpt", "claude-code"]))
            ),
            Column::new("note", Ty::Text(512)).null(),
            Column::new("units", Ty::Uint)
                .null()
                .on_conflict(OnConflict::Max),
            Column::new("hold", Ty::Uint)
                .null()
                .on_conflict(OnConflict::Hold),
            Column::new("vault_ref", Ty::SecretRef).null(),
            Column::new("src", Ty::SrcRef).null(),
            Column::new("region", Ty::Enum(&["global", "mainland-cn"]))
                .null()
                .since(2),
            Column::new("legacy", Ty::Text(64)).null().retired(2),
            Column::new("row", Ty::Enum(LIFE)),
        ],
        unique: &[],
        max_row: 0,
    }
);

crate::att_collection!(
    /// The same collection as a build from before version 2 compiled it.
    pub(super) ACCOUNT_V1 = Collection {
    name: "account",
    version: 1,
    kind: Kind::Cells,
    writers: Writers::Owner,
    prefix: "ac",
    derive: Derive::BirthSha256,
    columns: &[
        Column::new("login", Ty::Ref("address")),
        Column::new("status", Ty::Enum(&["pending", "active", "paused", "broken"])),
        Column::new("plan", Ty::Ref("plan")).null(),
        Column::new("services", Ty::Set(&Ty::Enum(&["codex", "chatgpt", "claude-code"]))),
        Column::new("note", Ty::Text(512)).null(),
        Column::new("units", Ty::Uint).null(),
        Column::new("hold", Ty::Uint).null(),
        Column::new("vault_ref", Ty::SecretRef).null(),
        Column::new("src", Ty::SrcRef).null(),
        Column::new("legacy", Ty::Text(64)).null(),
        Column::new("row", Ty::Enum(LIFE)),
    ],
    unique: &[],
    max_row: 0,
});

crate::att_collection!(
    pub(super) ADDRESS = Collection {
        name: "address",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ad",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("addr", Ty::Email),
            Column::new("row", Ty::Enum(LIFE)),
        ],
        unique: &[&["addr"]],
        max_row: 0,
    }
);

crate::att_collection!(
    pub(super) FACTS = Collection {
        name: "facts",
        version: 1,
        kind: Kind::Owned,
        writers: Writers::SelfKey,
        prefix: "",
        derive: Derive::SignerUuid,
        columns: &[Column::new("n", Ty::Uint), Column::new("label", Ty::Label)],
        unique: &[],
        max_row: 4096,
    }
);

crate::att_collection!(
    OBS = Collection {
        name: "obs",
        version: 1,
        kind: Kind::Owned,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[Column::new("n", Ty::Uint)],
        unique: &[],
        max_row: 4096,
    }
);

crate::att_collection!(
    EV = Collection {
        name: "ev",
        version: 1,
        kind: Kind::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[Column::new("events", Ty::Json(24 * 1024))],
        unique: &[],
        max_row: 24 * 1024,
    }
);

pub(super) const TABLE: Table = Table {
    application: "att-test",
    role: "registry",
    collections: &[&ACCOUNT, &ADDRESS, &FACTS, &OBS],
    self_quota: 2,
    ends: &[],
};

/// A hand-built causal history: each write names the writes it observed.
#[derive(Default)]
pub(super) struct Book {
    index: Option<Index>,
    next: u64,
    pub(super) heads: BTreeMap<u8, (u64, Digest)>,
    ops: Vec<(Digest, u8, u64, Vec<Change>)>,
}

pub(super) fn actor(n: u8) -> Actor {
    [n; 48]
}

impl Book {
    pub(super) fn new() -> Self {
        Self {
            index: Some(Index::new(1024)),
            ..Self::default()
        }
    }
    /// Append `changes` by `writer`, observing its own previous write and `seen`.
    pub(super) fn write(&mut self, writer: u8, seen: &[Digest], changes: Vec<Change>) -> Digest {
        self.next += 1;
        let mut id = [0; 32];
        id[..8].copy_from_slice(&self.next.to_be_bytes());
        let previous = self.heads.get(&writer).map(|(_, id)| *id);
        let sequence = self.heads.get(&writer).map_or(1, |(s, _)| s + 1);
        let mut parents: BTreeSet<Digest> = seen.iter().copied().collect();
        parents.extend(previous);
        self.index
            .as_mut()
            .unwrap()
            .insert(Header {
                id,
                actor: actor(writer),
                sequence,
                previous,
                parents: parents.into_iter().collect(),
            })
            .unwrap();
        self.heads.insert(writer, (sequence, id));
        self.ops.push((id, writer, sequence, changes));
        id
    }
    fn cells_without(&self, skip: &BTreeSet<Digest>) -> Cells {
        let index = self.index.as_ref().unwrap();
        let mut cells = Cells::new();
        for id in index.project().ordered {
            if skip.contains(&id) {
                continue;
            }
            let (_, _, _, changes) = self.ops.iter().find(|op| op.0 == id).unwrap();
            crate::documents::apply(index, &mut cells, id, changes, true);
        }
        cells
    }
    pub(super) fn cells(&self) -> Cells {
        self.cells_without(&BTreeSet::new())
    }
    fn ops(&self, period: u64) -> Vec<Op> {
        self.ops
            .iter()
            .map(|(id, writer, sequence, changes)| Op {
                id: *id,
                actor: actor(*writer),
                sequence: *sequence,
                roster: [7; 32],
                period,
                revert: false,
                changes: changes.clone(),
            })
            .collect()
    }
}

pub(super) fn put(collection: &Collection, pk: &str, col: &str, x: Value) -> Change {
    cell::change(
        collection,
        CellKey {
            pk,
            col,
            elem: None,
        },
        &CellValue::owner(collection.version, x),
    )
    .unwrap()
}

pub(super) fn elem(
    collection: &Collection,
    pk: &str,
    col: &str,
    elem: &str,
    present: bool,
) -> Change {
    cell::change(
        collection,
        CellKey {
            pk,
            col,
            elem: Some(elem),
        },
        &CellValue::owner(collection.version, Value::Bool(present)),
    )
    .unwrap()
}

pub(super) const PK: &str = "ac-0123456789abcdef";

/// NEGATIVE CONTROL: with `CellKey::parse` accepting a fourth part (its `parts.next().is_some()`
/// check removed), `a.b.c.d` parses and this fails.
#[test]
fn cell_keys_fit_the_grammar_and_never_hold_an_address() {
    let key = CellKey {
        pk: PK,
        col: "services",
        elem: Some("codex"),
    };
    let stored = key.encode().unwrap();
    assert_eq!(stored, "ac-0123456789abcdef.services.codex");
    assert_eq!(CellKey::parse(&stored).unwrap(), key);
    for bad in [
        "a.b.c.d",
        "pk",
        "pk.",
        ".col",
        "pk.Col",
        "who@example.invalid.col",
        "pk.col.who@example.invalid",
    ] {
        assert!(CellKey::parse(bad).is_err(), "{bad}");
    }
    let long = CellKey {
        pk: "p".repeat(26).leak(),
        col: "c".repeat(20).leak(),
        elem: Some("e".repeat(20).leak()),
    };
    assert!(long.encode().is_err(), "a key past 64 bytes is refused");
    assert!(
        CellKey {
            pk: "who@example.invalid",
            col: "addr",
            elem: None
        }
        .encode()
        .is_err()
    );
}

/// Two concurrent edits to different columns of one row both survive; to one column they leave
/// two candidates; an edit that observed both resolves them, one that saw only one does not.
/// NEGATIVE CONTROL: a whole-row key (`cell::change` writing `key.pk` instead of the cell key)
/// puts both columns in one register that no reader parses: the row loses its columns and this
/// fails.
#[test]
fn concurrent_edits_to_different_columns_both_survive() {
    let mut book = Book::new();
    let birth = book.write(
        1,
        &[],
        vec![
            put(&ACCOUNT, PK, "login", json!("ad-0000000000000001")),
            put(&ACCOUNT, PK, "status", json!("pending")),
            put(&ACCOUNT, PK, "row", json!("live")),
        ],
    );
    let a = book.write(
        1,
        &[birth],
        vec![put(&ACCOUNT, PK, "status", json!("active"))],
    );
    let b = book.write(
        2,
        &[birth],
        vec![put(&ACCOUNT, PK, "plan", json!("anthropic-max20"))],
    );
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert_eq!(row.get("status"), Some(&json!("active")));
    assert_eq!(row.get("plan"), Some(&json!("anthropic-max20")));
    assert_eq!(row.get("login"), Some(&json!("ad-0000000000000001")));
    assert!(row.fields.values().all(|f| !f.conflicted()));

    let c = book.write(2, &[a], vec![put(&ACCOUNT, PK, "status", json!("paused"))]);
    let d = book.write(1, &[b], vec![put(&ACCOUNT, PK, "status", json!("broken"))]);
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    let Some(Field::Conflict { candidates, .. }) = row.fields.get("status") else {
        panic!("concurrent edits to one column conflict");
    };
    assert_eq!(candidates.len(), 2);
    // Saw only one of the two: the other stays.
    book.write(3, &[c], vec![put(&ACCOUNT, PK, "status", json!("active"))]);
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert!(row.fields["status"].conflicted());
    // Saw both: resolved.
    let e = book.heads[&3].1;
    book.write(
        3,
        &[c, d, e],
        vec![put(&ACCOUNT, PK, "status", json!("active"))],
    );
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert_eq!(row.fields["status"].value(), Some(&json!("active")));
    assert!(!row.fields["status"].conflicted());
}

/// Equal concurrent values are one value; a real conflict shows the column rule's provisional
/// value: the safe status first, the larger number, nothing for `hold`.
/// NEGATIVE CONTROL: with the collapse in `cell::field` removed (every candidate kept), two
/// equal plans read as a conflict and this fails.
#[test]
fn equal_values_collapse_and_conflicts_follow_the_column_rule() {
    let mut book = Book::new();
    let birth = book.write(1, &[], vec![put(&ACCOUNT, PK, "row", json!("live"))]);
    for (writer, status, units, hold) in [(1, "active", 2, 5), (2, "banned", 3, 6)] {
        book.write(
            writer,
            &[birth],
            vec![
                put(&ACCOUNT, PK, "plan", json!("anthropic-max20")),
                put(&ACCOUNT, PK, "status", json!(status)),
                put(&ACCOUNT, PK, "units", json!(units)),
                put(&ACCOUNT, PK, "hold", json!(hold)),
            ],
        );
    }
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert!(!row.fields["plan"].conflicted(), "equal values collapse");
    assert_eq!(row.get("plan"), Some(&json!("anthropic-max20")));
    assert!(row.fields["status"].conflicted());
    assert_eq!(row.get("status"), Some(&json!("banned")));
    assert_eq!(row.get("units"), Some(&json!(3)));
    assert!(row.fields["hold"].conflicted());
    assert_eq!(row.get("hold"), None);
}

/// A concurrent add and remove of one set element read present; a remove that observed the
/// add reads absent.
/// NEGATIVE CONTROL: with `Field::present` requiring every candidate to add (`all` for `any`),
/// the concurrent add is lost and this fails.
#[test]
fn set_elements_are_add_wins() {
    let mut book = Book::new();
    let birth = book.write(1, &[], vec![elem(&ACCOUNT, PK, "services", "codex", true)]);
    let add = book.write(
        1,
        &[birth],
        vec![elem(&ACCOUNT, PK, "services", "chatgpt", true)],
    );
    book.write(
        2,
        &[birth],
        vec![elem(&ACCOUNT, PK, "services", "chatgpt", false)],
    );
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert_eq!(
        row.fields["services"].members(),
        BTreeSet::from(["chatgpt", "codex"])
    );
    let seen = book.heads[&2].1;
    book.write(
        3,
        &[add, seen],
        vec![elem(&ACCOUNT, PK, "services", "chatgpt", false)],
    );
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert_eq!(row.fields["services"].members(), BTreeSet::from(["codex"]));
}

/// A version-2 column written by a new build survives an old build's edit to another column
/// of the row; the old build shows the newer column and an unknown word raw (read-only), and
/// a retired column can no longer be written.
/// NEGATIVE CONTROL: with `cell::project` skipping columns its schema does not know, the old
/// build no longer lists `region` as raw and this fails.
#[test]
fn an_older_build_edits_its_columns_without_clobbering_newer_ones() {
    let mut book = Book::new();
    let birth = book.write(
        1,
        &[],
        vec![
            put(&ACCOUNT, PK, "status", json!("active")),
            put(&ACCOUNT, PK, "region", json!("mainland-cn")),
            put(&ACCOUNT, PK, "row", json!("live")),
        ],
    );
    let old = book.write(
        2,
        &[birth],
        vec![put(&ACCOUNT_V1, PK, "status", json!("paused"))],
    );
    book.write(
        1,
        &[old],
        vec![put(&ACCOUNT, PK, "status", json!("banned"))],
    );
    let new = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert_eq!(new.get("region"), Some(&json!("mainland-cn")));
    assert_eq!(new.get("status"), Some(&json!("banned")));
    assert!(new.raw.is_empty());
    let older = cell::project(&ACCOUNT_V1, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    assert_eq!(older.get("region"), Some(&json!("mainland-cn")));
    assert_eq!(
        older.raw,
        BTreeSet::from(["region".to_owned(), "status".to_owned()])
    );
    let retired = cell::change(
        &ACCOUNT,
        CellKey {
            pk: PK,
            col: "legacy",
            elem: None,
        },
        &CellValue::owner(2, json!("m8-2")),
    );
    assert!(retired.is_err());
}

/// Free text cannot carry an address, a credential shape or a plain-word password, a vault
/// reference holds only names, and no refusal quotes the value.
/// NEGATIVE CONTROL: with `guard::text` skipping `names_secret`, `password hunter2` is
/// accepted and this fails.
#[test]
fn values_refuse_secrets_and_addresses_without_quoting_them() {
    let note = |text: &str| {
        cell::change(
            &ACCOUNT,
            CellKey {
                pk: PK,
                col: "note",
                elem: None,
            },
            &CellValue::owner(2, json!(text)),
        )
    };
    assert!(note("renews on the 3rd; shared with the desk").is_ok());
    for secret in [
        "password hunter2",
        "PW: correct-horse",
        "reach me at who@example.invalid",
        // In halves, so the export guard never reads a token here.
        concat!("gh", "p_aaaaaaaaaaaaaaaaaaaaaaaa"),
        "key sk-ant-xyz",
        "otpauth://totp/x?secret=AAAA",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        let why = note(secret).unwrap_err().0;
        assert!(why.starts_with("account.note: "), "{why}");
        for word in secret.split_whitespace().filter(|word| word.len() >= 4) {
            assert!(!why.contains(word), "the refusal quotes {word}");
        }
    }
    assert!(!guard::credential_shaped("a risk-free task-list"));
    let reference = |x: Value| {
        cell::change(
            &ACCOUNT,
            CellKey {
                pk: PK,
                col: "vault_ref",
                elem: None,
            },
            &CellValue::owner(2, x),
        )
    };
    assert!(
        reference(json!({"vault": "alab", "entry": "ai-mailbox.toml", "item": "mailbox-1"}))
            .is_ok()
    );
    assert!(reference(json!({"vault": "alab", "entry": "x", "value": "hunter2"})).is_err());
    assert!(reference(json!({"vault": "alab", "entry": "has space"})).is_err());
    let src = cell::change(
        &ACCOUNT,
        CellKey {
            pk: PK,
            col: "src",
            elem: None,
        },
        &CellValue::owner(
            2,
            json!({"source": "sr-0011223344556677", "row": 4, "col": 9}),
        ),
    );
    assert!(src.is_ok());
    let copied = cell::change(
        &ACCOUNT,
        CellKey {
            pk: PK,
            col: "src",
            elem: None,
        },
        &CellValue::owner(
            2,
            json!({"source": "sr-0011223344556677", "row": 4, "col": 9, "text": "PRIVATE-CELL"}),
        ),
    );
    assert!(
        copied.is_err(),
        "a source coordinate never carries the cell's text"
    );
}

/// The key shapes of the lab's providers are refused, each in halves here so the export guard
/// never reads a token: a Google API key (39 characters, one under the long-run rule), an xAI
/// key, a Google OAuth access token and a base32 TOTP seed, bare or in groups of four; plain
/// capitals, `xai-web`, a hex digest and ordinary words stay.
/// NEGATIVE CONTROLS: with `KEYED` emptied, the Google key passes: `` google key `` (the
/// assertion's message); with `credential_shaped` skipping `base32_seed`, the grouped seed
/// passes: `` grouped seed ``; without the letter-past-`f` condition, a hex digest reads as a
/// seed: `` digest deadbeefcafe2345abcd ``.
#[test]
fn every_providers_key_shape_is_refused() {
    for (what, key) in [
        (
            "google key",
            concat!("AI", "zaSyA1b2C3d4E5f6G7h8I9j0KlMnOpQrStUVw"),
        ),
        ("xai key", concat!("xa", "i-0aB1cD2eF3gH4iJ5kL6mN7oP8qR9sT")),
        (
            "oauth token",
            concat!("ya", "29.a0AfH6SMBx1y2z3w4v5u6t7s8r9q"),
        ),
        ("grouped seed", concat!("JBSW Y3DP ", "EHPK 3PXP")),
        ("lower seed", concat!("jbsw y3dp ", "ehpk 3pxp")),
        ("bare seed", concat!("seed JBSWY3DP", "EHPK3PXP")),
    ] {
        assert!(guard::credential_shaped(key), "{what}");
        assert!(guard::text(key, 512).is_err(), "{what}");
    }
    for fine in [
        "PLEASE DO NOT USE THIS ACCOUNT",
        "moved to xai-web on m27",
        "AIzawa is the desk name",
        "plan ANTHROPIC MAX FIVE TIMES",
        "they will be back soon",
        "codes come by email to the hub",
        "digest deadbeefcafe2345abcd",
    ] {
        assert!(!guard::credential_shaped(fine), "{fine}");
    }
}

/// A full phone number never lands in a cell: free text and bounded JSON refuse a run of seven or
/// more digits (spaced, dashed, dotted or bracketed, with or without a leading `+`) without
/// quoting it, while dates, versions, an IPv4 host and digits inside a word stay; a phone row
/// keeps exactly its last four digits.
/// NEGATIVE CONTROLS: with `guard::text` skipping `phone_shaped`, `SMS +1 415 555 0101` is
/// accepted: `` called `Result::unwrap_err()` on an `Ok` value: Change { … } ``; with the
/// `Ty::Digits` check in `cell::check` answering `Ok(())`, `01a1` is a last four: ``
/// assertion failed: last4("01a1").is_err() ``.
#[test]
fn phone_numbers_stay_out_of_every_cell() {
    let note = |text: &str| {
        cell::change(
            &ACCOUNT,
            CellKey {
                pk: PK,
                col: "note",
                elem: None,
            },
            &CellValue::owner(2, json!(text)),
        )
    };
    for phone in [
        "SMS +1 415 555 0101",
        "(415) 555-0101 for codes",
        "call 555-0101 first",
        "+44 20 7946 0958",
        "4155550101",
        "renews 2026-11-01, codes to 415.555.0101",
    ] {
        let why = note(phone).unwrap_err().0;
        assert!(why.starts_with("account.note: "), "{why}");
        assert!(!why.contains("555") && !why.contains("7946"), "{why}");
    }
    for fine in [
        "renews 2026-11-01",
        "renews 2026-11-01 21:52",
        "host 192.168.1.20",
        "MacBook Pro (14-inch, 2024)",
        "moved from m27-2 on 0.147.0",
        "from sr-ee08c551e4729096",
        "1,000,000 tokens a week",
    ] {
        assert!(note(fine).is_ok(), "{fine}");
    }
    let billing = cell::change(
        &crate::alab::registry::ACCOUNT,
        CellKey {
            pk: PK,
            col: "billing",
            elem: None,
        },
        &CellValue::owner(1, json!({"card": "visa", "text": "+1 415 555 0101"})),
    );
    assert!(billing.is_err(), "bounded JSON refuses a phone number too");
    let last4 = |text: &str| {
        cell::change(
            &crate::alab::registry::PHONE,
            CellKey {
                pk: "ph-0011223344556677",
                col: "last4",
                elem: None,
            },
            &CellValue::owner(1, json!(text)),
        )
    };
    assert!(last4("0101").is_ok());
    assert!(last4("01a1").is_err());
    assert!(last4("101").is_err());
    assert!(last4("10101").is_err());
}

/// A schema with a secret-named column that is not a vault reference does not check, so it
/// cannot build; the same column as a vault reference does.
/// NEGATIVE CONTROL: with `secret_named` answering false, the password column checks and this
/// fails.
#[test]
fn secret_named_columns_must_be_vault_references() {
    const BAD: Collection = Collection {
        columns: &[
            Column::new("mailbox_password", Ty::Text(64)),
            Column::new("row", Ty::Enum(LIFE)),
        ],
        ..ADDRESS
    };
    const GOOD: Collection = Collection {
        columns: &[
            Column::new("mailbox_password", Ty::SecretRef),
            Column::new("row", Ty::Enum(LIFE)),
        ],
        ..ADDRESS
    };
    const NO_LIFECYCLE: Collection = Collection {
        columns: &[Column::new("addr", Ty::Email)],
        ..ADDRESS
    };
    assert_eq!(
        BAD.check(),
        Err("a secret-named column must be a vault reference")
    );
    assert_eq!(GOOD.check(), Ok(()));
    assert!(NO_LIFECYCLE.check().is_err());
    assert!(
        ACCOUNT.schema_row()["x"]["columns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["pii"] == json!(false))
    );
    assert_eq!(ADDRESS.schema_row()["x"]["columns"][0]["pii"], json!(true));
}

pub(super) const M1: [u8; 16] = [0x11; 16];
pub(super) const M2: [u8; 16] = [0x22; 16];

pub(super) fn machine_of(_: &Digest, actor: &Actor) -> Option<[u8; 16]> {
    match actor[0] {
        1 => Some(M1),
        2 => Some(M2),
        _ => None,
    }
}

pub(super) fn owned_change(collection: &Collection, key: &str, n: u64) -> Change {
    Change {
        collection: collection.name.to_owned(),
        key: key.to_owned(),
        value: Some(json!({"r": 1, "n": n, "label": "m2"})),
    }
}

/// An owned row lives only at its signer's own machine: a row Mac 2 writes at Mac 1's key is
/// inert, each key's latest ranks by (period, n, sequence), and a self-suffix key names its
/// writer in its last part.
/// NEGATIVE CONTROL: with `read_owned` skipping the signer check (`named` unfiltered), Mac 2's
/// row lands at Mac 1's key and this fails.
#[test]
fn owned_rows_at_another_macs_key_are_inert() {
    let m1 = keys::uuid32(&M1);
    let m2 = keys::uuid32(&M2);
    let mut book = Book::new();
    book.write(2, &[], vec![owned_change(&FACTS, &m2, 1)]);
    book.write(2, &[], vec![owned_change(&FACTS, &m1, 9)]);
    book.write(1, &[], vec![owned_change(&FACTS, &m1, 1)]);
    book.write(2, &[], vec![owned_change(&FACTS, &m2, 3)]);
    book.write(2, &[], vec![owned_change(&FACTS, &m2, 2)]);
    book.write(
        2,
        &[],
        vec![owned_change(&OBS, &format!("mb-0011223344556677.{m2}"), 1)],
    );
    book.write(
        1,
        &[],
        vec![owned_change(&OBS, &format!("mb-0011223344556677.{m2}"), 5)],
    );
    let mut ops = book.ops(0);
    let read = owned::read_owned(&FACTS, &ops, machine_of);
    assert_eq!(read.inert, 1);
    assert_eq!(read.rows[&m1].value["n"], json!(1));
    assert_eq!(read.rows[&m1].machine, M1);
    assert_eq!(
        read.rows[&m2].value["n"],
        json!(3),
        "the highest counter wins"
    );
    // A later period outranks any counter.
    ops.push(Op {
        period: 1,
        ..ops[0].clone()
    });
    let read = owned::read_owned(&FACTS, &ops, machine_of);
    assert_eq!(read.rows[&m2].rank.0, 1);
    let obs = owned::read_owned(&OBS, &book.ops(0), machine_of);
    assert_eq!(obs.inert, 1);
    assert_eq!(obs.rows.len(), 1);
}

/// A log keeps each writer's own entries in counter order, drops entries at another Mac's key,
/// and keeps the first of a reused counter.
/// NEGATIVE CONTROL: with `read_log` keeping every entry of a reused counter (its `seen`
/// check removed), the duplicate shows and this fails.
#[test]
fn log_entries_keep_each_writers_own_and_drop_reused_counters() {
    let m1 = keys::uuid32(&M1);
    let m2 = keys::uuid32(&M2);
    let batch = |key: String, tag: &str| Change {
        collection: "ev".into(),
        key,
        value: Some(json!({"events": [tag]})),
    };
    let mut book = Book::new();
    book.write(2, &[], vec![batch(format!("{m2}.2"), "second")]);
    book.write(2, &[], vec![batch(format!("{m2}.1"), "first")]);
    book.write(2, &[], vec![batch(format!("{m2}.1"), "reused")]);
    book.write(2, &[], vec![batch(format!("{m1}.1"), "forged")]);
    book.write(1, &[], vec![batch(format!("{m1}.1"), "own")]);
    book.write(1, &[], vec![batch(format!("{m1}.01"), "noncanonical")]);
    let read = owned::read_log(&EV, &book.ops(0), machine_of);
    let tags: Vec<_> = read
        .entries
        .iter()
        .map(|e| e.value["events"][0].as_str().unwrap())
        .collect();
    assert_eq!(tags, ["own", "first", "second"]);
    assert_eq!(read.inert, 3);
}

/// A counter reused across period tables keeps the entry of the earlier period, though each
/// table's writer sequence starts again at 1, so the later period's entry has the smaller one.
/// NEGATIVE CONTROL: with `read_log` comparing sequences alone, the later period's entry
/// replaces the first: `` left: ["reused"] right: ["first"] ``.
#[test]
fn a_counter_reused_in_a_later_period_keeps_the_first_entry() {
    let m2 = keys::uuid32(&M2);
    let batch = |tag: &str| Change {
        collection: "ev".into(),
        key: format!("{m2}.1"),
        value: Some(json!({"events": [tag]})),
    };
    let mut early = Book::new();
    early.write(2, &[], vec![batch("filler")]);
    early.write(2, &[], vec![batch("first")]);
    let mut late = Book::new();
    late.write(2, &[], vec![batch("reused")]);
    // Period 5's table holds the entry at sequence 2, period 6's the reuse at sequence 1.
    let mut ops: Vec<Op> = early.ops(5).into_iter().skip(1).collect();
    ops.extend(late.ops(6));
    for order in [ops.clone(), ops.into_iter().rev().collect()] {
        let read = owned::read_log(&EV, &order, machine_of);
        let tags: Vec<_> = read
            .entries
            .iter()
            .map(|e| e.value["events"][0].as_str().unwrap())
            .collect();
        assert_eq!(tags, ["first"]);
        assert_eq!(read.inert, 1);
    }
}

/// The schema collection reads as the highest version any eligible operation wrote, though an
/// older build's later write replaced the cell; a lower version may not be published, and two
/// rows at one version split it.
/// NEGATIVE CONTROL: with `schema_state` taking the last write it meets (no `held > version`
/// arm), the older build's version 1 wins and this fails.
#[test]
fn schema_reads_the_highest_version_ever_written() {
    let row = |version: u32, extra: &str| Change {
        collection: SCHEMA.into(),
        key: "account".into(),
        value: Some(json!({"r": version, "x": {"note": extra}})),
    };
    let mut book = Book::new();
    let v2 = book.write(1, &[], vec![row(2, "new")]);
    book.write(2, &[v2], vec![row(1, "old")]);
    let cells = book.cells();
    let current = &cells[&(SCHEMA.to_owned(), "account".to_owned())].candidates;
    assert_eq!(
        current.len(),
        1,
        "per-cell replacement keeps only the later write"
    );
    let state = owned::schema_state(&book.ops(0), "account");
    assert_eq!(
        state,
        SchemaState::One {
            version: 2,
            row: json!({"r": 2, "x": {"note": "new"}})
        }
    );
    assert!(!owned::may_publish_schema(&state, 1));
    assert!(owned::may_publish_schema(&state, 2));
    book.write(3, &[], vec![row(2, "other")]);
    assert_eq!(
        owned::schema_state(&book.ops(0), "account"),
        SchemaState::Split { version: 2 }
    );
}

/// A writer's automatic records past the table's quota never project; owner records never
/// count against it.
/// NEGATIVE CONTROL: with `owned::automatic` answering false, the third birth projects and
/// this fails.
#[test]
fn automatic_records_past_the_quota_never_project() {
    let birth = |pk: &str| {
        cell::change(
            &ADDRESS,
            CellKey {
                pk,
                col: "addr",
                elem: None,
            },
            &CellValue::automatic(1, json!(format!("{pk}@example.invalid"))),
        )
        .unwrap()
    };
    let mut book = Book::new();
    for pk in ["ad-1", "ad-2", "ad-3"] {
        book.write(2, &[], vec![birth(pk)]);
    }
    for n in 0..4 {
        book.write(1, &[], vec![put(&ACCOUNT, PK, "units", json!(n))]);
    }
    let quota = owned::quota(&TABLE, &book.ops(0), &[]);
    assert_eq!(quota.spent.get(&actor(2)), Some(&3));
    assert_eq!(quota.spent.get(&actor(1)), None);
    let rows = cell::project(&ADDRESS, &book.cells_without(&quota.over)).rows;
    assert_eq!(
        rows.keys().map(String::as_str).collect::<Vec<_>>(),
        ["ad-1", "ad-2"]
    );
    let accounts = cell::project(&ACCOUNT, &book.cells_without(&quota.over)).rows;
    assert_eq!(accounts[PK].get("units"), Some(&json!(3)));
}

/// A writer's automatic record costs the merges it needed too: the causal-only joins its
/// writer appended just before it on a wide frontier count against the quota, so one Mac's
/// births and their merges stay within the records the quota leaves the owner. A merge before
/// an owner record costs nothing.
/// NEGATIVE CONTROL: with `quota` charging each automatic record one record and no merges,
/// the second birth projects: `` left: Some(2) right: Some(3) ``.
#[test]
fn merges_before_an_automatic_record_count_against_the_quota() {
    let birth = |pk: &str| {
        cell::change(
            &ADDRESS,
            CellKey {
                pk,
                col: "addr",
                elem: None,
            },
            &CellValue::automatic(1, json!(format!("{pk}@example.invalid"))),
        )
        .unwrap()
    };
    let mut book = Book::new();
    // Writer 2: a birth (1), a merge (2), a birth (3). Writer 1: a merge (1), an owner edit (2).
    book.write(2, &[], vec![birth("ad-1")]);
    book.write(2, &[], vec![]);
    book.write(2, &[], vec![birth("ad-2")]);
    book.write(1, &[], vec![]);
    book.write(1, &[], vec![put(&ACCOUNT, PK, "units", json!(1))]);
    let mut ops = book.ops(0);
    let merges: Vec<(Actor, u64)> = ops
        .iter()
        .filter(|op| op.changes.is_empty())
        .map(|op| (op.actor, op.sequence))
        .collect();
    ops.retain(|op| !op.changes.is_empty());
    let quota = owned::quota(&TABLE, &ops, &merges);
    assert_eq!(quota.spent.get(&actor(2)), Some(&3));
    assert_eq!(quota.spent.get(&actor(1)), None);
    let rows = cell::project(&ADDRESS, &book.cells_without(&quota.over)).rows;
    assert_eq!(
        rows.keys().map(String::as_str).collect::<Vec<_>>(),
        ["ad-1"]
    );
}

/// The account key is the claims key: an alias login keys by its identity address, every
/// realm spelling the claims ring accepts collapses, and each provider has one word.
/// NEGATIVE CONTROL: with `claims_email` preferring the login, the alias account no longer
/// matches its claims key and this fails.
#[test]
fn account_keys_follow_the_claims_key() {
    let identity = "Person.One@Example.Invalid ";
    let alias = "alias-7@proton.example";
    let claims = keys::claims_email(Some(identity), Some(alias)).unwrap();
    assert_eq!(claims, "person.one@example.invalid");
    let pk = keys::account_pk("codex", &claims).unwrap();
    assert_eq!(
        pk,
        keys::account_pk("OpenAI", "person.one@example.invalid").unwrap()
    );
    assert_ne!(pk, keys::account_pk("codex", alias).unwrap());
    assert_eq!(
        keys::claims_email(None, Some(alias)).unwrap(),
        "alias-7@proton.example"
    );
    for spelling in ["kimi", "kimi-global", "KIMI:GLOBAL", " kimi:global "] {
        assert_eq!(keys::canonical_realm(spelling), Some("kimi:global"));
        assert_eq!(keys::provider_of_realm(spelling), Some(keys::KIMI));
    }
    assert_eq!(keys::canonical_realm("x"), Some("grok"));
    assert_eq!(keys::provider_of_realm("grok"), Some(keys::XAI));
    assert_eq!(keys::provider_of_realm("claude"), Some(keys::ANTHROPIC));
    assert_ne!(
        keys::account_pk("kimi:global", alias).unwrap(),
        keys::account_pk("kimi:mainland-cn", alias).unwrap()
    );
    assert!(keys::account_pk("nope", alias).is_err());
    assert!(keys::account_pk("claude", "not-an-address").is_err());
}

/// Every key derivation's longest key fits the grammar and leaves room for the longest column
/// and element under 64 bytes; random keys are not a function of what they stand for.
/// NEGATIVE CONTROL: with an auth-route code allowed past six characters
/// (`device-code` → `devicecode`), its key passes 26 and this fails.
#[test]
fn every_key_fits_the_grammar() {
    let account = keys::account_pk("claude", "a@example.invalid").unwrap();
    let machine = keys::machine_pk("m112");
    let mut longest = vec![
        account.clone(),
        machine.clone(),
        keys::address_pk("a@example.invalid").unwrap(),
        keys::subaccount_pk(&account, "0123456789ab").unwrap(),
        keys::preference_pk(&machine, &account).unwrap(),
        keys::source_pk(b"sheet"),
        keys::model_pk("Mac16,8").unwrap(),
        keys::random_pk(keys::Random::Phone).unwrap(),
        keys::random_pk(keys::Random::Mailbox).unwrap(),
    ];
    for (method, _) in keys::METHOD_CODES {
        longest.push(keys::auth_route_pk(&account, method).unwrap());
    }
    for pk in &longest {
        assert!(pk_grammar(pk), "{pk}");
        let key = CellKey {
            pk,
            col: "abcdefghijklmnopqrst",
            elem: Some("anthropic-max20"),
        };
        assert!(key.encode().is_ok(), "{pk}");
    }
    assert!(keys::preference_pk(&account, &machine).is_err());
    assert!(keys::auth_route_pk(&account, "carrier-pigeon").is_err());
    assert_ne!(
        keys::random_pk(keys::Random::Phone).unwrap(),
        keys::random_pk(keys::Random::Phone).unwrap()
    );
    assert!(keys::is_label("m112") && keys::is_label("m40-3fa2"));
    for bad in ["M109", "m0", "m01", "m40-3FA2", "m40-3fa", "x1"] {
        assert!(!keys::is_label(bad), "{bad}");
    }
}

/// A re-import writes only a cell that is absent or that an earlier import wrote; an owner's
/// value or a conflict is left and reported, and an unchanged cell spends nothing.
/// NEGATIVE CONTROL: with `import_step` writing over an owner's value (its owner arm made
/// `Write`), the owner's correction is overwritten and this fails.
#[test]
fn import_never_writes_over_the_owner() {
    let source = "sr-0011223344556677";
    let candidate = |value: CellValue| cell::Candidate {
        operation: [1; 32],
        value,
    };
    let imported = Field::Value(candidate(CellValue::imported(1, json!("pending"), source)));
    let owners = Field::Value(candidate(CellValue::owner(1, json!("active"))));
    let cleared = Field::Value(candidate(CellValue::owner(1, Value::Null)));
    let conflict = Field::Conflict {
        candidates: vec![
            candidate(CellValue::owner(1, json!("active"))),
            candidate(CellValue::imported(1, json!("pending"), source)),
        ],
        provisional: None,
    };
    assert_eq!(cell::import_step(None, &json!("paused")), ImportStep::Write);
    assert_eq!(
        cell::import_step(Some(&imported), &json!("paused")),
        ImportStep::Write
    );
    assert_eq!(
        cell::import_step(Some(&imported), &json!("pending")),
        ImportStep::Unchanged
    );
    assert_eq!(
        cell::import_step(Some(&owners), &json!("active")),
        ImportStep::Unchanged
    );
    assert!(matches!(
        cell::import_step(Some(&owners), &json!("paused")),
        ImportStep::Finding(_)
    ));
    assert!(matches!(
        cell::import_step(Some(&cleared), &json!("paused")),
        ImportStep::Finding(_)
    ));
    assert!(matches!(
        cell::import_step(Some(&conflict), &json!("paused")),
        ImportStep::Finding(_)
    ));
    // The provenance survives the round trip through a stored value.
    let stored = CellValue::imported(1, json!("pending"), source).encode();
    assert_eq!(
        CellValue::decode(&stored).unwrap().s.as_deref(),
        Some(source)
    );
    assert!(CellValue::decode(&json!({"r": 1, "x": 1, "s": "who@example.invalid"})).is_err());
}

/// An owner's value that an import wrote too stays the owner's: equal concurrent values collapse
/// to the owner's candidate even when the import's has the smaller digest, so a later import
/// with a different value leaves the cell and reports it.
/// NEGATIVE CONTROL: with `cell::field` sorting by digest alone, the import's candidate stands
/// for both and the next import would overwrite the owner: `` assertion failed:
/// matches!(field, Field::Value(c) if c.value.s.is_none()) ``.
#[test]
fn an_owner_value_an_import_also_wrote_stays_the_owners() {
    let source = "sr-0011223344556677";
    let mut book = Book::new();
    let birth = book.write(1, &[], vec![put(&ACCOUNT, PK, "row", json!("live"))]);
    // The import's write is made first, so its operation digest is the smaller.
    book.write(
        2,
        &[birth],
        vec![
            cell::change(
                &ACCOUNT,
                CellKey {
                    pk: PK,
                    col: "status",
                    elem: None,
                },
                &CellValue::imported(ACCOUNT.version, json!("paused"), source),
            )
            .unwrap(),
        ],
    );
    book.write(
        3,
        &[birth],
        vec![put(&ACCOUNT, PK, "status", json!("paused"))],
    );
    let row = cell::project(&ACCOUNT, &book.cells())
        .rows
        .remove(PK)
        .unwrap();
    let field = &row.fields["status"];
    assert!(matches!(field, Field::Value(c) if c.value.s.is_none()));
    assert!(matches!(
        cell::import_step(Some(field), &json!("active")),
        ImportStep::Finding(_)
    ));
}

fn relay_entry(n: u64, kind: &str) -> Value {
    json!({"a": format!("ac-{:016x}", n), "m": format!("mc-{:016x}", n * 7),
        "t": 1_793_152_853_928_u64 + n, "k": kind, "p": 2965})
}

/// The worst-case relay row keeps its highest-priority entries within its cap, dropping from the
/// end of the priority order, and the worst-case per-account use row fits its cap whole (the
/// byte math of design §4.17).
/// NEGATIVE CONTROL: with `fit` keeping every entry (returning `entries.len()`), the worst case
/// keeps all 144 entries, past 12 KiB, and this fails.
#[test]
fn rows_fit_their_caps_by_priority() {
    let relay = |entries: &[Value]| json!({"r": 1, "n": 999_999, "period": 2965, "bucket": 15, "entries": entries});
    // 768 accounts (two and a half times the lab's) in 16 buckets: 48 accounts × use, sign-in
    // and sign-out, priority ordered.
    let mut entries = Vec::new();
    for kind in ["use", "signin", "signout"] {
        for n in 0..48 {
            entries.push(relay_entry(n, kind));
        }
    }
    let kept = owned::fit(&entries, owned::caps::RELAY_ROW, relay).unwrap();
    assert!(kept < entries.len(), "the worst case needs truncation");
    assert!(kept >= 48, "every account's newest use survives");
    let bytes = aterm_json::to_vec(&relay(&entries[..kept])).unwrap().len();
    assert!(bytes <= owned::caps::RELAY_ROW);
    assert!(aterm_json::to_vec(&relay(&entries[..=kept])).unwrap().len() > owned::caps::RELAY_ROW);
    let t = 1_793_152_853_928_u64;
    let quota: Vec<Value> = (0..6)
        .map(|_| json!({"w": "7d-model", "p": 1000, "r": t, "rr": "exact", "t": t, "src": "session-usage"}))
        .collect();
    let walls: Vec<Value> = (0..6)
        .map(|_| json!({"w": "7d-model", "until": t, "t": t}))
        .collect();
    let use_row = json!({"r": 1, "n": 999_999, "period": 2965, "first": t, "last_start": t,
        "last_end": t, "sessions_7d": 9999, "signin": {"state": "signed-out", "t": t, "cli": "codex-alab"},
        "quota": quota, "wall": walls, "plan": {"word": "anthropic-max20", "t": t}, "key": "a".repeat(12)});
    assert!(aterm_json::to_vec(&use_row).unwrap().len() <= owned::caps::USE_ROW);
    assert!(owned::fit(&[], 2, relay).is_err());
    assert!(owned::relay_bucket("ac-0123456789abcdef") < owned::caps::RELAY_BUCKETS);
    let duty = owned::relay_duty(&M1);
    assert_eq!((duty[0] + 8) % 16, duty[1]);
}

/// Findings name dangling references, unique clashes among live rows, conflicts and raw
/// values by key and column, never by value.
/// NEGATIVE CONTROL: with the unique check in `check::findings` skipped, the duplicate address
/// goes unreported and this fails.
#[test]
fn findings_name_keys_never_values() {
    let mut book = Book::new();
    let addr = |pk: &str, x: &str| put(&ADDRESS, pk, "addr", json!(x));
    book.write(
        1,
        &[],
        vec![
            addr("ad-1", "same@example.invalid"),
            put(&ADDRESS, "ad-1", "row", json!("live")),
            addr("ad-2", "same@example.invalid"),
            put(&ADDRESS, "ad-2", "row", json!("live")),
            addr("ad-3", "same@example.invalid"),
            put(&ADDRESS, "ad-3", "row", json!("retired")),
            put(&ACCOUNT, PK, "login", json!("ad-9")),
            put(&ACCOUNT, PK, "row", json!("live")),
        ],
    );
    let root = book.heads[&1].1;
    book.write(
        1,
        &[root],
        vec![put(&ACCOUNT, PK, "status", json!("active"))],
    );
    book.write(
        2,
        &[root],
        vec![put(&ACCOUNT, PK, "status", json!("paused"))],
    );
    let cells = book.cells();
    let addresses = cell::project(&ADDRESS, &cells);
    let accounts = cell::project(&ACCOUNT, &cells);
    let tables = BTreeMap::from([("address", &addresses), ("account", &accounts)]);
    let found = check::findings(&[&ACCOUNT, &ADDRESS], &tables);
    assert!(found.contains(&Finding::Unique {
        collection: "address".into(),
        columns: vec!["addr".into()],
        pks: vec!["ad-1".into(), "ad-2".into()],
    }));
    assert!(found.contains(&Finding::Dangling {
        collection: "account".into(),
        pk: PK.into(),
        column: "login".into(),
        target: "address".into(),
    }));
    assert!(found.contains(&Finding::Conflict {
        collection: "account".into(),
        pk: PK.into(),
        column: "status".into(),
    }));
    assert!(!format!("{found:?}").contains('@'));
}

/// A content-derived request id is the same for the same changes in the same table and
/// differs for another table or other changes.
/// NEGATIVE CONTROL: with `request_id` leaving the role out, two tables' equal writes share an
/// id and this fails.
#[test]
fn request_ids_are_content_derived() {
    let changes = vec![put(&ACCOUNT, PK, "status", json!("active"))];
    let id = request_id("alab", "registry", &changes).unwrap();
    assert_eq!(id, request_id("alab", "registry", &changes).unwrap());
    assert_ne!(id, request_id("alab", "registry.2", &changes).unwrap());
    let other = vec![put(&ACCOUNT, PK, "status", json!("paused"))];
    assert_ne!(id, request_id("alab", "registry", &other).unwrap());
}

#[cfg(unix)]
mod commons {
    use super::*;
    use crate::{
        catalog::Descriptor,
        database::Command,
        documents::{application, log::Log},
        table::tests::{Lab, repair_commons},
    };
    use aterm_secrets::{client, commons::roster::Role};

    fn descriptor(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
    ) -> crate::Result<Descriptor> {
        Descriptor::application(bootstrap.trust(), TABLE.application, TABLE.role, 1)
    }

    fn write(lab: &Lab, n: usize, changes: Vec<Change>) {
        application::with_store(&lab.paths[n], true, descriptor, |session, store, _| {
            let command = Command::Write {
                request: crate::crypto::random()?,
                changes,
            };
            application::append(session, store, &command.encode()?).map(drop)
        })
        .unwrap();
    }

    /// Read the test table on Mac `n` without the records past the quota: owned facts through
    /// the historical resolver (or, with `current_only`, a map over the current members), and
    /// the addresses that project.
    fn read(lab: &Lab, n: usize, current_only: bool) -> (owned::OwnedRead, Vec<String>) {
        application::observe_store(&lab.paths[n], descriptor, |session, store, scope| {
            let log = Log::read_with(session, store, scope, Command::decode)?;
            let all: Vec<Op> = log
                .eligible()
                .iter()
                .filter_map(|(operation, command)| Op::from_eligible(0, operation, command))
                .collect();
            let quota = owned::quota(&TABLE, &all, &[]);
            let ops: Vec<Op> = all
                .into_iter()
                .filter(|op| !quota.over.contains(&op.id))
                .collect();
            let members: BTreeMap<Actor, [u8; 16]> = session
                .members()
                .iter()
                .map(|binding| (binding.actor(), *binding.machine()))
                .collect();
            let historical = owned::commons::historical(session);
            let facts = if current_only {
                owned::read_owned(&FACTS, &ops, |_, actor| members.get(actor).copied())
            } else {
                owned::read_owned(&FACTS, &ops, historical)
            };
            let addresses = cell::project(&ADDRESS, &log.cells_without(&quota.over))
                .rows
                .into_keys()
                .collect();
            Ok((facts, addresses))
        })
        .unwrap()
    }

    /// The account sheet's importer names each provider by the registry's one word.
    /// NEGATIVE CONTROL: with `import.rs` mapping the kimi column to a literal `moonshot`,
    /// the importer's word is not the registry's and this fails.
    #[test]
    fn the_importer_and_the_registry_share_provider_words() {
        let words: Vec<_> = crate::import::provider_words().collect();
        assert_eq!(words.len(), keys::PROVIDERS.len());
        assert!(words.iter().all(|word| keys::PROVIDERS.contains(word)));
    }

    /// On log-backed membership, a Mac removed from Commons keeps its owned row: the reader
    /// resolves its signer through the roster its record names. Its row at another Mac's key
    /// is inert. An owned write spends the quota too: Mac 1's facts row and first address
    /// birth are its two automatic records, and the next two births never project.
    /// NEGATIVE CONTROL: with `owned::commons::historical` resolving through the current
    /// members (`session.members()`), the removed Mac's row turns inert and this fails; the
    /// `current_only` read below is that map, and it blanks the row.
    #[test]
    fn a_removed_macs_rows_stay_attributable() {
        let lab = Lab::with_foundation(&[Role::Writer, Role::Writer]);
        client::seed_vault_genesis(&lab.paths[0], &lab._home.path().join("genesis.v2")).unwrap();
        let records = client::load_records(&lab.paths[0]).unwrap().encode();
        client::save_records(&lab.paths[1], &records).unwrap();
        let machines: Vec<[u8; 16]> = (0..2)
            .map(|n| *lab.bootstrap(n).request().unwrap().binding().machine())
            .collect();
        let actors: Vec<Actor> = (0..2)
            .map(|n| lab.bootstrap(n).request().unwrap().binding().actor())
            .collect();
        write(
            &lab,
            0,
            vec![owned_change(&FACTS, &keys::uuid32(&machines[0]), 1)],
        );
        write(
            &lab,
            1,
            vec![owned_change(&FACTS, &keys::uuid32(&machines[1]), 1)],
        );
        write(
            &lab,
            1,
            vec![owned_change(&FACTS, &keys::uuid32(&machines[0]), 7)],
        );
        for pk in ["ad-1", "ad-2", "ad-3"] {
            write(
                &lab,
                0,
                vec![
                    cell::change(
                        &ADDRESS,
                        CellKey {
                            pk,
                            col: "addr",
                            elem: None,
                        },
                        &CellValue::automatic(1, json!(format!("{pk}@example.invalid"))),
                    )
                    .unwrap(),
                ],
            );
        }
        repair_commons(&lab, 4);
        let (facts, addresses) = read(&lab, 0, false);
        assert_eq!(facts.rows.len(), 2);
        assert_eq!(facts.inert, 1, "Mac 2's row at Mac 1's key is inert");
        assert_eq!(facts.rows[&keys::uuid32(&machines[1])].machine, machines[1]);
        assert_eq!(
            addresses,
            ["ad-1"],
            "Mac 1's third and fourth automatic records are over quota"
        );

        client::remove_member_in_log(&lab.paths[0], actors[1]).unwrap();
        let records = client::load_records(&lab.paths[0]).unwrap().encode();
        client::save_records(&lab.paths[1], &records).unwrap();
        let (facts, _) = read(&lab, 0, false);
        assert_eq!(
            facts
                .rows
                .get(&keys::uuid32(&machines[1]))
                .map(|row| row.machine),
            Some(machines[1]),
            "the removed Mac's row stays attributed to it"
        );
        let (current, _) = read(&lab, 0, true);
        assert!(
            !current.rows.contains_key(&keys::uuid32(&machines[1])),
            "the current members alone blank it"
        );
    }
}

/// A registry lifecycle conflict never reads as live: a retire concurrent with a `live` write
/// reads as retired whichever record has the smaller digest, and a conflict against a word a
/// newer build wrote, which the rule cannot rank, is not live either.
/// NEGATIVE CONTROLS: with the registry's `row` column on the default `Show` rule, the `live`
/// write (the smaller digest) is provisional: `` left: Some("live") right: Some("retired") ``;
/// with `Row::live` ignoring a conflict, the row against the newer word reads as live and the
/// last assertion fails.
#[test]
fn a_lifecycle_conflict_never_reads_as_live() {
    let machines = &crate::alab::registry::MACHINE;
    let pk = "mc-0000000000000001";
    let word = |x: &str| {
        let key = CellKey {
            pk,
            col: LIFECYCLE,
            elem: None,
        };
        Change {
            collection: machines.name.to_owned(),
            key: key.encode().unwrap(),
            value: Some(CellValue::owner(1, json!(x)).encode()),
        }
    };
    let concurrent = |first: &str, second: &str| {
        let mut book = Book::new();
        book.write(1, &[], vec![word(first)]);
        book.write(2, &[], vec![word(second)]);
        cell::project(machines, &book.cells()).rows[pk].clone()
    };
    let retired = concurrent("live", "retired");
    assert!(retired.fields[LIFECYCLE].conflicted());
    assert_eq!(retired.lifecycle(), Some("retired"));
    assert!(!retired.live());
    let newer = concurrent("live", "paused");
    assert!(newer.fields[LIFECYCLE].conflicted());
    assert!(!newer.live());
}

/// An account's label names no machine, in any spelling a machine has, and is free text past
/// that: the guards still hold, and the default label passes for every realm. A preference's key
/// is the pair alone, and names no rank.
/// NEGATIVE CONTROL: with `guard::account_label` skipping `machine_shaped`, the first spelling is
/// stored: `` called `Result::unwrap_err()` on an `Ok` value: () `` at `m2`.
#[test]
fn an_account_label_names_no_machine() {
    let label = crate::alab::registry::ACCOUNT.column("label").unwrap();
    let write = |text: &str| {
        cell::validate(&crate::alab::registry::ACCOUNT, label, &json!(text)).map_err(|e| e.0)
    };
    for shaped in [
        "m2",
        "M109",
        "m2.3",
        "m27-2",
        "m106-m38",
        "kimi-m16",
        "m2 spare",
        "m4max",
        "claude.m8",
    ] {
        let why = write(shaped).unwrap_err();
        assert!(why.contains("names no machine"), "{shaped}: {why}");
        assert!(!why.contains(shaped), "{why}");
    }
    for fine in ["research", "max-plan", "muse-1", "team a", "m", "mac mini"] {
        assert!(write(fine).is_ok(), "{fine}");
    }
    for refused in ["", "  ", "a@example.test", &"x".repeat(33)] {
        assert!(write(refused).is_err(), "{refused}");
    }
    let account = keys::account_pk("claude", "a@example.test").unwrap();
    for realm in [
        "claude",
        "codex",
        "grok",
        "kimi:global",
        "kimi:mainland-cn",
        "google",
        "meta",
    ] {
        let label = keys::default_account_label(realm, &account).unwrap();
        assert!(write(&label).is_ok(), "{label}");
        assert_eq!(label.rsplit('-').next().unwrap(), &account[3..9]);
    }
    assert!(keys::default_account_label("carrier", &account).is_err());
    assert!(keys::default_account_label("claude", "m8").is_err());
    let machine = keys::machine_pk("m8");
    let pk = keys::preference_pk(&machine, &account).unwrap();
    assert!(pk.starts_with("pf-") && pk.len() == 25, "{pk}");
    assert_ne!(
        pk,
        keys::preference_pk(&keys::machine_pk("m9"), &account).unwrap()
    );
}

/// A note cell an import would copy lab-wide refuses what says or reads like a sign-in secret,
/// including what `guard::text` lets a person type, and keeps ordinary prose.
/// NEGATIVE CONTROL: with `note_cell` returning `text(value, max)` alone, `pass: Hunter2!` is
/// accepted: this test's `` refused: pass: Hunter2! `` message; with `NOTE_WORDS` unread,
/// `login pass plainword` is.
#[test]
fn a_copied_note_refuses_what_reads_as_a_secret() {
    for text in [
        "pass: Hunter2!",
        "pw Hunter2!",
        "login pass plainword",
        "seed phrase apple pear",
        "PIN 4821",
        "token abc",
        "Hunter2!",
        "abc123",
        "hun!ter",
    ] {
        assert!(guard::note_cell(text, 512).is_err(), "refused: {text}");
    }
    for text in [
        "phone required",
        "compass bypass passed",
        "claude banned",
        "moved to m106-m38, then m3.2",
        "gpt-5.5 at $200/mo",
        "it's banned! (again)",
        "2026-11-01: paused",
    ] {
        assert_eq!(guard::note_cell(text, 512), Ok(()), "kept: {text}");
    }
    assert!(guard::text("pass: Hunter2!", 512).is_ok());
}

/// Vault references, foreign keys, set elements and cell primary keys enforce the credential
/// and inner-type guards without rejecting legitimate vault entry names (`mailbox-password`,
/// `ai-mailbox.toml`, `totp`) or 16-digit hex digest keys (`mc-0000000000000009`).
/// NEGATIVE CONTROLS: with `Ty::SecretRef` checking only `crypto::name`, a token pasted into
/// `"entry"` is accepted; with `check_elem` passing every `pk_grammar` word, `"abcd"` is
/// accepted in a `Ty::Set(&Ty::Digits(4))` column and a token is accepted in `clis`.
#[test]
fn vault_refs_foreign_keys_and_set_elements_enforce_credential_and_type_guards() {
    let token = concat!("ghp", "_cccccccccccccccccccccc");
    let seed = concat!("JBSWY3DP", "EHPK3PXP");
    let oauth = concat!("ya", "29.a0AfH6SMBx1y2z3w4v5u6t7s8r9q");

    // Legitimate vault entry names that name secret kinds stay valid.
    for entry in [
        "ai-mailbox.toml",
        "mailbox-password",
        "totp",
        "mailbox-mb-0011223344556677",
    ] {
        let ok = cell::change(
            &ACCOUNT,
            CellKey {
                pk: PK,
                col: "vault_ref",
                elem: None,
            },
            &CellValue::owner(2, json!({"vault": "alab", "entry": entry, "item": "totp"})),
        );
        assert!(ok.is_ok(), "{entry}: {ok:?}");
    }
    // Credential shapes in vault, entry or item are refused without quoting them.
    for bad in [
        json!({"vault": token, "entry": "ai-mailbox.toml"}),
        json!({"vault": "alab", "entry": oauth}),
        json!({"vault": "alab", "entry": "ai-mailbox.toml", "item": seed}),
    ] {
        let why = cell::change(
            &ACCOUNT,
            CellKey {
                pk: PK,
                col: "vault_ref",
                elem: None,
            },
            &CellValue::owner(2, bad),
        )
        .unwrap_err()
        .0;
        assert!(
            !why.contains("ghp") && !why.contains("JBSW") && !why.contains("ya29"),
            "{why}"
        );
    }

    // Foreign-key references accept 16-digit decimal-hex keys and refuse credential shapes.
    assert!(
        cell::change(
            &ACCOUNT,
            CellKey {
                pk: "ac-0000000000000009",
                col: "login",
                elem: None,
            },
            &CellValue::owner(2, json!("ad-0000000000000009")),
        )
        .is_ok()
    );
    for bad in [token, seed] {
        assert!(
            cell::change(
                &ACCOUNT,
                CellKey {
                    pk: PK,
                    col: "login",
                    elem: None,
                },
                &CellValue::owner(2, json!(bad)),
            )
            .is_err()
        );
        assert!(
            cell::change(
                &ACCOUNT,
                CellKey {
                    pk: bad,
                    col: "row",
                    elem: None,
                },
                &CellValue::owner(2, json!("live")),
            )
            .is_err()
        );
        assert!(
            cell::change(
                &crate::alab::registry::MACHINE,
                CellKey {
                    pk: "mc-0000000000000009",
                    col: "clis",
                    elem: Some(bad),
                },
                &CellValue::owner(1, json!(true)),
            )
            .is_err()
        );
    }

    // `check_elem` enforces the set's inner scalar type, not only `pk_grammar`.
    const PINS: Collection = Collection {
        name: "pin",
        columns: &[
            Column::new("codes", Ty::Set(&Ty::Digits(4))),
            Column::new("row", Ty::Enum(LIFE)),
        ],
        ..ADDRESS
    };
    assert!(
        cell::change(
            &PINS,
            CellKey {
                pk: "ph-0011223344556677",
                col: "codes",
                elem: Some("0101"),
            },
            &CellValue::owner(1, json!(true)),
        )
        .is_ok()
    );
    assert!(
        cell::change(
            &PINS,
            CellKey {
                pk: "ph-0011223344556677",
                col: "codes",
                elem: Some("abcd"),
            },
            &CellValue::owner(1, json!(true)),
        )
        .is_err()
    );
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fleet_db replica's dump as import sources, over hand-built histories: synthetic dumps of
//! the shape `aterm db dump <file> --show-passwords` writes (every address `example.invalid` or
//! `proton.example`, every password `synthetic-pass-<n>`, every number in `555-01xx`).

use super::super::tests::{SEALER, Stand};
use super::*;
use crate::{
    alab::{
        import::{Input, Plan, Step, plan},
        registry::{ADDRESS, MAILBOX, PHONE, SOURCE},
    },
    typed::{
        cell::{self, Cells},
        grid::tests::{Book, put},
        secret::{self, Sealer as _},
    },
};

const NOW: u64 = 1_800_000_000_000;

/// A synthetic `[everyone]` key the dumps' sources are keyed under.
const KEY: [u8; 32] = [0x5a; 32];

/// The plan of `sheets` over `cells`.
fn planned(sheets: &[Sheet], cells: &Cells) -> Plan {
    plan(
        &Input {
            sheets,
            macs: &[],
            vendors: &Stand,
            sealer: &SEALER,
            now_ms: NOW,
            births: true,
            unread: &[],
        },
        cells,
    )
    .unwrap()
}

/// The registry after `plan`'s rows, written in order by writer 1 on `book`.
fn apply(book: &mut Book, plan: &Plan) -> Cells {
    for changes in &plan.rows {
        book.write(1, &[], changes.clone());
    }
    book.cells()
}

fn sheets(dump: &Value, mode: Mode) -> Vec<Sheet> {
    dump_sheets(
        "fleet-db-m104.json",
        &aterm_json::to_vec(dump).unwrap(),
        mode,
        &KEY,
    )
    .unwrap()
}

fn step<'p>(plan: &'p Plan, collection: &str, pk: &str, column: &str) -> Option<&'p Step> {
    plan.planned
        .iter()
        .find(|p| p.collection == collection && p.pk == pk && p.column == column)
        .map(|p| &p.step)
}

fn value<'p>(plan: &'p Plan, collection: &str, pk: &str, column: &str) -> Option<&'p Value> {
    plan.planned
        .iter()
        .find(|p| p.collection == collection && p.pk == pk && p.column == column)
        .map(|p| &p.x)
}

fn kinds(plan: &Plan) -> Vec<&'static str> {
    plan.findings.iter().map(|f| f.kind.word()).collect()
}

/// Every stored value the plan writes, as JSON text.
fn written(plan: &Plan) -> String {
    plan.rows
        .iter()
        .flatten()
        .map(|c| aterm_json::to_string(c.value.as_ref().unwrap()).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn email_row(n: usize) -> Value {
    json!({
        "email": format!("box{n:02}@proton.example"),
        "provider": "proton",
        "account_type": "root",
        "root_account": null,
        "password": format!("synthetic-pass-{n:02}"),
        "webmail_url": "https://mail.proton.me/u/0/inbox",
        "status": "active",
        "phone": null,
        "protected": false,
        "assigned_computers": [],
        "services": [],
        "notes": null,
    })
}

/// `row` with `changes` set.
fn with(mut row: Value, changes: &[(&str, Value)]) -> Value {
    let object = row.as_object_mut().unwrap();
    for (key, value) in changes {
        object.insert((*key).to_owned(), value.clone());
    }
    row
}

/// A dump of these email rows and no other table's.
fn emails(rows: Vec<Value>) -> Value {
    json!({"emails": rows, "computers": [], "phones": [], "ai_accounts": []})
}

/// A dump of the replica's shape: 39 mailbox logins with passwords, as fleet_db held them.
fn thirty_nine() -> Value {
    emails((1..=39).map(email_row).collect())
}

/// One row of every table with every column fleet_db holds, plus a column it never held.
fn every_column() -> Value {
    json!({
        "emails": [
            {
                "email": "root@proton.example",
                "provider": "proton",
                "account_type": "root",
                "root_account": null,
                "password": "synthetic-pass-root",
                "webmail_url": "https://mail.proton.me/u/1/inbox",
                "status": "active",
                "phone": "415-555-0101",
                "protected": false,
                "assigned_computers": ["m3"],
                "services": ["claude"],
                "notes": "the hub",
                "favourite_colour": "teal",
            },
            {
                "email": "alias1@proton.example",
                "provider": "proton",
                "account_type": "alias",
                "root_account": "root@proton.example",
                "password": null,
                "webmail_url": "https://mail.proton.me/u/0/inbox",
                "status": "suspended",
                "phone": null,
                "protected": false,
                "assigned_computers": [],
                "services": [],
                "notes": null,
            },
        ],
        "phones": [{
            "phone": "415-555-0101",
            "label": "desk-line",
            "owner_carrier": "Tello",
            "line_type": "eSIM",
            "device": "desk phone",
            "status": "active",
            "linked_emails": ["root@proton.example"],
            "linked_computers": ["m3"],
            "notes": "primary test line",
        }],
        "ai_accounts": [{
            "id": "claude:root@proton.example",
            "provider": "anthropic",
            "service": "claude",
            "email": "root@proton.example",
            "receiving_mailbox": "root@proton.example",
            "assigned_computers": ["m3"],
            "tier": "1",
            "status": "active",
            "notes": "max plan",
        }],
        "computers": [{
            "label": "m3",
            "hostname": "studio.lan",
            "ip": "192.0.2.3",
            "node_id": "00112233445566778899aabbccddeeff",
            "make": "Apple",
            "arch": "arm64",
            "os": "macOS",
            "chip": "M3 Max",
            "memory_gb": 64,
            "disk_gb": 2000,
            "aliases": ["m30", "fb"],
            "nickname": "studio",
            "nickname_aliases": ["redballoon"],
            "location": "SF",
            "status": "parked",
            "assigned_accounts": ["claude:root@proton.example"],
            "assigned_emails": ["root@proton.example"],
            "notes": "desk Mac",
        }],
    })
}

/// The fixture holds every column [`DUMP_TABLES`] names for each table, and each is carried:
/// to its registry cell (the root's provider, kind, status, phone, note and sealed password; the
/// alias's `alias_of`; the phone's sealed number, label, carrier, line type, device and note;
/// the account's provider, service, login and note, its tier read the harness's way; the
/// machine's make, arch, OS, chip, memory, disk, aliases, nickname and its aliases, location,
/// status, node, host and note), or as a relation (preferences, the alias's mailbox), or
/// dropped by the design's word (`ip`). A column fleet_db never held is a finding by name, its
/// value nowhere.
/// NEGATIVE CONTROL: with `dump_computer` skipping `memory_gb` (its loop over `("memory_gb",
/// "ram_gb")` dropped), the machine's `ram_gb` is never planned: `` left: None right:
/// Some(Number(64)) ``.
#[test]
fn every_fleet_db_column_is_carried() {
    let dump = every_column();
    for (word, _, columns) in DUMP_TABLES {
        let row = &dump[word][0];
        let held: BTreeSet<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .filter(|k| *k != "favourite_colour")
            .collect();
        let named: BTreeSet<&str> = columns.iter().map(|(c, _)| *c).collect();
        assert_eq!(held, named, "{word}");
    }
    let sheets = sheets(&dump, Mode::Authoritative);
    assert_eq!(sheets.len(), 4);
    let plan = planned(&sheets, &Cells::new());
    let root = keys::address_pk("root@proton.example").unwrap();
    let alias = keys::address_pk("alias1@proton.example").unwrap();
    for (column, x) in [
        ("provider", json!("proton")),
        ("kind", json!("standalone")),
        ("status", json!("active")),
        ("protected", Value::Null),
        ("note", json!("the hub")),
    ] {
        if x.is_null() {
            assert_eq!(value(&plan, "address", &root, column), None, "{column}");
        } else {
            assert_eq!(value(&plan, "address", &root, column), Some(&x), "{column}");
        }
    }
    assert_eq!(
        value(&plan, "address", &alias, "status"),
        Some(&json!("suspended"))
    );
    assert_eq!(
        value(&plan, "address", &alias, "alias_of"),
        Some(&json!(root))
    );
    let mailbox = keys::mailbox_pk(&root).unwrap();
    assert_eq!(
        value(&plan, "address", &alias, "mailbox"),
        Some(&json!(mailbox))
    );
    assert_eq!(
        value(&plan, "address", &root, "mailbox"),
        Some(&json!(mailbox))
    );
    assert_eq!(
        value(&plan, "mailbox", &mailbox, "login"),
        Some(&json!(root))
    );
    assert_eq!(
        value(&plan, "mailbox", &mailbox, "webmail"),
        Some(&json!("https://mail.proton.me/u/1/inbox"))
    );
    assert_eq!(
        value(&plan, "address", &root, "password"),
        Some(&json!(secret::MASK))
    );
    // The phone: found by its number, its cells carried.
    let phone = value(&plan, "address", &root, "phone")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    assert!(keys::is_digest_pk(&phone, "ph"), "{phone}");
    for (column, x) in [
        ("label", json!("desk-line")),
        ("carrier", json!("Tello")),
        ("line_type", json!("eSIM")),
        ("device", json!("desk phone")),
        ("last4", json!("0101")),
        ("number", json!(secret::MASK)),
        ("note", json!("primary test line")),
    ] {
        assert_eq!(value(&plan, "phone", &phone, column), Some(&x), "{column}");
    }
    // The account.
    let account = keys::account_pk("claude", "root@proton.example").unwrap();
    assert_eq!(
        value(&plan, "account", &account, "provider"),
        Some(&json!("anthropic"))
    );
    assert_eq!(
        value(&plan, "account", &account, "services.claude-code"),
        Some(&json!(true))
    );
    assert_eq!(
        value(&plan, "account", &account, "note"),
        Some(&json!("max plan"))
    );
    // The machine.
    let machine = keys::machine_pk("m3");
    let m = keys::planned_machine_pk("m3", |_| false).unwrap_or(machine);
    let machine_value = |column: &str| {
        plan.planned
            .iter()
            .find(|p| p.collection == "machine" && p.column == column && p.pk == m)
            .map(|p| p.x.clone())
    };
    for (column, x) in [
        ("make", json!("Apple")),
        ("arch", json!("arm64")),
        ("os", json!("macOS")),
        ("chip", json!("M3 Max")),
        ("ram_gb", json!(64)),
        ("disk_gb", json!(2000)),
        ("aliases.m30", json!(true)),
        ("nickname", json!("studio")),
        ("nickname_aliases", json!(["fb", "redballoon"])),
        ("location", json!("SF")),
        ("fleet_node", json!("00112233445566778899aabbccddeeff")),
        ("hosts", json!(["studio.lan"])),
        ("note", json!("desk Mac")),
    ] {
        assert_eq!(machine_value(column), Some(x), "{column}");
    }
    // A word other than the `active` fleet_db wrote for every row is a statement: carried on a
    // birth too.
    assert_eq!(machine_value("status"), Some(json!("parked")));
    let preference = keys::preference_pk(&m, &account).unwrap();
    assert!(
        plan.planned
            .iter()
            .any(|p| p.collection == "preference" && p.pk == preference),
        "{:?}",
        kinds(&plan)
    );
    // `ip` is carried nowhere, and the unknown column only by its name.
    let text = written(&plan);
    for absent in ["192.0.2.3", "teal", "synthetic-pass-root", "555-0101"] {
        assert!(!text.contains(absent), "{absent}");
    }
    assert!(
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::Unmapped && f.column.as_deref() == Some("favourite_colour"))
    );
    // The source rows: one per table, named for the file and table.
    let sources: BTreeSet<String> = plan
        .rows
        .iter()
        .flatten()
        .filter(|c| c.collection == SOURCE.name && c.key.ends_with(".name"))
        .map(|c| {
            cell::CellValue::decode(c.value.as_ref().unwrap())
                .unwrap()
                .x
                .to_string()
        })
        .collect();
    assert_eq!(sources.len(), 4, "{sources:?}");
    assert!(sources.iter().all(|s| s.contains("fleet-db-m104.json#")));
}

/// The 39 passwords of a dump of the replica's shape land as 39 sealed `Secret` cells: each
/// token opens, under its own row and column, to that row's password and to no other row's, no
/// plaintext is in any written value, and a re-import of the same dump writes nothing.
/// NEGATIVE CONTROL: with `finish` sealing every secret under one row's coordinates (`pk` in
/// `sealer.seal(name, "ad-0000000000000000", column, ..)`), no token opens under its own row:
/// `` assertion failed: SEALER.holds(..) ``.
#[test]
fn thirty_nine_passwords_land_as_sealed_secret_cells_and_a_rerun_writes_nothing() {
    let dump = thirty_nine();
    let sheets = sheets(&dump, Mode::Authoritative);
    let plan = planned(&sheets, &Cells::new());
    let text = written(&plan);
    assert!(!text.contains("synthetic-pass"), "no plaintext is written");
    let mut book = Book::new();
    let cells = apply(&mut book, &plan);
    let addresses = cell::project(&ADDRESS, &cells);
    let mut sealed = 0;
    for n in 1..=39 {
        let pk = keys::address_pk(&format!("box{n:02}@proton.example")).unwrap();
        let token = addresses.rows[&pk]
            .get("password")
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        assert!(secret::is_sealed(&token));
        let want = Secret::new(format!("synthetic-pass-{n:02}"));
        assert!(SEALER.holds(ADDRESS.name, &pk, "password", &token, &want));
        let other = keys::address_pk(&format!("box{:02}@proton.example", n % 39 + 1)).unwrap();
        assert!(!SEALER.holds(ADDRESS.name, &other, "password", &token, &want));
        sealed += 1;
    }
    assert_eq!(sealed, 39);
    assert_eq!(cell::project(&MAILBOX, &cells).rows.len(), 39);
    let again = planned(&sheets, &cells);
    assert!(again.rows.is_empty(), "{:?}", kinds(&again));
    // The same dump read fill-only, as another replica's, matches the registry.
    let replica = planned(&self::sheets(&dump, Mode::FillOnly), &cells);
    assert!(replica.rows.is_empty(), "{:?}", kinds(&replica));
    assert!(!replica.findings.iter().any(|f| f.kind == Kind::Differs));
}

/// A password that reads as a flag (`1`, `yes`, anything under six characters) or the dump's
/// redaction mark is refused with a finding by key and column, and no `Secret` cell is planned.
/// NEGATIVE CONTROL: with `want_secret` skipping `flag_like`, the `1` is sealed as a password:
/// `` left: Some(String("••••••")) right: None ``.
#[test]
fn a_flag_or_a_redaction_never_becomes_a_password() {
    let dump = emails(vec![
        with(email_row(1), &[("password", json!("1"))]),
        with(email_row(2), &[("password", json!("yes"))]),
        with(
            email_row(3),
            &[("password", json!(REDACTED)), ("has_password", json!(true))],
        ),
        with(email_row(4), &[("password", json!("abc"))]),
    ]);
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    for n in 1..=4 {
        let pk = keys::address_pk(&format!("box{n:02}@proton.example")).unwrap();
        assert_eq!(value(&plan, "address", &pk, "password"), None, "{n}");
    }
    let found = kinds(&plan);
    assert_eq!(
        found.iter().filter(|k| **k == "secret-flag-like").count(),
        3,
        "{found:?}"
    );
    assert_eq!(
        found.iter().filter(|k| **k == "secret-redacted").count(),
        1,
        "{found:?}"
    );
    let text = aterm_json::to_string(
        &plan
            .findings
            .iter()
            .map(|f| f.column.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!text.contains("yes") && !text.contains("abc"));
}

/// The emails sheet's Password column is a has-password flag: 39 rows of `1` plan no password
/// at all, though each row's address and mailbox are imported, and the flags count as 39 set.
/// NEGATIVE CONTROL: with `Sheet::parse` keeping the Password cells as they are and `emails`
/// wanting each as a `Secret`, the flags are refused as flag-like rather than absent: `` left:
/// 39 right: 0 `` (the flag-like count).
#[test]
fn a_sheets_password_flags_never_become_passwords() {
    let mut text = String::from(
        "Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n",
    );
    for n in 1..=39 {
        text.push_str(&format!("box{n:02}@proton.example,,1,Active,proton,,,\n"));
    }
    let sheet = Sheet::parse(SheetKind::Emails, "alab-emails.csv", text.as_bytes()).unwrap();
    let (flagged, set, unkeyed) = sheet.password_flags();
    assert_eq!((flagged.len(), set, unkeyed), (39, 39, 0));
    // A flag counts against the row's own address, never another column's; a row naming none
    // is counted apart, and two rows for one address are two flags.
    let mut more = text.clone();
    more.push_str(",root@proton.example,1,Active,proton,,,\n");
    more.push_str("box01@proton.example,,1,Active,proton,,,\n");
    let more = Sheet::parse(SheetKind::Emails, "alab-emails.csv", more.as_bytes()).unwrap();
    let (flagged, set, unkeyed) = more.password_flags();
    assert_eq!((flagged.len(), set, unkeyed), (39, 41, 1));
    assert!(!flagged.contains(&keys::address_pk("root@proton.example").unwrap()));
    let plan = planned(std::slice::from_ref(&sheet), &Cells::new());
    assert!(
        !plan.planned.iter().any(|p| p.column == "password"),
        "a flag is never a password"
    );
    assert_eq!(
        kinds(&plan)
            .iter()
            .filter(|k| **k == "secret-flag-like")
            .count(),
        0
    );
    assert_eq!(
        plan.planned
            .iter()
            .filter(|p| p.collection == "address" && p.column == "addr")
            .count(),
        39
    );
}

/// A fill-only source fills what is empty and never overwrites what another import wrote: a
/// second replica's different status and password, and a sheet's different provider, are each
/// a `differs` finding, recorded in the source row by collection, key and column with no value,
/// while the cells it alone states are written. An authoritative dump writes over a fill-only
/// sheet's earlier value, and a re-run of the fill-only source writes nothing more.
/// NEGATIVE CONTROL: with `decide` writing every import-written cell (`|| true` beside `own`),
/// the replica's status overwrites the dump's: `` left: Some(Write) right: Some(Kept) ``.
#[test]
fn fill_only_never_overwrites_and_a_difference_is_recorded_without_its_value() {
    let first = emails(vec![email_row(1), email_row(2)]);
    let mut book = Book::new();
    let authoritative = sheets(&first, Mode::Authoritative);
    let cells = apply(&mut book, &planned(&authoritative, &Cells::new()));
    let pk = keys::address_pk("box01@proton.example").unwrap();
    // Another replica: a different status and password on box01, a note box01 never had.
    let other = emails(vec![
        with(
            email_row(1),
            &[
                ("status", json!("disabled")),
                ("password", json!("synthetic-pass-other")),
                ("notes", json!("from the other replica")),
            ],
        ),
        email_row(2),
    ]);
    let replica: Vec<Sheet> = dump_sheets(
        "fleet-db-m22.json",
        &aterm_json::to_vec(&other).unwrap(),
        Mode::FillOnly,
        &KEY,
    )
    .unwrap();
    let plan = planned(&replica, &cells);
    assert_eq!(step(&plan, "address", &pk, "status"), Some(&Step::Kept));
    assert_eq!(step(&plan, "address", &pk, "password"), Some(&Step::Kept));
    assert_eq!(step(&plan, "address", &pk, "note"), Some(&Step::Write));
    let differs: BTreeSet<(&str, &str)> = plan
        .findings
        .iter()
        .filter(|f| f.kind == Kind::Differs)
        .map(|f| (f.pk.as_deref().unwrap(), f.column.as_deref().unwrap()))
        .collect();
    assert_eq!(
        differs,
        BTreeSet::from([(pk.as_str(), "password"), (pk.as_str(), "status")])
    );
    let recorded = plan
        .rows
        .iter()
        .flatten()
        .find(|c| c.collection == SOURCE.name && c.key.ends_with(".differs"))
        .map(|c| {
            cell::CellValue::decode(c.value.as_ref().unwrap())
                .unwrap()
                .x
        })
        .unwrap();
    assert_eq!(
        recorded,
        json!([
            {"collection": "address", "pk": pk, "column": "password", "elem": null},
            {"collection": "address", "pk": pk, "column": "status", "elem": null},
        ])
    );
    let text = written(&plan);
    assert!(!text.contains("disabled") && !text.contains("synthetic-pass"));
    let cells = apply(&mut book, &plan);
    let held = cell::project(&ADDRESS, &cells);
    assert_eq!(held.rows[&pk].get("status"), Some(&json!("active")));
    assert!(SEALER.holds(
        ADDRESS.name,
        &pk,
        "password",
        held.rows[&pk].get("password").unwrap().as_str().unwrap(),
        &Secret::new("synthetic-pass-01".into()),
    ));
    let again = planned(&replica, &cells);
    assert!(again.rows.is_empty(), "{:?}", kinds(&again));
    // A sheet (fill-only) stating another provider differs too; the authoritative dump then
    // writes over a value only the sheet wrote.
    let sheet = Sheet::parse(
        SheetKind::Emails,
        "alab-emails.csv",
        b"Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n\
          box01@proton.example,,1,Active,gmail,,,\n\
          box03@proton.example,,1,Suspended,gmail,,,\n",
    )
    .unwrap();
    let plan = planned(std::slice::from_ref(&sheet), &cells);
    assert_eq!(step(&plan, "address", &pk, "provider"), Some(&Step::Kept));
    let box03 = keys::address_pk("box03@proton.example").unwrap();
    assert_eq!(step(&plan, "address", &box03, "status"), Some(&Step::Write));
    let cells = apply(&mut book, &plan);
    let later = emails(vec![email_row(1), email_row(2), email_row(3)]);
    let plan = planned(&sheets(&later, Mode::Authoritative), &cells);
    assert_eq!(step(&plan, "address", &box03, "status"), Some(&Step::Write));
}

/// A protected row never holds a password: a dump's protected row with one is refused with a
/// finding, as is a password for a row the registry already holds protected, and `check` names
/// a protected row that holds one anyway by key and column.
/// NEGATIVE CONTROL: with `finish`'s `guarded` always false, the protected row's password is
/// planned: `` left: Some(String("••••••")) right: None ``.
#[test]
fn a_protected_row_refuses_a_password() {
    let dump = emails(vec![
        with(email_row(1), &[("protected", json!(true))]),
        email_row(2),
    ]);
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    let one = keys::address_pk("box01@proton.example").unwrap();
    let two = keys::address_pk("box02@proton.example").unwrap();
    assert_eq!(value(&plan, "address", &one, "password"), None);
    assert_eq!(
        value(&plan, "address", &one, "protected"),
        Some(&json!(true))
    );
    assert!(kinds(&plan).contains(&"protected-secret"));
    let mut book = Book::new();
    apply(&mut book, &plan);
    // The owner protects box02 after its password landed: `check` names it, and a re-import
    // writes no password there.
    let seen = book.all();
    book.write(
        1,
        &seen,
        vec![crate::typed::grid::tests::put(
            &ADDRESS,
            &two,
            "protected",
            json!(true),
        )],
    );
    let cells = book.cells();
    let (_, found) = crate::alab::checks::lab(&cells);
    assert!(found.iter().any(|f| matches!(f,
        crate::typed::check::Finding::Protected { pk, column, .. } if *pk == two && column == "password")),
        "{found:?}");
    let changed = emails(vec![
        with(email_row(1), &[("protected", json!(true))]),
        with(email_row(2), &[("password", json!("synthetic-pass-new"))]),
    ]);
    let plan = planned(&sheets(&changed, Mode::Authoritative), &cells);
    assert_eq!(step(&plan, "address", &two, "password"), None);
    assert!(kinds(&plan).contains(&"protected-secret"));
}

/// A row the replica holds concurrent values for imports only the columns they agree on: each
/// other column is a finding by key and column, never written.
/// NEGATIVE CONTROL: with `agreed` ignoring the conflict flag, the status is written: `` left:
/// Some(String("active")) right: None ``.
#[test]
fn a_replica_conflict_is_a_finding_not_a_write() {
    let other = with(email_row(1), &[("status", json!("suspended"))]);
    let dump = emails(vec![with(email_row(1), &[("_conflicts", json!([other]))])]);
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    let pk = keys::address_pk("box01@proton.example").unwrap();
    assert_eq!(value(&plan, "address", &pk, "status"), None);
    assert_eq!(
        value(&plan, "address", &pk, "provider"),
        Some(&json!("proton"))
    );
    assert!(
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::DumpConflicted && f.column.as_deref() == Some("status"))
    );
}

/// Two replicas naming one number name one phone row: the second finds the row by opening its
/// sealed `number`, so no second row is minted and nothing more is written.
/// NEGATIVE CONTROL: with `phone` skipping the opened-number lookup, the second replica mints a
/// row of its own: `` left: 2 right: 1 ``.
#[test]
fn a_phone_is_found_by_its_sealed_number() {
    let phone = |number: &str| {
        json!({"phones": [{"phone": number, "label": "phone-1", "owner_carrier": "Tello",
            "line_type": "eSIM", "device": null, "status": "active", "linked_emails": [],
            "linked_computers": [], "notes": null}]})
    };
    let dump = phone("+1 415-555-0102");
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &planned(&sheets(&dump, Mode::Authoritative), &Cells::new()),
    );
    assert_eq!(cell::project(&PHONE, &cells).rows.len(), 1);
    let other = phone("(415) 555-0102");
    let replica = dump_sheets(
        "fleet-db-m22.json",
        &aterm_json::to_vec(&other).unwrap(),
        Mode::FillOnly,
        &KEY,
    )
    .unwrap();
    let plan = planned(&replica, &cells);
    let cells = apply(&mut book, &plan);
    assert_eq!(cell::project(&PHONE, &cells).rows.len(), 1);
    assert!(!kinds(&plan).contains(&"duplicate-number"));
}

/// A file that is not a fleet_db dump is refused: not JSON, a table that is not a list of rows,
/// or a table fleet_db never had.
#[test]
fn only_a_fleet_db_dump_is_read() {
    for bad in [
        &b"not json"[..],
        br#"[1, 2]"#,
        br#"{"emails": {"a": 1}}"#,
        br#"{"emails": [1]}"#,
        br#"{"passwords": []}"#,
    ] {
        assert!(dump_sheets("dump.json", bad, Mode::FillOnly, &KEY).is_err());
    }
}

/// Every replica's dump may share a file name, so a dump is its own source by its bytes, not its
/// name: a fill-only dump named like the authoritative one it differs from differs, never
/// writes over it, while a re-run of the authoritative dump itself writes nothing.
/// NEGATIVE CONTROL: with `Sheet::wrote` matching a dump by name (as a fleet-accounts file),
/// the second replica's status is written: `` left: Some(Write) right: Some(Kept) ``.
#[test]
fn a_dump_named_like_another_is_not_its_own_source() {
    let read = |dump: &Value, mode| {
        dump_sheets("fleet.json", &aterm_json::to_vec(dump).unwrap(), mode, &KEY).unwrap()
    };
    let first = emails(vec![email_row(1)]);
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &planned(&read(&first, Mode::Authoritative), &Cells::new()),
    );
    let pk = keys::address_pk("box01@proton.example").unwrap();
    let other = emails(vec![with(
        email_row(1),
        &[
            ("status", json!("disabled")),
            ("password", json!("synthetic-pass-other")),
        ],
    )]);
    let plan = planned(&read(&other, Mode::FillOnly), &cells);
    assert_eq!(step(&plan, "address", &pk, "status"), Some(&Step::Kept));
    assert_eq!(step(&plan, "address", &pk, "password"), Some(&Step::Kept));
    assert!(!written(&plan).contains("disabled"));
    let again = planned(&read(&first, Mode::FillOnly), &cells);
    assert!(again.rows.is_empty(), "{:?}", kinds(&again));
}

/// A row the replica holds a concurrent delete for imports nothing of itself, its password
/// least of all: one finding names the row.
/// NEGATIVE CONTROL: with `row_conflict`'s result ignored by `dump_email`, the row's password
/// is planned: `` left: Some(String(..)) right: None ``.
#[test]
fn a_concurrently_deleted_row_imports_nothing() {
    let dump = emails(vec![
        with(email_row(1), &[("_conflicts", json!([{"_deleted": true}]))]),
        with(email_row(2), &[("_conflicts", json!(["unreadable"]))]),
    ]);
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    for n in [1, 2] {
        let pk = keys::address_pk(&format!("box{n:02}@proton.example")).unwrap();
        for column in ["password", "status", "provider", "mailbox"] {
            assert_eq!(value(&plan, "address", &pk, column), None, "{column}");
        }
        assert!(plan.findings.iter().any(|f| f.kind == Kind::DumpConflicted
            && f.pk.as_deref() == Some(pk.as_str())
            && f.column.is_none()));
    }
    assert!(!written(&plan).contains("synthetic-pass"));
}

/// A `protected` flag the replica holds concurrent values for guards the row as a conflicted
/// registry cell does: the password is refused, even where the row's own value is empty, and
/// the flag itself is not written.
/// NEGATIVE CONTROL: with `dump_email` skipping its `read.conflicted("protected")` guard, the
/// password is planned: `` left: Some(String(..)) right: None ``.
#[test]
fn a_conflicted_protected_flag_refuses_the_password() {
    for (own, theirs) in [(json!(false), json!(true)), (json!(null), json!(true))] {
        let other = with(email_row(1), &[("protected", theirs)]);
        let dump = emails(vec![with(
            email_row(1),
            &[("protected", own), ("_conflicts", json!([other]))],
        )]);
        let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
        let pk = keys::address_pk("box01@proton.example").unwrap();
        assert_eq!(value(&plan, "address", &pk, "password"), None);
        assert_eq!(value(&plan, "address", &pk, "protected"), None);
        assert!(plan.findings.iter().any(|f| f.kind == Kind::DumpConflicted
            && f.column.as_deref() == Some("protected")));
    }
}

/// An account row whose tier the replica holds concurrent values for decides nothing: no
/// account, region, flow or status is planned from it, only a finding.
/// NEGATIVE CONTROL: with `dump_account`'s conflicted-column refusal dropped, the account is
/// planned: `` assertion failed: !plan.planned.iter().any(|p| p.collection == "account") ``.
#[test]
fn a_conflicted_tier_decides_no_account() {
    let mut dump = every_column();
    let account = dump["ai_accounts"].get_index(0).unwrap().clone();
    let other = with(account.clone(), &[("tier", json!("2"))]);
    let accounts = json!([with(account, &[("_conflicts", json!([other]))])]);
    dump.as_object_mut()
        .unwrap()
        .insert("ai_accounts".to_owned(), accounts);
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    assert!(!plan.planned.iter().any(|p| p.collection == "account"));
    assert!(
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::DumpConflicted && f.column.as_deref() == Some("tier"))
    );
}

fn reports_conflict(plan: &Plan, column: &str) -> bool {
    plan.findings
        .iter()
        .any(|f| f.kind == Kind::DumpConflicted && f.column.as_deref() == Some(column))
}

/// A concurrent value is a disagreement whichever of the two a row shows, its own cell empty or
/// not, in every column the registry could carry, so the digest order cannot decide `covered`.
/// Absent, null and blank say nothing, and a column carried nowhere is no disagreement.
/// NEGATIVE CONTROL: with `dump` not calling `disagreements`, an empty cell beside a note is
/// recorded covered: `` notes [] ``.
#[test]
fn a_concurrent_value_is_a_finding_whichever_value_the_row_shows() {
    let email_cases = [
        ("notes", json!("one Mac's note")),
        ("status", json!("suspended")),
        ("phone", json!("+1 415-555-0102")),
        ("webmail_url", json!("https://mail.example.invalid/")),
        ("root_account", json!("box02@proton.example")),
        ("provider", json!("gmail")),
        ("services", json!(["claude"])),
        ("assigned_computers", json!(["m3"])),
    ];
    for (column, text) in email_cases {
        let full = with(email_row(1), &[(column, text)]);
        let empty = with(email_row(1), &[(column, json!(null))]);
        for (own, other) in [(&empty, &full), (&full, &empty)] {
            let own = with(own.clone(), &[("_conflicts", json!([other]))]);
            let plan = planned(
                &sheets(&emails(vec![own]), Mode::Authoritative),
                &Cells::new(),
            );
            assert!(
                reports_conflict(&plan, column),
                "{column} {:?}",
                kinds(&plan)
            );
        }
    }
    let phone = |label: &str, linked: Value| {
        json!({"phone": "+1 415-555-0102", "label": label, "status": "active",
            "linked_emails": linked})
    };
    for (column, own, other) in [
        ("label", phone("alpha", json!([])), phone("beta", json!([]))),
        (
            "linked_emails",
            phone("alpha", json!([])),
            phone("alpha", json!(["box01@proton.example"])),
        ),
    ] {
        let own = with(own, &[("_conflicts", json!([other]))]);
        let plan = planned(
            &sheets(&json!({"phones": [own]}), Mode::Authoritative),
            &Cells::new(),
        );
        assert!(
            reports_conflict(&plan, column),
            "{column} {:?}",
            kinds(&plan)
        );
    }
    let computer = |column: &str, value: Value| {
        with(
            json!({"label": "m40", "status": "active"}),
            &[(column, value)],
        )
    };
    let own = with(
        computer("assigned_emails", json!([])),
        &[(
            "_conflicts",
            json!([computer("assigned_emails", json!(["box01@proton.example"]))]),
        )],
    );
    let plan = planned(
        &sheets(&json!({"computers": [own]}), Mode::Authoritative),
        &Cells::new(),
    );
    assert!(
        reports_conflict(&plan, "assigned_emails"),
        "{:?}",
        kinds(&plan)
    );
    let own = with(
        computer("ip", json!("192.0.2.1")),
        &[("_conflicts", json!([computer("ip", json!("192.0.2.2"))]))],
    );
    let plan = planned(
        &sheets(&json!({"computers": [own]}), Mode::Authoritative),
        &Cells::new(),
    );
    assert!(!reports_conflict(&plan, "ip"), "{:?}", kinds(&plan));

    let mut absent = email_row(1);
    absent.as_object_mut().unwrap().remove("notes");
    for other in [absent, with(email_row(1), &[("notes", json!("   "))])] {
        let own = with(email_row(1), &[("_conflicts", json!([other]))]);
        let plan = planned(
            &sheets(&emails(vec![own]), Mode::Authoritative),
            &Cells::new(),
        );
        assert!(
            !kinds(&plan).contains(&"dump-conflicted"),
            "{:?}",
            kinds(&plan)
        );
    }
}

/// A row's kind the replica holds concurrent values for decides nothing, whichever it shows: not
/// an alias link, a kind, a mailbox or where its password goes.
/// NEGATIVE CONTROL: with `undecided` always false, the row showing `alias` is linked to its root
/// and the row showing `root` mints a mailbox: `` left: Some(..) right: None ``.
#[test]
fn a_conflicted_account_type_decides_nothing() {
    let root = with(
        email_row(1),
        &[("root_account", json!("box02@proton.example"))],
    );
    let alias = with(root.clone(), &[("account_type", json!("alias"))]);
    let pk = keys::address_pk("box01@proton.example").unwrap();
    for (own, other) in [(&root, &alias), (&alias, &root)] {
        let own = with(own.clone(), &[("_conflicts", json!([other]))]);
        let plan = planned(
            &sheets(&emails(vec![own]), Mode::Authoritative),
            &Cells::new(),
        );
        for column in ["kind", "alias_of", "mailbox", "password"] {
            assert_eq!(value(&plan, "address", &pk, column), None, "{column}");
        }
        assert!(
            reports_conflict(&plan, "account_type"),
            "{:?}",
            kinds(&plan)
        );
    }
}

/// The address an account signs in with decides its row: one the replica holds concurrent values
/// for makes no account and no address.
/// NEGATIVE CONTROL: with `dump_account`'s `email` refusal dropped, the digest order picks the
/// login: `` assertion failed: !plan.planned.iter().any(|p| p.collection == "account") ``.
#[test]
fn a_conflicted_account_email_decides_no_login() {
    let mut dump = every_column();
    let account = dump["ai_accounts"].get_index(0).unwrap().clone();
    let other = with(account.clone(), &[("email", json!("other@proton.example"))]);
    let accounts = json!([with(account, &[("_conflicts", json!([other]))])]);
    dump.as_object_mut()
        .unwrap()
        .insert("ai_accounts".to_owned(), accounts);
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    assert!(!plan.planned.iter().any(|p| p.collection == "account"));
    assert!(reports_conflict(&plan, "email"), "{:?}", kinds(&plan));
    let other = keys::address_pk("other@proton.example").unwrap();
    assert_eq!(value(&plan, "address", &other, "addr"), None);
}

/// What the plan takes back: every write of nothing, an unset element or an ended row.
fn withdrawn(plan: &Plan) -> Vec<String> {
    plan.planned
        .iter()
        .filter(|p| {
            p.step == Step::Write && (p.x == json!("ended") || p.x.is_null() || p.x == json!(false))
        })
        .map(|p| format!("{}.{}={}", p.collection, p.column, p.x))
        .collect()
}

/// `earlier` as sheets an import read under `later`'s keys: a later build's read of the same bytes.
fn under_the_keys_of(earlier: &mut [Sheet], later: &[Sheet]) {
    for (sheet, same) in earlier.iter_mut().zip(later) {
        sheet.source.clone_from(&same.source);
        sheet.sha256.clone_from(&same.sha256);
    }
}

/// A dump of this one computer row and no other table's.
fn computers(row: Value) -> Value {
    json!({"emails": [], "computers": [row], "phones": [], "ai_accounts": []})
}

/// What a sheet does not state may be what a disagreement in it hides, so a sheet with one takes
/// nothing back: no cell or set element, and no preference, of any row an earlier read of it wrote.
/// NEGATIVE CONTROL: with `ReadNow::settled` always true:
/// `` assertion `left == right` failed: login `` and `` left: ["preference.row=\"ended\""] ``.
#[test]
fn a_conflict_takes_back_nothing_an_earlier_read_stated() {
    let account = json!({
        "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
        "email": "box01@proton.example", "receiving_mailbox": "box01@proton.example",
        "assigned_computers": ["m3"], "tier": "1", "status": "active", "notes": "max plan",
    });
    let computer = json!({
        "label": "m3", "status": "active", "location": "SF", "aliases": "m4",
        "assigned_accounts": [],
    });
    let dump = |accounts: Value, computer: Value| {
        json!({
            "emails": [email_row(1)], "phones": [], "computers": [computer],
            "ai_accounts": accounts,
        })
    };
    let concurrent = |row: &Value, changes: &[(&str, Value)]| {
        with(
            row.clone(),
            &[("_conflicts", json!([with(row.clone(), changes)]))],
        )
    };
    let on_account =
        |changes: &[(&str, Value)]| dump(json!([concurrent(&account, changes)]), computer.clone());
    let shown = dump(json!([account.clone()]), computer.clone());
    let receives = with(
        account.clone(),
        &[("receiving_mailbox", json!("box02@proton.example"))],
    );
    let unassigned = with(account.clone(), &[("assigned_computers", json!([]))]);
    let assigned = with(
        computer.clone(),
        &[("assigned_accounts", json!(["claude:box01@proton.example"]))],
    );
    // A login with a password states its own mailbox, so only a row without one leaves the cell to
    // the `ai_accounts` row these two cases read.
    let bare = |accounts: Value| {
        json!({
            "emails": [with(email_row(1), &[("password", json!(null))])],
            "phones": [], "computers": [computer.clone()], "ai_accounts": accounts,
        })
    };
    let cases = [
        (
            "login",
            shown.clone(),
            on_account(&[("email", json!("other@proton.example"))]),
        ),
        ("tier", shown.clone(), on_account(&[("tier", json!("2"))])),
        (
            "placement",
            shown.clone(),
            on_account(&[("assigned_computers", json!(["m5"]))]),
        ),
        (
            "delete",
            shown.clone(),
            on_account(&[("_deleted", json!(true))]),
        ),
        (
            "aliases",
            shown.clone(),
            dump(
                json!([account.clone()]),
                concurrent(&computer, &[("aliases", json!("m6"))]),
            ),
        ),
        (
            "machine placement",
            dump(json!([unassigned.clone()]), assigned.clone()),
            dump(
                json!([unassigned.clone()]),
                concurrent(&assigned, &[("assigned_accounts", json!([]))]),
            ),
        ),
        (
            "a mailbox beside a login",
            bare(json!([receives.clone()])),
            bare(json!([concurrent(
                &receives,
                &[("email", json!("other@proton.example"))]
            )])),
        ),
        (
            "mailbox from another row",
            bare(json!([receives.clone()])),
            bare(json!([concurrent(
                &receives,
                &[("receiving_mailbox", json!("box03@proton.example"))]
            )])),
        ),
        (
            "kind",
            emails(vec![email_row(1), email_row(2)]),
            emails(vec![
                concurrent(
                    &email_row(1),
                    &[
                        ("account_type", json!("alias")),
                        ("root_account", json!("box02@proton.example")),
                    ],
                ),
                email_row(2),
            ]),
        ),
    ];
    for (case, shown, conflicted) in cases {
        let conflicted = sheets(&conflicted, Mode::Authoritative);
        let mut earlier = sheets(&shown, Mode::Authoritative);
        under_the_keys_of(&mut earlier, &conflicted);
        let mut book = Book::new();
        let cells = apply(&mut book, &planned(&earlier, &Cells::new()));
        let plan = planned(&conflicted, &cells);
        assert!(
            kinds(&plan).contains(&"dump-conflicted"),
            "{case}: {:?}",
            kinds(&plan)
        );
        assert_eq!(withdrawn(&plan), Vec::<String>::new(), "{case}");
    }
}

/// A disagreement in one sheet stops the sources read beside it too: what they no longer state may
/// be what it hides. Alone, the same read takes back.
/// NEGATIVE CONTROL: with `ReadNow::settled` always true:
/// `` left: ["machine.aliases.m4=false"] right: [] ``.
#[test]
fn a_disagreement_takes_nothing_back_from_a_source_read_beside_it() {
    let own = json!({"label": "m3", "status": "active", "location": "SF"});
    let later = sheets(&computers(own.clone()), Mode::Authoritative);
    let mut earlier = sheets(
        &computers(with(own, &[("aliases", json!("m4"))])),
        Mode::Authoritative,
    );
    under_the_keys_of(&mut earlier, &later);
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&earlier, &Cells::new()));
    assert_eq!(
        withdrawn(&planned(&later, &cells)),
        ["machine.aliases.m4=false"]
    );

    let other = json!({"label": "m5", "status": "active", "location": "NY"});
    let disputed = with(
        other.clone(),
        &[(
            "_conflicts",
            json!([with(other, &[("location", json!("LA"))])]),
        )],
    );
    let beside = dump_sheets(
        "fleet-db-m105.json",
        &aterm_json::to_vec(&computers(disputed)).unwrap(),
        Mode::Authoritative,
        &KEY,
    )
    .unwrap();
    let read: Vec<Sheet> = later.into_iter().chain(beside).collect();
    let plan = planned(&read, &cells);
    assert!(
        kinds(&plan).contains(&"dump-conflicted"),
        "{:?}",
        kinds(&plan)
    );
    assert_eq!(withdrawn(&plan), Vec::<String>::new());
}

/// A receiving address several rows say reads as nothing, and the sheet still states it, so the
/// mailbox it gave stays until the address is settled.
/// NEGATIVE CONTROL: with the `address_unread` term gone from `finish`:
/// `` left: ["address.mailbox=null"] right: [] ``.
#[test]
fn an_ambiguous_receiving_address_takes_no_mailbox_back() {
    let read = sheets(
        &json!({
            "emails": [with(email_row(1), &[("password", json!(null))])],
            "phones": [], "computers": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "receiving_mailbox": "box02@proton.example",
                "tier": "1", "status": "active",
            }],
        }),
        Mode::Authoritative,
    );
    let says = |name: &str| {
        let pk = keys::address_pk(name).unwrap();
        vec![
            put(&ADDRESS, &pk, "addr", json!("box02@proton.example")),
            put(&ADDRESS, &pk, "row", json!("live")),
        ]
    };
    let mut book = Book::new();
    book.write(2, &[], says("cy@proton.example"));
    let held = book.cells();
    let cells = apply(&mut book, &planned(&read, &held));
    let login = keys::address_pk("box01@proton.example").unwrap();
    assert!(
        cell::project(&ADDRESS, &cells).rows[&login]
            .get("mailbox")
            .is_some_and(|mailbox| mailbox.as_str().is_some())
    );
    let seen = book.all();
    book.write(2, &seen, says("dan@proton.example"));
    let plan = planned(&read, &book.cells());
    assert!(
        kinds(&plan).contains(&"address-ambiguous"),
        "{:?}",
        kinds(&plan)
    );
    assert_eq!(withdrawn(&plan), Vec::<String>::new());
}

/// One list cell made of two columns: a disagreement on either leaves it as it is, rather than
/// writing what the other column alone says.
/// NEGATIVE CONTROL: with `dump_computer` writing the list anyway: the panic names `aliases`.
#[test]
fn a_disagreement_on_either_nickname_column_leaves_the_list() {
    let computer = json!({
        "label": "m3", "status": "active", "aliases": "The Studio", "nickname_aliases": "studio",
    });
    for (column, value) in [("aliases", "Old Studio"), ("nickname_aliases", "old")] {
        let other = with(computer.clone(), &[(column, json!(value))]);
        let own = with(computer.clone(), &[("_conflicts", json!([other]))]);
        let plan = planned(&sheets(&computers(own), Mode::Authoritative), &Cells::new());
        assert!(
            !plan.planned.iter().any(|p| p.column == "nickname_aliases"),
            "{column}"
        );
        assert!(reports_conflict(&plan, column), "{:?}", kinds(&plan));
    }
}

/// A concurrent delete is reported on a row whose read stops before it looks: a tier that holds no
/// membership. The mark is otherwise unseen, and it stops the whole run taking back.
/// NEGATIVE CONTROL: with `disagreements` skipping a whole-row mark, the panic prints `` [] ``.
#[test]
fn a_concurrent_delete_is_reported_on_a_row_its_read_skips() {
    let own = json!({
        "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
        "email": "box01@proton.example", "tier": "0", "status": "active",
        "_conflicts": [{"_deleted": true}],
    });
    let plan = planned(
        &sheets(
            &json!({"emails": [], "phones": [], "computers": [], "ai_accounts": [own]}),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    assert!(
        kinds(&plan).contains(&"dump-conflicted"),
        "{:?}",
        kinds(&plan)
    );
}

/// A protected row holds no mailbox, so a receiving address beside it is reported unless the row's
/// link to its root says it; one no key can read is reported on any row. Each is what the registry
/// would otherwise never hold, with the replica reading as covered.
/// NEGATIVE CONTROL: with `dump_account` passing a protected row's address over: the first
/// assertion fails.
#[test]
fn a_receiving_mailbox_nothing_holds_is_reported() {
    let reported = |row: Value, receiving: &str| {
        let plan = planned(
            &sheets(
                &json!({
                    "emails": [row], "phones": [], "computers": [],
                    "ai_accounts": [{
                        "id": "claude:box01@proton.example", "provider": "anthropic",
                        "service": "claude", "email": "box01@proton.example",
                        "receiving_mailbox": receiving, "tier": "1", "status": "active",
                    }],
                }),
                Mode::Authoritative,
            ),
            &Cells::new(),
        );
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::Unmapped && f.column.as_deref() == Some("receiving_mailbox"))
    };
    let plain = with(email_row(1), &[("password", json!(null))]);
    let protected = with(plain.clone(), &[("protected", json!(true))]);
    let alias = with(
        protected.clone(),
        &[
            ("account_type", json!("alias")),
            ("root_account", json!("box02@proton.example")),
        ],
    );
    assert!(reported(protected.clone(), "box02@proton.example"));
    assert!(!reported(protected, "box01@proton.example"));
    assert!(!reported(alias.clone(), "box02@proton.example"));
    assert!(reported(alias, "box03@proton.example"));
    assert!(reported(plain.clone(), "not an address"));
    assert!(!reported(plain, "box02@proton.example"));
}

/// A placeholder stays `planned` when the same bytes are read again: fleet_db called every row
/// `active`, so an import of an unknown machine settles at once. Any other word moves it.
/// NEGATIVE CONTROL: with `says_nothing` always false, the row is born `active`: `left:
/// Some(String("active")) right: Some(String("planned"))`.
#[test]
fn a_placeholder_stays_planned_when_read_again() {
    let computer = |status: &str| computers(json!({"label": "m3", "status": status}));
    let read = sheets(&computer("active"), Mode::Authoritative);
    let mut book = Book::new();
    let first = planned(&read, &Cells::new());
    assert_eq!(
        value(&first, "machine", &keys::machine_pk("m3"), "status"),
        Some(&json!("planned"))
    );
    let cells = apply(&mut book, &first);
    let again = planned(&read, &cells);
    assert!(again.rows.is_empty(), "{:?}", again.planned);
    let moved = planned(&sheets(&computer("parked"), Mode::Authoritative), &cells);
    assert!(
        moved
            .planned
            .iter()
            .any(|p| p.column == "status" && p.x == json!("parked")),
        "{:?}",
        moved.planned
    );
}

/// A row with no key holds what no registry row can, so it is reported in each table that has one.
/// NEGATIVE CONTROL: with `dump_computer` returning silently: the panic names `computers`.
#[test]
fn a_row_with_no_key_is_reported() {
    for table in ["computers", "phones"] {
        let mut dump = emails(Vec::new());
        dump.as_object_mut()
            .unwrap()
            .insert(table.to_owned(), json!([{"notes": "a note"}]));
        let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
        assert!(
            plan.findings.iter().any(|f| f.kind == Kind::FreeText),
            "{table}"
        );
    }
}

/// A column the design carries nowhere hides nothing a read could state, so a disagreement on it
/// leaves its sheet able to take back what an earlier read of it wrote.
/// NEGATIVE CONTROL: with `dump_sheets` marking a dropped column, `disagreements` names it and the
/// sheet takes nothing back: the first assertion fails with `` ["dump-conflicted"] ``.
#[test]
fn a_disagreement_on_a_column_nothing_carries_still_takes_back() {
    let own = json!({"label": "m3", "status": "active", "location": "SF", "ip": "192.0.2.1"});
    let other = with(own.clone(), &[("ip", json!("192.0.2.2"))]);
    let later = sheets(
        &computers(with(own.clone(), &[("_conflicts", json!([other]))])),
        Mode::Authoritative,
    );
    let mut earlier = sheets(
        &computers(with(own, &[("aliases", json!("m4"))])),
        Mode::Authoritative,
    );
    under_the_keys_of(&mut earlier, &later);
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&earlier, &Cells::new()));
    let plan = planned(&later, &cells);
    assert!(
        !kinds(&plan).contains(&"dump-conflicted"),
        "{:?}",
        kinds(&plan)
    );
    assert_eq!(withdrawn(&plan), ["machine.aliases.m4=false"]);
}

/// A column this build does not name may be a credential by a word no list holds, so its text never
/// enters the sheet a review reads. Its row still counts as holding something, and two such columns
/// stay two findings, even where the guard refuses both names.
/// NEGATIVE CONTROL: with `dump_sheets` keeping the cell's text, the sheet holds it: the assertion
/// over the cells fails with `` green heron lake ``.
#[test]
fn a_column_this_build_does_not_name_stays_out_of_the_sheet() {
    let other = with(
        email_row(1),
        &[
            ("recovery_phrase", json!("green heron lake")),
            ("passphrase", json!("slow amber wind")),
            ("passcode", json!("tall quiet stone")),
        ],
    );
    let row = with(
        email_row(1),
        &[
            ("recovery_phrase", json!("green heron lake")),
            ("passphrase", json!("blue otter river")),
            ("passcode", json!("grey silent reef")),
            ("_conflicts", json!([other])),
        ],
    );
    let keyless = json!({"recovery_phrase": "green heron lake"});
    let read = sheets(&emails(vec![row, keyless]), Mode::Authoritative);
    let sheet = read
        .iter()
        .find(|sheet| sheet.kind == SheetKind::FleetDb(Dump::Emails))
        .unwrap();
    assert_eq!(sheet.rows.len(), 2);
    let cells = sheet.rows.iter().flatten().cloned().collect::<Vec<_>>();
    for phrase in ["green heron lake", "blue otter river", "grey silent reef"] {
        assert!(!cells.contains(&phrase.to_owned()), "{phrase}");
    }
    let plan = planned(&read, &Cells::new());
    let refused = plan
        .findings
        .iter()
        .filter(|f| f.kind == Kind::DumpConflicted && f.column.is_none())
        .collect::<Vec<_>>();
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(refused.iter().all(|f| f.at.len() == 1), "{refused:?}");
    assert!(
        plan.findings
            .iter()
            .any(|f| f.at.iter().any(|at| at.row == 3)),
        "{:?}",
        kinds(&plan)
    );
}

/// A concurrent password is one finding that names its column, whichever values both sides hold.
/// NEGATIVE CONTROL: with `disagreements` passing `password` through the guard:
/// `` left: [None] right: [Some("password")] ``.
#[test]
fn a_conflicted_password_is_listed_once_by_its_column() {
    for (own, other) in [
        (json!(null), json!("1")),
        (json!("synthetic-pass-a"), json!("synthetic-pass-b")),
    ] {
        let other = with(email_row(1), &[("password", other)]);
        let row = with(
            email_row(1),
            &[("password", own), ("_conflicts", json!([other]))],
        );
        let plan = planned(
            &sheets(&emails(vec![row]), Mode::Authoritative),
            &Cells::new(),
        );
        let columns: Vec<Option<&str>> = plan
            .findings
            .iter()
            .filter(|f| f.kind == Kind::DumpConflicted)
            .map(|f| f.column.as_deref())
            .collect();
        assert_eq!(columns, [Some("password")]);
    }
}

/// A column only a concurrent value holds is carried or reported as one a row holds, whichever
/// value the digest order showed; only what a read adds is no column. A concurrent value's cell
/// never enters the sheet.
/// NEGATIVE CONTROL: with `dump_sheets` reading headers from the rows alone, it is not reported:
/// `` assertion failed: unmapped(&plan) ``.
#[test]
fn a_column_only_a_concurrent_value_holds_is_reported() {
    let unmapped = |plan: &Plan| {
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::Unmapped && f.collection == Some("address"))
    };
    let other = with(
        email_row(1),
        &[("recovery_password", json!("synthetic-recovery-1"))],
    );
    let own = with(email_row(1), &[("_conflicts", json!([other]))]);
    let sheet = sheets(&emails(vec![own]), Mode::Authoritative);
    let plan = planned(&sheet, &Cells::new());
    assert!(unmapped(&plan));
    assert!(!written(&plan).contains("synthetic-recovery"));

    let own = with(
        email_row(1),
        &[("_recovery_password", json!("synthetic-1"))],
    );
    let plan = planned(
        &sheets(&emails(vec![own]), Mode::Authoritative),
        &Cells::new(),
    );
    assert!(unmapped(&plan));

    let read = with(
        email_row(1),
        &[
            ("_conflicts", json!([{"_deleted": true}])),
            ("has_password", json!(true)),
        ],
    );
    let sheet = sheets(&emails(vec![read]), Mode::Authoritative);
    for added in ["_conflicts", "_deleted", "has_password"] {
        assert!(!sheet[0].headers.iter().any(|h| h == added), "{added}");
    }
    let dump = json!({"computers": [{"label": "m40", "has_password": "synthetic-1"}]});
    let sheet = sheets(&dump, Mode::Authoritative);
    assert!(sheet[0].headers.iter().any(|h| h == "has_password"));
}

/// A none word claims no password and no login, in a replica's cell as in a sheet's: it makes no
/// mailbox, and is judged against the registry where a replica holds it.
/// NEGATIVE CONTROL: with `dump_email`'s none-word arm dropped, `0` mints a mailbox:
/// `` left: Some(String("mb-3cf52c7f9324e875")) right: None ``.
#[test]
fn a_none_word_makes_no_mailbox() {
    let dump = emails(vec![
        with(email_row(1), &[("password", json!("0"))]),
        with(email_row(2), &[("password", json!(null))]),
        with(
            email_row(3),
            &[
                ("password", json!("none")),
                ("account_type", json!("alias")),
                ("root_account", json!("box02@proton.example")),
            ],
        ),
    ]);
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    let pk = |n: usize| keys::address_pk(&format!("box{n:02}@proton.example")).unwrap();
    assert_eq!(value(&plan, "address", &pk(1), "mailbox"), None);
    assert_eq!(value(&plan, "address", &pk(2), "mailbox"), None);
    assert!(
        !plan
            .findings
            .iter()
            .any(|f| f.kind == Kind::Unmapped && f.column.as_deref() == Some("password")),
        "{:?}",
        kinds(&plan)
    );
    assert_eq!(
        kinds(&plan)
            .iter()
            .filter(|k| **k == "secret-flag-like")
            .count(),
        2,
        "{:?}",
        kinds(&plan)
    );

    let text = "Email Address,Root Account,Password,Status,Provider,Activation Phone Number,\
                Protection Flag,Notes\nbox01@proton.example,,0,Active,proton,,,\n\
                box02@proton.example,,1,Active,proton,,,\n";
    let sheet = Sheet::parse(SheetKind::Emails, "alab-emails.csv", text.as_bytes()).unwrap();
    assert_eq!(sheet.password_flags().1, 1);
    let plan = planned(std::slice::from_ref(&sheet), &Cells::new());
    assert_eq!(value(&plan, "address", &pk(1), "mailbox"), None);
    assert!(value(&plan, "address", &pk(2), "mailbox").is_some());
}

/// A list column is read item by item: an alias holding a space stays one nickname alias, never
/// split into another Mac's label, and a list fleet_db stored as a `,`/`;` string is its items.
/// NEGATIVE CONTROL: with `items` splitting on whitespace as well, `old m12` names m12 as one of
/// m40's aliases: `` left: Some(Bool(true)) right: None ``.
#[test]
fn a_list_column_is_read_item_by_item() {
    let computer = |aliases: Value, nicknames: Value| {
        json!({"computers": [{"label": "m40", "hostname": "m40.local", "aliases": aliases,
            "nickname_aliases": nicknames, "status": "active"}]})
    };
    let machine = keys::machine_pk("m40");
    let plan = planned(
        &sheets(
            &computer(json!(["old m12", "m41"]), json!(["Red Balloon"])),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    assert_eq!(value(&plan, "machine", &machine, "aliases.m12"), None);
    assert_eq!(
        value(&plan, "machine", &machine, "aliases.m41"),
        Some(&json!(true))
    );
    assert_eq!(
        value(&plan, "machine", &machine, "nickname_aliases"),
        Some(&json!(["old m12", "Red Balloon"]))
    );
    let plan = planned(
        &sheets(
            &computer(json!("m41, m42; m43"), json!(null)),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    for alias in ["m41", "m42", "m43"] {
        assert_eq!(
            value(&plan, "machine", &machine, &format!("aliases.{alias}")),
            Some(&json!(true)),
            "{alias}"
        );
    }
}

/// A number whose phone row the owner retired is found there and left retired, as a retired
/// address is: no second row is minted for it.
/// NEGATIVE CONTROL: with `phone` reading only live rows, the import mints a second row: ``
/// left: 2 right: 1 ``.
#[test]
fn a_retired_phones_number_is_not_minted_again() {
    let dump = json!({"phones": [{"phone": "+1 415-555-0103", "label": "phone-1",
        "owner_carrier": "Tello", "line_type": "eSIM", "device": null, "status": "active",
        "linked_emails": [], "linked_computers": [], "notes": null}]});
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &planned(&sheets(&dump, Mode::Authoritative), &Cells::new()),
    );
    let pk = cell::project(&PHONE, &cells)
        .rows
        .keys()
        .next()
        .unwrap()
        .clone();
    let seen = book.all();
    book.write(
        1,
        &seen,
        vec![crate::typed::grid::tests::put(
            &PHONE,
            &pk,
            crate::typed::LIFECYCLE,
            json!("ended"),
        )],
    );
    let cells = book.cells();
    let plan = planned(&sheets(&dump, Mode::Authoritative), &cells);
    let cells = apply(&mut book, &plan);
    assert_eq!(cell::project(&PHONE, &cells).rows.len(), 1);
    assert!(kinds(&plan).contains(&"retired-kept"), "{:?}", kinds(&plan));
}

/// A dump's source is keyed under the `[everyone]` key: no plain digest of its bytes, nor of any
/// prefix of them, reaches the shared `source` row, so a reader of that row cannot test a guessed
/// password against it, while a dump that differs only in one password is still a new source
/// with its own digest, and another epoch's key names it differently.
/// NEGATIVE CONTROL: with `keyed_source` a plain `Sha256::digest(bytes)` (the key ignored), the
/// plain-digest assertion fails: `` left: "…" right: "…" `` under `assert_ne!`.
#[test]
fn a_dump_source_is_a_keyed_digest_that_still_tells_a_password_change() {
    use aterm_digest::Sha256;
    let one = emails(vec![email_row(1)]);
    let two = emails(vec![with(
        email_row(1),
        &[("password", json!("synthetic-pass-91"))],
    )]);
    let read = |dump: &Value, key: &[u8; 32]| {
        dump_sheets(
            "fleet-db-m104.json",
            &aterm_json::to_vec(dump).unwrap(),
            Mode::FillOnly,
            key,
        )
        .unwrap()
        .into_iter()
        .find(|sheet| sheet.name.ends_with("#emails"))
        .unwrap()
    };
    let (a, b) = (read(&one, &KEY), read(&two, &KEY));
    // Only a password differs, and the sources differ: a change the `source` row tells.
    assert_ne!(a.source, b.source);
    assert_ne!(a.sha256, b.sha256);
    assert!(a.source.starts_with("sr-") && a.sha256.starts_with(&a.source[3..]));
    // Neither is the plain digest of the canonical bytes the source is read from.
    let canonical = aterm_json::to_vec(&json!({
        "file": "fleet-db-m104.json#emails",
        "rows": one.get("emails").unwrap(),
    }))
    .unwrap();
    assert_ne!(a.sha256, hex::encode_lower(&Sha256::digest(&canonical)));
    assert_ne!(a.source, keys::source_pk(&canonical));
    // Keyed: the same dump under another epoch's key is another digest, and the same key and
    // bytes name one source on every Mac.
    let other = read(&one, &[0xa5; 32]);
    assert_ne!(a.sha256, other.sha256);
    assert_eq!(read(&one, &KEY).sha256, a.sha256);
}

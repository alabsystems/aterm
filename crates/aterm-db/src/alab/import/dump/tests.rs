// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A fleet_db replica's dump as import sources, over hand-built histories: synthetic dumps of
//! the shape `aterm db dump <file> --show-passwords` writes (every address `example.invalid` or
//! `proton.example`, every password `synthetic-pass-<n>`, every number in `555-01xx`).

use super::super::tests::{SEALER, Stand};
use super::*;
use crate::{
    alab::{
        import::{Input, Plan, Step, Terms, Vendors, plan},
        registry::{ADDRESS, MACHINE as MACHINE_TABLE, MAILBOX, PHONE, SOURCE},
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

/// An address no row can be found for may be the one that states what a row holds of it, so with a
/// logins sheet read beside the dump no part of the row is taken back: its link and kind as its
/// mailbox.
/// NEGATIVE CONTROL: with the `address_unread` term reading only `mailbox`:
/// `` left: ["address.alias_of=null", "address.kind=null"] right: [] ``.
#[test]
fn an_ambiguous_root_takes_nothing_of_the_row_back() {
    let mut read = sheets(
        &emails(vec![with(
            email_row(1),
            &[
                ("password", json!(null)),
                ("account_type", json!("alias")),
                ("root_account", json!("box02@proton.example")),
            ],
        )]),
        Mode::Authoritative,
    );
    let records = vec![
        vec!["email".to_owned()],
        vec!["other@proton.example".to_owned()],
    ];
    read.push(
        Sheet::from_records_with_bytes(SheetKind::Logins, "logins.csv", records, b"logins")
            .unwrap(),
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
    apply(&mut book, &planned(&read, &held));
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

/// What a plan of `read` reports by no cell: each `(collection, column)` of an `unmapped` finding.
fn unmapped(plan: &Plan) -> Vec<(&'static str, String)> {
    plan.findings
        .iter()
        .filter(|f| f.kind == Kind::Unmapped)
        .map(|f| {
            (
                f.collection.unwrap_or_default(),
                f.column.clone().unwrap_or_default(),
            )
        })
        .collect()
}

/// What the replica `dump` leaves uncovered once its own import is in the registry.
fn left_uncovered(dump: &Value, mode: Mode) -> Vec<String> {
    let read = sheets(dump, mode);
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&read, &Cells::new()));
    super::super::covered::judge(&planned(&read, &cells), &|_| false)
        .into_iter()
        .map(|cell| format!("{}:{:?}", cell.kind, cell.column.as_deref()))
        .collect()
}

/// A phones sheet of these records, as `~/.secrets/alab-phones.csv` beside a replica.
fn phones_sheet(records: Vec<Vec<String>>) -> Sheet {
    let bytes = records
        .iter()
        .map(|row| row.join(","))
        .collect::<Vec<_>>()
        .join("\n");
    Sheet::from_records_with_bytes(
        SheetKind::Phones,
        "alab-phones.csv",
        records,
        bytes.as_bytes(),
    )
    .unwrap()
}

/// One `phones` row, its number the synthetic `415-555-0101`.
fn phones(row: Value) -> Value {
    json!({
        "phones": [with(json!({"phone": "415-555-0101"}), row.as_object().unwrap().iter()
            .map(|(k, v)| (k.as_str(), v.clone())).collect::<Vec<_>>().as_slice())],
        "emails": [], "computers": [], "ai_accounts": [],
    })
}

/// A cell a guard refuses is in no registry cell, so it keeps the replica uncovered, by its column:
/// a coordinate alone would let a covered replica be deleted with the value in it.
/// NEGATIVE CONTROL: with `refused` keeping only the coordinate, every case is covered:
/// `` left: [] right: ["unmapped:Some(\"chip\")"] ``.
#[test]
fn a_cell_a_guard_refuses_keeps_the_replica_uncovered() {
    let machine = |changes: &[(&str, Value)]| computers(with(json!({"label": "m3"}), changes));
    let aliases: Vec<String> = (0..9).map(|n| format!("name {n}")).collect();
    for (column, dump) in [
        ("chip", machine(&[("chip", json!("x".repeat(36)))])),
        ("memory_gb", machine(&[("memory_gb", json!("64 GB"))])),
        ("disk_gb", machine(&[("disk_gb", json!(16.5))])),
        ("aliases", machine(&[("aliases", json!(["a@b.example"]))])),
        (
            "nickname_aliases",
            machine(&[("nickname_aliases", json!(["red@balloon"]))]),
        ),
        (
            "nickname_aliases",
            machine(&[("nickname_aliases", json!(aliases))]),
        ),
        ("node_id", machine(&[("node_id", json!("not-a-node"))])),
        ("hostname", machine(&[("hostname", json!("a@b.example"))])),
        ("status", machine(&[("status", json!("retired"))])),
        ("label", phones(json!({"label": "desk@home"}))),
        ("line_type", phones(json!({"line_type": "x".repeat(33)}))),
        ("owner_carrier", phones(json!({"owner_carrier": "a@b"}))),
        ("device", phones(json!({"device": "a@b"}))),
        (
            "phone",
            emails(vec![with(
                email_row(1),
                &[("phone", json!("no phone yet"))],
            )]),
        ),
    ] {
        assert_eq!(
            left_uncovered(&dump, Mode::Authoritative),
            vec![format!("unmapped:{:?}", Some(column))],
            "{column}: {dump:?}"
        );
    }
    let clean = machine(&[
        ("chip", json!("M3 Max")),
        ("memory_gb", json!(64)),
        ("nickname_aliases", json!(["redballoon"])),
        ("node_id", json!("00112233445566778899aabbccddeeff")),
        ("status", json!("parked")),
    ]);
    assert_eq!(
        left_uncovered(&clean, Mode::Authoritative),
        Vec::<String>::new()
    );
}

/// A mesh id is the Mac's own to publish, so an import never writes it and a row that says it covers
/// it: until then the replica says what no cell does.
/// NEGATIVE CONTROL: with `dump_computer` not comparing it, a row that does not say it covers it:
/// `` left: [] right: [("machine", "node_id")] ``.
#[test]
fn a_mesh_id_is_covered_by_the_row_that_says_it() {
    let node = "0123456789abcdef";
    let row = keys::planned_machine_pk("m3", |_| false).unwrap();
    let dump = |id: String| computers(json!({"label": "m3", "node_id": id}));
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE_TABLE, &row, "label", json!("m3")),
            put(&MACHINE_TABLE, &row, "status", json!("active")),
            put(&MACHINE_TABLE, &row, "row", json!("live")),
        ],
    );
    let uncovered = |cells: &Cells, id: String| {
        let plan = planned(&sheets(&dump(id), Mode::FillOnly), cells);
        unmapped(&plan)
    };
    assert_eq!(
        uncovered(&book.cells(), format!("n-{node}")),
        vec![("machine", "node_id".to_owned())]
    );
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&MACHINE_TABLE, &row, "node", json!(node))],
    );
    assert_eq!(uncovered(&book.cells(), format!("n-{node}")), vec![]);
    assert_eq!(uncovered(&book.cells(), node.to_owned()), vec![]);
}

/// A row the registry holds by a replica row's mesh id is that Mac's, whatever label it says: no
/// second row for it, from its own row or an account naming it. A row holding another Mac's id is
/// no row of this one, its label or its host notwithstanding, so nothing of it lands there.
/// NEGATIVE CONTROL: with `found` not asking `stated_principal`, a row is minted beside it: `` left:
/// 4 right: 0 ``. With `stated_row` not asking it, the host's row takes the make: `` left: None
/// right: Some(String("Apple")) ``.
#[test]
fn a_replica_row_finds_its_mac_by_its_mesh_id() {
    let node = "0123456789abcdef";
    let mac = |label: &str| {
        let row = keys::planned_machine_pk(label, |_| false).unwrap();
        let mut book = Book::new();
        book.write(
            2,
            &[],
            vec![
                put(&MACHINE_TABLE, &row, "label", json!(label)),
                put(&MACHINE_TABLE, &row, "status", json!("active")),
                put(&MACHINE_TABLE, &row, "node", json!(node)),
                put(&MACHINE_TABLE, &row, "row", json!("live")),
            ],
        );
        (row, book.cells())
    };
    let (row, cells) = mac("m99");
    for id in [format!("n-{node}"), node.to_owned()] {
        let dump = json!({
            "emails": [], "phones": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "assigned_computers": ["m100"], "tier": "1",
                "status": "active",
            }],
            "computers": [{
                "label": "m100", "node_id": id, "make": "Apple", "status": "active",
                "assigned_accounts": ["claude:box01@proton.example"],
            }],
        });
        let plan = planned(&sheets(&dump, Mode::Authoritative), &cells);
        assert_eq!(
            plan.planned
                .iter()
                .filter(|p| p.collection == "machine" && p.pk != row)
                .count(),
            0,
            "{:?}",
            plan.planned
        );
        assert_eq!(value(&plan, "machine", &row, "make"), Some(&json!("Apple")));
    }

    // A host another Mac's row holds says less than the id: the row goes to its own Mac.
    let row = keys::planned_machine_pk("m99", |_| false).unwrap();
    let elsewhere = keys::planned_machine_pk("m7", |_| false).unwrap();
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&MACHINE_TABLE, &row, "label", json!("m99")),
            put(&MACHINE_TABLE, &row, "status", json!("active")),
            put(&MACHINE_TABLE, &row, "node", json!(node)),
            put(&MACHINE_TABLE, &row, "row", json!("live")),
            put(&MACHINE_TABLE, &elsewhere, "label", json!("m7")),
            put(&MACHINE_TABLE, &elsewhere, "status", json!("active")),
            put(
                &MACHINE_TABLE,
                &elsewhere,
                "node",
                json!("fedcba9876543210"),
            ),
            put(&MACHINE_TABLE, &elsewhere, "hosts", json!(["studio.local"])),
            put(&MACHINE_TABLE, &elsewhere, "row", json!("live")),
        ],
    );
    let hosted = computers(json!({
        "label": "m100", "node_id": format!("n-{node}"), "hostname": "studio.local",
        "make": "Apple", "status": "active",
    }));
    let plan = planned(&sheets(&hosted, Mode::Authoritative), &book.cells());
    assert_eq!(value(&plan, "machine", &row, "make"), Some(&json!("Apple")));
    assert_eq!(value(&plan, "machine", &elsewhere, "make"), None);

    let (row, cells) = mac("m3");
    let other = computers(json!({
        "label": "m3", "node_id": "n-fedcba9876543210", "make": "Dell", "status": "active",
    }));
    for mode in [Mode::Authoritative, Mode::FillOnly] {
        let plan = planned(&sheets(&other, mode), &cells);
        assert_eq!(value(&plan, "machine", &row, "make"), None);
    }
}

/// A relation naming a machine by its alias is the relation the machine's own row states: one Mac,
/// carried, from an address's row and from a phone's.
/// NEGATIVE CONTROL: with `names_one_of` comparing the word alone: `` left: [("address",
/// "assigned_computers")] right: [] `` and `` left: [("phone", "linked_computers")] right: [] ``.
#[test]
fn a_relation_naming_a_machines_alias_is_carried() {
    let id = "claude:box01@proton.example";
    let dump = |address: &str, phone: &str| {
        json!({
            "emails": [with(email_row(1), &[
                ("assigned_computers", json!([address])), ("phone", json!("415-555-0101")),
            ])],
            "phones": [{"phone": "415-555-0101", "linked_computers": [phone]}],
            "ai_accounts": [{
                "id": id, "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "assigned_computers": ["m36"], "tier": "1",
                "status": "active",
            }],
            "computers": [{"label": "m36", "aliases": ["m104"], "assigned_accounts": [id]}],
        })
    };
    for (address, phone) in [("m104", "m36"), ("m36", "m104")] {
        let plan = planned(
            &sheets(&dump(address, phone), Mode::Authoritative),
            &Cells::new(),
        );
        assert_eq!(unmapped(&plan), vec![], "{address} {phone}");
    }
}

/// A machine row an edit of the replica removes whole is reported, as an address or an account is,
/// and left for the owner to retire.
/// NEGATIVE CONTROL: with `finish` listing only accounts and addresses: `` left: [] right:
/// ["machine"] ``.
#[test]
fn a_machine_no_longer_listed_is_reported() {
    let first = sheets(
        &computers(json!({"label": "m3", "make": "Apple", "status": "active"})),
        Mode::Authoritative,
    );
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&first, &Cells::new()));
    let mut read = sheets(&emails(vec![]), Mode::Authoritative);
    for sheet in &mut read {
        sheet.earlier = first.iter().map(|sheet| sheet.source.clone()).collect();
    }
    let plan = planned(&read, &cells);
    let listed: Vec<&str> = plan
        .findings
        .iter()
        .filter(|f| f.kind == Kind::NoLongerListed)
        .filter_map(|f| f.collection)
        .collect();
    assert_eq!(listed, vec!["machine"]);
}

/// Only an alias names another address, and what a row's words cannot say is reported rather than
/// read as a standalone address with the text in no cell.
/// NEGATIVE CONTROL: with `(false, root)` dropping `root`: `` root/other: left: [] ``.
#[test]
fn a_root_or_a_kind_no_row_can_say_is_reported() {
    let other = json!("other@proton.example");
    let own = json!("box01@proton.example");
    for (case, kind, root, reported) in [
        (
            "alias/text",
            json!("alias"),
            json!("see the notes"),
            Some("root_account"),
        ),
        (
            "root/other",
            json!("root"),
            other.clone(),
            Some("root_account"),
        ),
        (
            "blank/other",
            json!(""),
            other.clone(),
            Some("root_account"),
        ),
        (
            "alias/none",
            json!("alias"),
            json!(null),
            Some("account_type"),
        ),
        (
            "alias/own",
            json!("alias"),
            own.clone(),
            Some("account_type"),
        ),
        (
            "forward/none",
            json!("forward"),
            json!(null),
            Some("account_type"),
        ),
        ("root/own", json!("root"), own.clone(), None),
        ("root/none", json!("root"), json!(null), None),
        ("standalone/none", json!("standalone"), json!(null), None),
        ("alias/other", json!("alias"), other.clone(), None),
    ] {
        let row = with(
            email_row(1),
            &[
                ("password", json!(null)),
                ("account_type", kind),
                ("root_account", root),
            ],
        );
        let plan = planned(
            &sheets(&emails(vec![row]), Mode::Authoritative),
            &Cells::new(),
        );
        assert_eq!(
            unmapped(&plan),
            reported
                .map(|column| ("address", column.to_owned()))
                .into_iter()
                .collect::<Vec<_>>(),
            "{case}"
        );
    }
}

/// A word the registry holds no cell for is reported, and the registry's own words are carried: a
/// banned account does not read as pending.
/// NEGATIVE CONTROL: with `dump_account` reading only `suspended`, `banned` plans `pending`:
/// `` left: Some(String("pending")) right: Some(String("banned")) ``.
#[test]
fn a_status_or_a_provider_is_carried_or_reported() {
    let email = |column: &str, word: &str| {
        planned(
            &sheets(
                &emails(vec![with(
                    email_row(1),
                    &[
                        ("password", json!(null)),
                        ("webmail_url", json!(null)),
                        (column, json!(word)),
                    ],
                )]),
                Mode::Authoritative,
            ),
            &Cells::new(),
        )
    };
    let reported = vec![("address", "status".to_owned())];
    assert_eq!(unmapped(&email("status", "locked")), reported);
    assert_eq!(unmapped(&email("status", "unknown")), vec![]);
    assert_eq!(unmapped(&email("status", "Suspended")), vec![]);
    let reported = vec![("address", "provider".to_owned())];
    assert_eq!(unmapped(&email("provider", "fastmail")), reported);
    assert_eq!(unmapped(&email("provider", "other")), vec![]);
    assert_eq!(unmapped(&email("provider", "Hotmail")), vec![]);
    // A flag that says neither guards the row until it is settled.
    let unsure = email("protected", "maybe");
    let beside_a_password = planned(
        &sheets(
            &emails(vec![with(email_row(1), &[("protected", json!("maybe"))])]),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    let login = keys::address_pk("box01@proton.example").unwrap();
    assert_eq!(
        value(&beside_a_password, "address", &login, "password"),
        None
    );
    assert_eq!(unmapped(&unsure), vec![("address", "protected".to_owned())]);
    assert_eq!(unmapped(&email("protected", "no")), vec![]);

    let account = |status: &str, notes: &str| {
        planned(
            &sheets(
                &json!({
                    "emails": [], "phones": [], "computers": [],
                    "ai_accounts": [{
                        "id": "claude:box01@proton.example", "provider": "anthropic",
                        "service": "claude", "email": "box01@proton.example", "tier": "1",
                        "status": status, "notes": notes,
                    }],
                }),
                Mode::Authoritative,
            ),
            &Cells::new(),
        )
    };
    let status = |plan: &Plan| {
        let pk = keys::account_pk("claude", "box01@proton.example").unwrap();
        value(plan, "account", &pk, "status").cloned()
    };
    assert_eq!(status(&account("banned", "")), Some(json!("banned")));
    assert_eq!(status(&account("Broken", "")), Some(json!("broken")));
    assert_eq!(status(&account("active", "")), Some(json!("pending")));
    assert_eq!(status(&account("suspended", "")), Some(json!("paused")));
    let gone = account("gone", "");
    assert_eq!(status(&gone), Some(json!("pending")));
    assert_eq!(unmapped(&gone), vec![("account", "status".to_owned())]);
}

/// A row the registry holds protected stays without a mailbox, whatever a replica not yet told says
/// of it: only its link to its root can say where its codes arrive, by the row that address names.
/// NEGATIVE CONTROL: with `guarded` reading only this run's marks, the link goes unreported: `` left:
/// [] right: [("address", "receiving_mailbox")] ``. With `address_key` the key the address derives,
/// the renamed root is reported: `` renamed: left: [("address", "receiving_mailbox")] right: [] ``.
/// With `dump_email` not asking `guarded`, a replica not yet told mints a mailbox row.
#[test]
fn a_protected_row_holds_no_mailbox_and_its_link_is_read_by_the_row_it_names() {
    let login = keys::address_pk("box01@proton.example").unwrap();
    let dump = |row: Value, receiving: &str| {
        json!({
            "emails": [row], "phones": [], "computers": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "receiving_mailbox": receiving,
                "tier": "1", "status": "active",
            }],
        })
    };
    let plain = with(email_row(1), &[("password", json!(null))]);
    let row = |address: &str, changes: Vec<(&'static str, Value)>| {
        let pk = keys::address_pk(address).unwrap();
        let mut cells = vec![
            put(&ADDRESS, &pk, "addr", json!(address)),
            put(&ADDRESS, &pk, "row", json!("live")),
        ];
        cells.extend(
            changes
                .into_iter()
                .map(|(column, x)| put(&ADDRESS, &pk, column, x)),
        );
        cells
    };

    // Protected by the registry alone: nothing of the replica makes it a mailbox.
    let mut book = Book::new();
    book.write(
        2,
        &[],
        row("box01@proton.example", vec![("protected", json!(true))]),
    );
    let plan = planned(
        &sheets(
            &dump(plain.clone(), "box02@proton.example"),
            Mode::Authoritative,
        ),
        &book.cells(),
    );
    assert_eq!(value(&plan, "address", &login, "mailbox"), None);
    assert_eq!(
        unmapped(&plan),
        vec![("address", "receiving_mailbox".to_owned())]
    );
    // A replica not yet told, with a password to show, makes no mailbox of it either.
    let plan = planned(
        &sheets(
            &dump(email_row(1), "box01@proton.example"),
            Mode::Authoritative,
        ),
        &book.cells(),
    );
    assert!(
        !plan.planned.iter().any(|p| p.collection == MAILBOX.name),
        "{:?}",
        plan.planned
    );

    // The link names the row that says the root now, under a key its address no longer derives.
    let renamed = with(
        plain.clone(),
        &[
            ("protected", json!(true)),
            ("account_type", json!("alias")),
            ("root_account", json!("box02@proton.example")),
        ],
    );
    let root = keys::address_pk("cy@proton.example").unwrap();
    let mut book = Book::new();
    book.write(2, &[], row("cy@proton.example", vec![]));
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&ADDRESS, &root, "addr", json!("box02@proton.example"))],
    );
    let plan = planned(
        &sheets(
            &dump(renamed.clone(), "box02@proton.example"),
            Mode::Authoritative,
        ),
        &book.cells(),
    );
    assert_eq!(
        value(&plan, "address", &login, "alias_of"),
        Some(&json!(root))
    );
    assert_eq!(unmapped(&plan), vec![], "renamed");
    let plan = planned(
        &sheets(&dump(renamed, "box03@proton.example"), Mode::Authoritative),
        &book.cells(),
    );
    assert_eq!(
        unmapped(&plan),
        vec![("address", "receiving_mailbox".to_owned())]
    );

    // A link the registry already holds says it, on a row of the replica that does not.
    let target = keys::address_pk("box02@proton.example").unwrap();
    let mut book = Book::new();
    book.write(2, &[], row("box02@proton.example", vec![]));
    let seen = book.all();
    book.write(
        2,
        &seen,
        row(
            "box01@proton.example",
            vec![("protected", json!(true)), ("alias_of", json!(target))],
        ),
    );
    let plan = planned(
        &sheets(&dump(plain, "box02@proton.example"), Mode::Authoritative),
        &book.cells(),
    );
    assert_eq!(unmapped(&plan), vec![], "held link");
}

/// A protected row holds no mailbox whichever source names one: a logins sheet's correction is kept
/// out and reported as a dump's is.
/// NEGATIVE CONTROL: with `finish` writing a mailbox on a protected row: `` left: Some(String("…"))
/// right: None ``.
#[test]
fn a_correction_gives_a_protected_row_no_mailbox() {
    let records = vec![
        vec!["email".to_owned(), "your correction".to_owned()],
        vec![
            "box01@proton.example".to_owned(),
            "box02@proton.example".to_owned(),
        ],
    ];
    let read =
        [
            Sheet::from_records_with_bytes(SheetKind::Logins, "logins.csv", records, b"logins")
                .unwrap(),
        ];
    let login = keys::address_pk("box01@proton.example").unwrap();
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            put(&ADDRESS, &login, "addr", json!("box01@proton.example")),
            put(&ADDRESS, &login, "protected", json!(true)),
            put(&ADDRESS, &login, "row", json!("live")),
        ],
    );
    let plan = planned(&read, &book.cells());
    assert_eq!(value(&plan, "address", &login, "mailbox"), None);
    assert!(
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::ProtectedSecret && f.column.as_deref() == Some("mailbox")),
        "{:?}",
        plan.findings
    );
    let free = planned(&read, &Cells::new());
    assert!(value(&free, "address", &login, "mailbox").is_some());
}

/// A label the replica states replaces the one this run made up for lack of it: a phone a sheet
/// numbered first keeps its own.
/// NEGATIVE CONTROL: with `dump_phone_label` reporting a label it could replace, `desk-line` is not
/// planned and the assertion over `plan.planned` fails.
#[test]
fn a_stated_label_replaces_a_made_up_one() {
    let sheet = phones_sheet(vec![
        vec!["phone".to_owned()],
        vec!["415-555-0101".to_owned()],
    ]);
    let mut read = vec![sheet];
    read.extend(sheets(
        &phones(json!({"label": "desk-line"})),
        Mode::FillOnly,
    ));
    let plan = planned(&read, &Cells::new());
    assert!(
        plan.planned
            .iter()
            .any(|p| p.column == "label" && p.x == json!("desk-line")),
        "{:?}",
        plan.planned
    );
    assert_eq!(unmapped(&plan), vec![]);
}

/// A phone's label moves when the replica's does, and one another row holds is reported rather than
/// held by two.
/// NEGATIVE CONTROL: with `dump_phone_label` not called: `` left: None right:
/// Some(String("desk-line")) ``.
#[test]
fn a_phone_follows_its_replica_label() {
    let read = |label: &str| sheets(&phones(json!({"label": label})), Mode::Authoritative);
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&read("phone-1"), &Cells::new()));
    let pk = cell::project(&PHONE, &cells)
        .rows
        .keys()
        .next()
        .unwrap()
        .clone();
    let plan = planned(&read("desk-line"), &cells);
    assert_eq!(
        value(&plan, "phone", &pk, "label"),
        Some(&json!("desk-line"))
    );
    assert_eq!(unmapped(&plan), vec![]);

    let two = json!({
        "emails": [], "computers": [], "ai_accounts": [],
        "phones": [{"phone": "415-555-0102", "label": "phone-1"}],
    });
    let plan = planned(&sheets(&two, Mode::Authoritative), &cells);
    assert_eq!(unmapped(&plan), vec![("phone", "label".to_owned())]);

    // Nor do two rows of one run reach one label.
    let both = json!({
        "emails": [], "computers": [], "ai_accounts": [],
        "phones": [
            {"phone": "415-555-0101", "label": "desk-line"},
            {"phone": "415-555-0102", "label": "desk-line"},
        ],
    });
    let plan = planned(&sheets(&both, Mode::Authoritative), &cells);
    assert_eq!(unmapped(&plan), vec![("phone", "label".to_owned())]);
}

/// A relation an `emails` or `phones` row states is carried by the row that holds it, element by
/// element: one account row does not vouch for a machine or a service it never names.
/// NEGATIVE CONTROL: with the relations read only for an address with no account row: `` extra
/// machine: left: [] ``.
#[test]
fn a_relation_no_row_states_is_reported() {
    let dump = |computers: Value, services: Value| {
        json!({
            "emails": [with(
                email_row(1),
                &[
                    ("password", json!(null)),
                    ("assigned_computers", computers),
                    ("services", services),
                ],
            )],
            "phones": [], "computers": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "assigned_computers": ["m3"], "tier": "1",
                "status": "active",
            }],
        })
    };
    let reported =
        |dump: Value| unmapped(&planned(&sheets(&dump, Mode::Authoritative), &Cells::new()));
    assert_eq!(reported(dump(json!(["M3"]), json!(["claude"]))), vec![]);
    assert_eq!(
        reported(dump(json!(["m3", "m5"]), json!(["claude"]))),
        vec![("address", "assigned_computers".to_owned())],
        "extra machine"
    );
    assert_eq!(
        reported(dump(json!(["m3"]), json!(["claude", "codex"]))),
        vec![("address", "services".to_owned())],
        "extra service"
    );

    let linked = |emails: Value, machines: Value| {
        json!({
            "emails": [with(
                email_row(1),
                &[
                    ("password", json!(null)),
                    ("phone", json!("(415) 555-0101")),
                    ("assigned_computers", json!(["m3"])),
                ],
            )],
            "computers": [], "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "assigned_computers": ["m3"], "tier": "1",
                "status": "active",
            }],
            "phones": [{
                "phone": "415-555-0101", "linked_emails": emails, "linked_computers": machines,
            }],
        })
    };
    assert_eq!(
        reported(linked(json!(["box01@proton.example"]), json!(["m3"]))),
        vec![]
    );
    assert_eq!(
        reported(linked(json!(["cy@proton.example"]), json!(["m3"]))),
        vec![("phone", "linked_emails".to_owned())]
    );
    assert_eq!(
        reported(linked(json!(["box01@proton.example"]), json!(["m5"]))),
        vec![("phone", "linked_computers".to_owned())]
    );
}

/// A computer naming a row the vendor's mark reads as holding no account owes no preference: the row
/// is decided silent, and its other half is not what keeps the replica uncovered.
/// NEGATIVE CONTROL: with `no_account` never filled: `` left: ["no-account"] right: [] ``.
#[test]
fn a_computer_naming_a_row_with_no_account_owes_nothing() {
    let dump = |id: &str| {
        json!({
            "emails": [], "phones": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "assigned_computers": ["m3"], "tier": "0",
                "status": "active",
            }],
            "computers": [{"label": "m3", "assigned_accounts": [id]}],
        })
    };
    let read = |id: &str| {
        kinds(&planned(
            &sheets(&dump(id), Mode::Authoritative),
            &Cells::new(),
        ))
    };
    assert_eq!(read("claude:box01@proton.example"), Vec::<&str>::new());
    assert_eq!(read("codex:box01@proton.example"), vec!["no-account"]);
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
    let canonical = [
        b"fleet-db-m104.json#emails\0".as_slice(),
        &aterm_json::to_vec(&one).unwrap(),
    ]
    .concat();
    assert_ne!(a.sha256, hex::encode_lower(&Sha256::digest(&canonical)));
    assert_ne!(a.source, keys::source_pk(&canonical));
    // Keyed: the same dump under another epoch's key is another digest, and the same key and
    // bytes name one source on every Mac.
    let other = read(&one, &[0xa5; 32]);
    assert_ne!(a.sha256, other.sha256);
    assert_eq!(read(&one, &KEY).sha256, a.sha256);
}

/// A login on two rows of the sheet keeps only its last machines on its `ai_accounts` row. The
/// machines' own rows state the rest, and a preference is read from them, so nothing is lost.
/// NEGATIVE CONTROL: with `dump_account_relations` reading no `computers` row: `` left:
/// [("address", "assigned_computers")] right: [] ``.
#[test]
fn a_login_on_two_rows_is_carried_by_its_machines() {
    let id = "claude:box01@proton.example";
    let dump = |first_machine_names: Value| {
        json!({
            "emails": [with(
                email_row(1),
                &[("password", json!(null)), ("assigned_computers", json!(["m1", "m2"]))],
            )],
            "phones": [],
            "ai_accounts": [{
                "id": id, "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "assigned_computers": ["m2"], "tier": "1",
                "status": "active",
            }],
            "computers": [
                {"label": "m1", "assigned_accounts": first_machine_names},
                {"label": "m2", "assigned_accounts": [id]},
            ],
        })
    };
    let reported =
        |dump: Value| unmapped(&planned(&sheets(&dump, Mode::Authoritative), &Cells::new()));
    assert_eq!(reported(dump(json!([id]))), vec![]);
    assert_eq!(
        reported(dump(json!([]))),
        vec![("address", "assigned_computers".to_owned())]
    );
}

/// A label this run makes up leaves free every label a replica's row states, so that row gets its
/// word: a sheet's blank line numbers the replica's labels past the row count, and a row with no
/// label may be read first.
/// NEGATIVE CONTROL: with `phone` minting any free label: `` left: [("phone", "label")] right: [] ``.
#[test]
fn a_made_up_label_takes_none_a_replica_row_states() {
    let labels = |plan: &Plan| {
        let mut labels: Vec<String> = plan
            .planned
            .iter()
            .filter(|p| p.collection == "phone" && p.column == "label")
            .map(|p| p.x.as_str().unwrap().to_owned())
            .collect();
        labels.sort();
        labels
    };
    let numbered =
        |rows: Value| json!({"phones": rows, "emails": [], "computers": [], "ai_accounts": []});
    let csv = phones_sheet(vec![
        vec!["phone".to_owned()],
        vec!["415-555-0101".to_owned()],
        vec!["415-555-0102".to_owned()],
        vec!["415-555-0103".to_owned()],
    ]);
    let mut read = vec![csv];
    read.extend(sheets(
        &numbered(json!([
            {"phone": "415-555-0101", "label": "phone-1"},
            {"phone": "415-555-0102", "label": "phone-3"},
            {"phone": "415-555-0103", "label": "phone-4"},
        ])),
        Mode::FillOnly,
    ));
    let plan = planned(&read, &Cells::new());
    assert_eq!(unmapped(&plan), vec![]);
    assert_eq!(labels(&plan), ["phone-1", "phone-3", "phone-4"]);

    let plan = planned(
        &sheets(
            &numbered(json!([
                {"phone": "415-555-0100"},
                {"phone": "415-555-0101", "label": "phone-1"},
            ])),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    assert_eq!(unmapped(&plan), vec![]);
    assert_eq!(labels(&plan), ["phone-1", "phone-2"]);
}

/// `active` says nothing of itself, but beside a stricter word the registry holds it is a word that
/// reaches no cell: the row is not moved, and the replica stays uncovered until someone says which.
/// NEGATIVE CONTROL: with `takes_back` false: `` left: [] right: [("account", "status")] ``.
#[test]
fn active_beside_a_word_it_would_take_back_is_reported() {
    let dump = |status: &str| {
        json!({
            "emails": [], "phones": [], "computers": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "tier": "1", "status": status,
            }],
        })
    };
    let pk = keys::account_pk("claude", "box01@proton.example").unwrap();
    for (word, held) in [
        ("banned", "banned"),
        ("broken", "broken"),
        ("paused", "paused"),
        ("suspended", "paused"),
    ] {
        let mut book = Book::new();
        let cells = apply(
            &mut book,
            &planned(&sheets(&dump(word), Mode::Authoritative), &Cells::new()),
        );
        for mode in [Mode::Authoritative, Mode::FillOnly] {
            let plan = planned(&sheets(&dump("active"), mode), &cells);
            assert_eq!(
                unmapped(&plan),
                vec![("account", "status".to_owned())],
                "{word}"
            );
            assert!(
                value(&plan, "account", &pk, "status").is_none_or(|x| *x == json!(held)),
                "{word}"
            );
        }
    }
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &planned(&sheets(&dump("active"), Mode::Authoritative), &Cells::new()),
    );
    assert_eq!(
        unmapped(&planned(&sheets(&dump("active "), Mode::FillOnly), &cells)),
        vec![]
    );
}

/// What an earlier import of this same replica wrote and its edit no longer says is reported in either
/// mode, by the record beside it, and stays: the replica reads uncovered. Another replica's, named
/// alike, is no withdrawal at all.
/// NEGATIVE CONTROL: with `Sheet::said` not reading `earlier`: `` left: [] right: [("differs",
/// Some("note"))] ``. With it matching any dump named alike, the renamed file reports it: `` left:
/// [("differs", Some("note"))] right: [] ``. With `own` reading `said`, the edit empties the note:
/// `` left: Some(Write) right: Some(Kept) ``.
#[test]
fn an_edited_replica_reports_what_it_no_longer_says() {
    let login = keys::address_pk("box01@proton.example").unwrap();
    let first = sheets(
        &emails(vec![with(email_row(1), &[("notes", json!("old words"))])]),
        Mode::Authoritative,
    );
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&first, &Cells::new()));
    let edited = emails(vec![email_row(1)]);
    let judged = |plan: &Plan| {
        super::super::covered::judge(plan, &|_| false)
            .into_iter()
            .map(|cell| (cell.kind, cell.column))
            .collect::<Vec<_>>()
    };
    let reported = vec![("differs", Some("note".to_owned()))];

    let other = planned(&sheets(&edited, Mode::Authoritative), &cells);
    assert_eq!(judged(&other), vec![]);

    let earlier: BTreeSet<String> = first.iter().map(|sheet| sheet.source.clone()).collect();
    for mode in [Mode::Authoritative, Mode::FillOnly] {
        let mut read = sheets(&edited, mode);
        for sheet in &mut read {
            sheet.earlier.clone_from(&earlier);
        }
        let plan = planned(&read, &cells);
        assert_eq!(step(&plan, "address", &login, "note"), Some(&Step::Kept));
        assert_eq!(judged(&plan), reported);
    }

    // A key the record names under another file's name is not this table's.
    let renamed = dump_sheets(
        "another-file.json",
        &aterm_json::to_vec(&edited).unwrap(),
        Mode::Authoritative,
        &KEY,
    );
    let mut renamed = renamed.unwrap();
    for sheet in &mut renamed {
        sheet.earlier.clone_from(&earlier);
    }
    assert_eq!(judged(&planned(&renamed, &cells)), vec![]);
}

/// A preference an edit of the replica withdraws stays live and is reported, in either mode.
/// NEGATIVE CONTROL: with `finish` ending it as its own source's: `` left: Some(String("ended"))
/// right: None ``.
#[test]
fn an_edit_withdrawing_a_preference_is_reported() {
    let id = "claude:box01@proton.example";
    let dump = |machines: Value, assigned: Value| {
        json!({
            "emails": [], "phones": [],
            "ai_accounts": [{
                "id": id, "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "assigned_computers": machines, "tier": "1",
                "status": "active",
            }],
            "computers": [{"label": "m3", "assigned_accounts": assigned}],
        })
    };
    let first = sheets(&dump(json!(["m3"]), json!([id])), Mode::Authoritative);
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&first, &Cells::new()));
    let earlier: BTreeSet<String> = first.iter().map(|sheet| sheet.source.clone()).collect();
    let edited = |mode| {
        let mut read = sheets(&dump(json!([]), json!([])), mode);
        for sheet in &mut read {
            sheet.earlier.clone_from(&earlier);
        }
        planned(&read, &cells)
    };
    let machine = keys::planned_machine_pk("m3", |_| false).unwrap();
    let account = keys::account_pk("claude", "box01@proton.example").unwrap();
    let preference = keys::preference_pk(&machine, &account).unwrap();
    for mode in [Mode::Authoritative, Mode::FillOnly] {
        let plan = edited(mode);
        assert_eq!(value(&plan, "preference", &preference, "row"), None);
        assert!(kinds(&plan).contains(&"differs"), "{:?}", kinds(&plan));
    }
}

/// A cell an edit of the replica clears is reported and kept: a fact of a machine or a phone, a
/// mailbox's address, an address's phone. Each is the replica's own earlier word, under a key its
/// edit changed, and a blank there may be a value this import could not carry as well as none.
/// NEGATIVE CONTROL: with `REPORT_VALUES` missing a column, that column's assertion fails with
/// `` left: None right: Some(Kept) ``. With `dump_computer` not visiting its row, every machine
/// column's does. With `from_dump` not asked, the sheet's blank is reported: `` left: Some(Kept)
/// right: None ``.
#[test]
fn a_cell_an_edit_clears_is_reported() {
    let edited = |full: &Value, bare: &Value, phone: Option<&str>| {
        let first = sheets(full, Mode::Authoritative);
        let mut book = Book::new();
        if let Some(pk) = phone {
            let number = SEALER
                .seal(
                    PHONE.name,
                    pk,
                    "number",
                    &secret::Secret::new("4155550101".into()),
                )
                .unwrap();
            let changes = [
                ("number", json!(number)),
                ("last4", json!("0101")),
                ("label", json!("phone-1")),
                ("row", json!("live")),
            ]
            .into_iter()
            .map(|(col, x)| {
                cell::change(
                    &PHONE,
                    cell::CellKey {
                        pk,
                        col,
                        elem: None,
                    },
                    &cell::CellValue::owner(PHONE.version, x),
                )
                .unwrap()
            })
            .collect();
            book.write(1, &[], changes);
        }
        let before = book.cells();
        let cells = apply(&mut book, &planned(&first, &before));
        if let Some(pk) = phone {
            let phones = cell::project(&PHONE, &cells);
            assert_eq!(phones.rows.len(), 1);
            let number = phones.rows[pk]
                .get("number")
                .and_then(Value::as_str)
                .unwrap();
            assert!(SEALER.holds(
                PHONE.name,
                pk,
                "number",
                number,
                &secret::Secret::new("4155550101".into())
            ));
        }
        let mut read = sheets(bare, Mode::Authoritative);
        let earlier: BTreeSet<String> = first.iter().map(|sheet| sheet.source.clone()).collect();
        for sheet in &mut read {
            sheet.earlier.clone_from(&earlier);
        }
        planned(&read, &cells)
    };
    let machine = keys::planned_machine_pk("m3", |_| false).unwrap();
    let plan = edited(
        &computers(json!({
            "label": "m3", "make": "Apple", "arch": "arm64", "os": "macOS", "chip": "M4",
            "memory_gb": 64, "disk_gb": 2000, "nickname": "studio", "nickname_aliases": ["fb"],
            "location": "SF", "notes": "desk",
        })),
        &computers(json!({"label": "m3"})),
        None,
    );
    for column in [
        "make",
        "arch",
        "os",
        "chip",
        "ram_gb",
        "disk_gb",
        "nickname",
        "nickname_aliases",
        "location",
        "note",
    ] {
        assert_eq!(
            step(&plan, "machine", &machine, column),
            Some(&Step::Kept),
            "{column}"
        );
    }

    let plan = edited(
        &phones(json!({
            "owner_carrier": "Acme", "line_type": "eSIM", "device": "Desk", "notes": "line",
            "label": "phone-1",
        })),
        &phones(json!({})),
        // A valid random-key shape whose hex happens to be decimal is still an identifier.
        Some("ph-1234567890123456"),
    );
    let phone = plan
        .planned
        .iter()
        .find(|p| p.collection == "phone")
        .map(|p| p.pk.clone())
        .unwrap();
    for column in ["carrier", "line_type", "device", "note"] {
        assert_eq!(
            step(&plan, "phone", &phone, column),
            Some(&Step::Kept),
            "{column}"
        );
    }

    let login = keys::address_pk("box01@proton.example").unwrap();
    let plan = edited(
        &emails(vec![with(
            email_row(1),
            &[
                ("phone", json!("415-555-0101")),
                ("webmail_url", json!("https://mail.example.invalid/")),
            ],
        )]),
        &emails(vec![email_row(1)]),
        Some("ph-1234567890123456"),
    );
    assert_eq!(step(&plan, "address", &login, "phone"), Some(&Step::Kept));
    let mailbox = plan
        .planned
        .iter()
        .find(|p| p.collection == MAILBOX.name)
        .map(|p| p.pk.clone())
        .unwrap();
    assert_eq!(
        step(&plan, MAILBOX.name, &mailbox, "webmail"),
        Some(&Step::Kept)
    );

    // A sheet's blank fact is neither: a sheet leaves a cell blank for more reasons than "none".
    let csv = |rows: &str| {
        Sheet::parse(
            SheetKind::Computers,
            "alab-computers.csv",
            format!("id,make,arch\n{rows}").as_bytes(),
        )
        .unwrap()
    };
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &planned(&[csv("m3,Apple,arm64\n")], &Cells::new()),
    );
    let plan = planned(&[csv("m3,,arm64\n")], &cells);
    assert_eq!(step(&plan, "machine", &machine, "make"), None);
    assert!(!kinds(&plan).contains(&"differs"), "{:?}", kinds(&plan));
}

/// The sheet infers a mailbox from a password flag or a root: where a row is protected that is no
/// source's word, so it is no finding and makes no mailbox row, and the same whichever source marks
/// the row, the registry or a replica read after the sheet.
/// NEGATIVE CONTROL: with `emails` not asking `guarded`: `` protected-secret `` is among the kinds.
/// With `dump_marks` not called, the replica's mark comes too late: the same.
#[test]
fn a_mailbox_only_inferred_is_no_finding_on_a_protected_row() {
    let csv = || {
        Sheet::parse(
            SheetKind::Emails,
            "alab-emails.csv",
            b"Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n\
              box01@proton.example,,1,Active,proton,,,\n\
              box02@proton.example,box03@proton.example,,Active,proton,,,\n",
        )
        .unwrap()
    };
    let clean = |plan: &Plan| {
        assert!(
            !kinds(plan).contains(&"protected-secret"),
            "{:?}",
            kinds(plan)
        );
        assert!(
            !plan.planned.iter().any(|p| p.collection == MAILBOX.name),
            "{:?}",
            plan.planned
        );
    };
    let mut book = Book::new();
    for address in ["box01@proton.example", "box02@proton.example"] {
        let pk = keys::address_pk(address).unwrap();
        book.write(
            2,
            &[],
            vec![
                put(&ADDRESS, &pk, "addr", json!(address)),
                put(&ADDRESS, &pk, "protected", json!(true)),
                put(&ADDRESS, &pk, "row", json!("live")),
            ],
        );
    }
    clean(&planned(&[csv()], &book.cells()));

    let marked = |n: usize, root: Value| {
        with(
            email_row(n),
            &[
                ("password", json!(null)),
                ("protected", json!(true)),
                ("root_account", root),
            ],
        )
    };
    let mut read = vec![csv()];
    read.extend(sheets(
        &emails(vec![
            marked(1, json!(null)),
            marked(2, json!("box03@proton.example")),
        ]),
        Mode::FillOnly,
    ));
    clean(&planned(&read, &Cells::new()));
}

/// A row an import linked and hinted, later protected, gives both back, whichever source still
/// names them: a hint is no source's word, and a correction naming a mailbox stays reported.
/// NEGATIVE CONTROL: with `finish` leaving the wants in place: `` left: None right: Some(Null) ``.
/// With `shielded` not taking back an edited replica's link: `` left: Some(Kept) right:
/// Some(Write) ``.
#[test]
fn a_row_protected_later_gives_back_its_mailbox_and_hint() {
    let read = [Sheet::from_records_with_bytes(
        SheetKind::Logins,
        "logins.csv",
        vec![
            vec![
                "email".to_owned(),
                "codes arrive in (my guess)".to_owned(),
                "your correction".to_owned(),
            ],
            vec![
                "box01@proton.example".to_owned(),
                "box02@proton.example".to_owned(),
                "box02@proton.example".to_owned(),
            ],
        ],
        b"logins",
    )
    .unwrap()];
    let login = keys::address_pk("box01@proton.example").unwrap();
    let mut book = Book::new();
    let first = planned(&read, &Cells::new());
    assert!(value(&first, "address", &login, "mailbox_hint").is_some());
    apply(&mut book, &first);
    book.write(
        2,
        &[],
        vec![put(&ADDRESS, &login, "protected", json!(true))],
    );
    let plan = planned(&read, &book.cells());
    for column in ["mailbox", "mailbox_hint"] {
        assert_eq!(
            value(&plan, "address", &login, column),
            Some(&Value::Null),
            "{column}"
        );
        assert_eq!(
            step(&plan, "address", &login, column),
            Some(&Step::Write),
            "{column}"
        );
    }
    assert!(
        plan.findings
            .iter()
            .any(|f| f.kind == Kind::ProtectedSecret && f.column.as_deref() == Some("mailbox")),
        "{:?}",
        plan.findings
    );
    apply(&mut book, &plan);
    let (lab, generic) = crate::alab::checks::lab(&book.cells());
    assert!(lab.is_empty(), "{lab:?}");
    assert!(generic.is_empty(), "{generic:?}");

    // The protection is the registry's word, not an edit's: a replica edited since still gives
    // the link back, on an authoritative read.
    let first = sheets(&emails(vec![email_row(1)]), Mode::Authoritative);
    let mut book = Book::new();
    apply(&mut book, &planned(&first, &Cells::new()));
    let seen = book.all();
    book.write(
        2,
        &seen,
        vec![put(&ADDRESS, &login, "protected", json!(true))],
    );
    let mut read = sheets(
        &emails(vec![with(email_row(1), &[("notes", json!("edited"))])]),
        Mode::Authoritative,
    );
    for sheet in &mut read {
        sheet.earlier = first.iter().map(|sheet| sheet.source.clone()).collect();
    }
    let plan = planned(&read, &book.cells());
    assert_eq!(
        step(&plan, "address", &login, "mailbox"),
        Some(&Step::Write)
    );
}

/// A protected address is no mailbox's login, whatever names it as one: an alias's root, a
/// receiving mailbox, an owner's correction, and a password a replica gives beside a sheet's mark.
/// What only infers a mailbox is no finding; what states one is reported.
/// NEGATIVE CONTROL: with `mailbox` not asking `guarded`, a mailbox row is planned: `` assertion
/// failed: !plan.planned.iter().any(… ``. With `dump_marks` not reading the emails sheet, the
/// replica's password mints one the same way.
#[test]
fn a_protected_address_is_no_mailboxs_login() {
    let no_mailbox = |plan: &Plan| {
        assert!(
            !plan.planned.iter().any(|p| p.collection == MAILBOX.name),
            "{:?}",
            plan.planned
        );
    };
    let csv = |rows: &str| {
        let text = format!(
            "Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n{rows}"
        );
        Sheet::parse(SheetKind::Emails, "alab-emails.csv", text.as_bytes()).unwrap()
    };
    let plan = planned(
        &[csv("box01@proton.example,,,Active,proton,,Y,\n\
             box02@proton.example,box01@proton.example,1,Active,proton,,,\n")],
        &Cells::new(),
    );
    no_mailbox(&plan);
    assert!(
        !kinds(&plan).contains(&"protected-secret"),
        "{:?}",
        kinds(&plan)
    );

    let mut read = vec![csv("box01@proton.example,,,Active,proton,,Y,\n")];
    read.extend(sheets(&emails(vec![email_row(1)]), Mode::Authoritative));
    no_mailbox(&planned(&read, &Cells::new()));

    let dump = json!({
        "emails": [with(email_row(1), &[("password", json!(null)), ("protected", json!(true))])],
        "phones": [], "computers": [],
        "ai_accounts": [{
            "id": "claude:box03@proton.example", "provider": "anthropic", "service": "claude",
            "email": "box03@proton.example", "receiving_mailbox": "box01@proton.example",
            "tier": "1", "status": "active",
        }],
    });
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    no_mailbox(&plan);
    assert!(unmapped(&plan).contains(&("address", "receiving_mailbox".to_owned())));

    let logins = Sheet::from_records_with_bytes(
        SheetKind::Logins,
        "logins.csv",
        vec![
            vec!["email".to_owned(), "your correction".to_owned()],
            vec![
                "box02@proton.example".to_owned(),
                "box01@proton.example".to_owned(),
            ],
        ],
        b"logins",
    )
    .unwrap();
    let plan = planned(
        &[csv("box01@proton.example,,,Active,proton,,Y,\n"), logins],
        &Cells::new(),
    );
    no_mailbox(&plan);
    let box02 = keys::address_pk("box02@proton.example").unwrap();
    assert!(
        plan.findings.iter().any(|f| f.kind == Kind::ProtectedSecret
            && f.pk.as_deref() == Some(box02.as_str())
            && f.column.as_deref() == Some("mailbox")),
        "{:?}",
        plan.findings
    );
}

/// A link to a mailbox whose login only this run marks protected stays, as that row's own mailbox
/// does, from a receiving mailbox and from a correction and a guess: the owner's word that the
/// login is not protected may win the mark.
/// NEGATIVE CONTROL: with `login_marked_now` always false: `` left: Some(Write) `` at the mailbox.
#[test]
fn a_mark_on_a_mailboxs_login_only_this_run_reads_takes_no_link_back() {
    let login = keys::address_pk("box01@proton.example").unwrap();
    let receiving = keys::address_pk("box02@proton.example").unwrap();
    let marked = || {
        Sheet::parse(
            SheetKind::Emails,
            "alab-emails.csv",
            b"Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n\
              box02@proton.example,,,Active,proton,,yes,\n",
        )
        .unwrap()
    };
    let dump = json!({
        "emails": [with(email_row(2), &[("password", json!(null))])],
        "phones": [], "computers": [],
        "ai_accounts": [{
            "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
            "email": "box01@proton.example", "receiving_mailbox": "box02@proton.example",
            "tier": "1", "status": "active",
        }],
    });
    let logins = Sheet::from_records_with_bytes(
        SheetKind::Logins,
        "logins.csv",
        vec![
            vec![
                "email".to_owned(),
                "codes arrive in (my guess)".to_owned(),
                "your correction".to_owned(),
            ],
            vec![
                "box01@proton.example".to_owned(),
                "box02@proton.example".to_owned(),
                "box02@proton.example".to_owned(),
            ],
        ],
        b"logins",
    )
    .unwrap();
    for (read, columns) in [
        (sheets(&dump, Mode::Authoritative), &["mailbox"][..]),
        (vec![logins], &["mailbox", "mailbox_hint"][..]),
    ] {
        let mut book = Book::new();
        let first = planned(&read, &Cells::new());
        for column in columns {
            assert!(
                value(&first, "address", &login, column).is_some(),
                "{column}"
            );
        }
        apply(&mut book, &first);
        let seen = book.all();
        book.write(
            2,
            &seen,
            vec![put(&ADDRESS, &receiving, "protected", json!(false))],
        );
        let mut again = vec![marked()];
        again.extend(read);
        let plan = planned(&again, &book.cells());
        for column in columns {
            assert_ne!(
                step(&plan, "address", &login, column),
                Some(&Step::Write),
                "{column}"
            );
        }
    }
}

/// A mark only this run reads gives nothing back: the owner's word that the row is not protected
/// wins it, so the link and hint an import made stay, while no new link is made.
/// NEGATIVE CONTROL: with `marked_now` false: `` left: Some(Null) right: None ``.
#[test]
fn a_mark_the_owner_overrules_takes_nothing_back() {
    let logins = Sheet::from_records_with_bytes(
        SheetKind::Logins,
        "logins.csv",
        vec![
            vec![
                "email".to_owned(),
                "codes arrive in (my guess)".to_owned(),
                "your correction".to_owned(),
            ],
            vec![
                "box01@proton.example".to_owned(),
                "box02@proton.example".to_owned(),
                "box02@proton.example".to_owned(),
            ],
        ],
        b"logins",
    )
    .unwrap();
    let login = keys::address_pk("box01@proton.example").unwrap();
    let mut book = Book::new();
    let first = planned(std::slice::from_ref(&logins), &Cells::new());
    assert!(value(&first, "address", &login, "mailbox").is_some());
    assert!(value(&first, "address", &login, "mailbox_hint").is_some());
    apply(&mut book, &first);
    book.write(
        2,
        &[],
        vec![put(&ADDRESS, &login, "protected", json!(false))],
    );
    let marked = Sheet::parse(
        SheetKind::Emails,
        "alab-emails.csv",
        b"Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n\
          box01@proton.example,,,Active,proton,,yes,\n",
    )
    .unwrap();
    let plan = planned(&[logins, marked], &book.cells());
    assert_eq!(value(&plan, "address", &login, "mailbox"), None);
    assert_eq!(value(&plan, "address", &login, "mailbox_hint"), None);
}

/// A row read as holding no account owes no preference however early it is read: its machine's row
/// names it, and an account the registry already holds does not make that a preference.
/// NEGATIVE CONTROL: with `dump_computer` asking `no_account` only once no account is found: `` left:
/// Some(..) right: None ``.
#[test]
fn a_row_with_no_account_owes_no_preference() {
    let id = "claude:box01@proton.example";
    let dump = |tier: &str, machine: &str| {
        json!({
            "emails": [], "phones": [],
            "ai_accounts": [{
                "id": id, "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "tier": tier, "status": "active",
            }],
            "computers": [{"label": machine, "assigned_accounts": [id]}],
        })
    };
    let mut book = Book::new();
    let cells = apply(
        &mut book,
        &planned(
            &sheets(&dump("1", "m1"), Mode::Authoritative),
            &Cells::new(),
        ),
    );
    let plan = planned(&sheets(&dump("0", "m2"), Mode::Authoritative), &cells);
    let machine = keys::planned_machine_pk("m2", |_| false).unwrap();
    let account = keys::account_pk("claude", "box01@proton.example").unwrap();
    let preference = keys::preference_pk(&machine, &account).unwrap();
    assert_eq!(value(&plan, "preference", &preference, "row"), None);
    assert!(!kinds(&plan).contains(&"no-account"), "{:?}", kinds(&plan));
}

/// A phone cell holding words or a second number would run their digits into one number no phone
/// can dial: reported, whichever table states it.
/// A number grouped by dots and an international number a spreadsheet's number cell holds without
/// its `+` are one number each.
/// NEGATIVE CONTROL: with `one_number` always true: `` left: [] right: [("address", "phone")] ``.
/// With the float rule reading the last dot group: `` left: [("address", "phone")] right: [] ``.
#[test]
fn a_phone_cell_holding_more_than_a_number_is_reported() {
    let email = |phone: &str| {
        planned(
            &sheets(
                &emails(vec![with(
                    email_row(1),
                    &[("password", json!(null)), ("phone", json!(phone))],
                )]),
                Mode::Authoritative,
            ),
            &Cells::new(),
        )
    };
    let reported = vec![("address", "phone".to_owned())];
    assert_eq!(unmapped(&email("415-555-0101 / 415-555-0102")), reported);
    assert_eq!(unmapped(&email("415-555-0101 ext 12")), reported);
    assert_eq!(unmapped(&email("5550101 5550102")), reported);
    assert_eq!(unmapped(&email("4155550101.0")), reported);
    assert_eq!(unmapped(&email("(415) 555-0101")), vec![]);
    assert_eq!(unmapped(&email("+1 415.555.0101")), vec![]);
    assert_eq!(unmapped(&email(" 1-415-555-0101 ")), vec![]);
    assert_eq!(unmapped(&email("01.23.45.67.00")), vec![]);
    assert_eq!(unmapped(&email("447700900123")), vec![]);

    let plan = planned(
        &sheets(
            &phones(json!({"phone": "415-555-0102 ext 12"})),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    assert!(kinds(&plan).contains(&"free-text"), "{:?}", kinds(&plan));
    assert!(!plan.planned.iter().any(|p| p.collection == "phone"));
}

/// fleet_db's bootstrap writes `suspended` on every vendor of a row whose note says `disabled` or
/// `deactivated`, whichever vendor it names: where this vendor's mark reads a member, the word only
/// restates the note, so it is reported and pauses no one. A row whose note says nothing so is
/// paused by the word.
/// NEGATIVE CONTROL: with the `suspended` arm pausing whatever the note: `` left:
/// Some(String("paused")) ``.
#[test]
fn a_note_disabling_another_vendor_pauses_no_one() {
    let dump = |notes: Value| {
        json!({
            "emails": [], "phones": [], "computers": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "tier": "1", "status": "suspended",
                "notes": notes,
            }],
        })
    };
    let pk = keys::account_pk("claude", "box01@proton.example").unwrap();
    let plan = planned(
        &sheets(&dump(json!("codex payment disabled")), Mode::Authoritative),
        &Cells::new(),
    );
    assert_ne!(
        value(&plan, "account", &pk, "status"),
        Some(&json!("paused"))
    );
    assert!(unmapped(&plan).contains(&("account", "status".to_owned())));
    let plan = planned(
        &sheets(&dump(Value::Null), Mode::Authoritative),
        &Cells::new(),
    );
    assert_eq!(
        value(&plan, "account", &pk, "status"),
        Some(&json!("paused"))
    );
}

/// A row holding no account still says its note: its login keeps it, so every word of the replica
/// is somewhere once it goes. One a member row of the login repeats is that account's.
/// NEGATIVE CONTROL: with the `Mark::Absent` arm returning before the note: `` left: None right:
/// Some(String("refund owed")) ``. With `note_on_a_member_row` false, the login takes the member's
/// note too: `` left: Some(String("refund owed")) right: None ``.
#[test]
fn a_note_on_a_row_with_no_account_is_kept_by_its_login() {
    let row = |service: &str, provider: &str, tier: &str| {
        json!({
            "id": format!("{service}:box01@proton.example"), "provider": provider,
            "service": service, "email": "box01@proton.example", "tier": tier,
            "status": "active", "notes": "refund owed",
        })
    };
    let dump = |rows: Vec<Value>| json!({"emails": [], "phones": [], "computers": [], "ai_accounts": rows});
    let login = keys::address_pk("box01@proton.example").unwrap();
    let plan = planned(
        &sheets(
            &dump(vec![row("claude", "anthropic", "0")]),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    assert_eq!(
        value(&plan, "address", &login, "note"),
        Some(&json!("refund owed"))
    );
    let plan = planned(
        &sheets(
            &dump(vec![
                row("claude", "anthropic", "0"),
                row("codex", "openai", "1"),
            ]),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    assert_eq!(value(&plan, "address", &login, "note"), None);
}

/// Two rows with no key yet are two uncovered cells, not one: the count says how many the owner
/// has to look at.
/// NEGATIVE CONTROL: with `judge` folding keyless findings into one: `` left: 1 right: 2 ``.
#[test]
fn rows_with_no_key_are_each_uncovered() {
    let dump = json!({
        "phones": [{"phone": "call the desk"}, {"phone": "ask the office"}],
        "emails": [], "computers": [], "ai_accounts": [],
    });
    let plan = planned(&sheets(&dump, Mode::Authoritative), &Cells::new());
    assert_eq!(super::super::covered::judge(&plan, &|_| false).len(), 2);
}

/// What an account or machine row says beyond its cells is reported: a tier the mark reads as a
/// member, an id no login derives, an assigned address that is no address.
/// NEGATIVE CONTROL: with those checks removed, each assertion fails with `` left: [] ``.
#[test]
fn what_a_row_says_beyond_its_cells_is_reported() {
    struct AnyMember;
    impl Vendors for AnyMember {
        fn mark(&self, _: &str, _: &str, _: &str) -> Mark {
            Mark::Member(Terms::default())
        }
    }
    let account = |id: &str, tier: &str| {
        let read = sheets(
            &json!({
                "emails": [], "phones": [], "computers": [],
                "ai_accounts": [{
                    "id": id, "provider": "anthropic", "service": "claude",
                    "email": "box01@proton.example", "tier": tier, "status": "active",
                }],
            }),
            Mode::Authoritative,
        );
        let plan = plan(
            &Input {
                sheets: &read,
                macs: &[],
                vendors: &AnyMember,
                sealer: &SEALER,
                now_ms: NOW,
                births: true,
                unread: &[],
            },
            &Cells::new(),
        )
        .unwrap();
        unmapped(&plan)
    };
    let id = "claude:box01@proton.example";
    assert_eq!(account(id, "1"), vec![]);
    assert_eq!(account(id, "1.0"), vec![]);
    let tier = vec![("account", "tier".to_owned())];
    assert_eq!(account(id, "0.5"), tier);
    assert_eq!(account(id, "2.5"), tier);
    let free_id = vec![("account", "id".to_owned())];
    assert_eq!(account("team-seat-7", "1"), free_id);
    assert_eq!(account("codex:box01@proton.example", "1"), free_id);
    assert_eq!(account("claude:box02@proton.example", "1"), free_id);

    let machine = planned(
        &sheets(
            &computers(json!({"label": "m3", "assigned_emails": ["see the owner notes"]})),
            Mode::Authoritative,
        ),
        &Cells::new(),
    );
    assert_eq!(
        unmapped(&machine),
        vec![("machine", "assigned_emails".to_owned())]
    );
}

/// An edit to one table of the replica is an edit to every one: a table whose own rows did not change
/// takes back nothing its earlier read wrote that the edit withdrew through it, in either mode. A
/// receiving mailbox protected by the edit leaves the link its `ai_accounts` row made, and an account
/// the edit reads as none leaves the preference its machine's row made, reported.
/// NEGATIVE CONTROL: with each table keyed by its own rows (`{"file", "rows"}`): `` left:
/// Some(String("ended")) right: None `` at the preference.
#[test]
fn an_edit_to_one_table_takes_nothing_back_through_another() {
    let edited = |full: &Value, bare: &Value, mode: Mode| {
        let first = sheets(full, Mode::Authoritative);
        let mut book = Book::new();
        let cells = apply(&mut book, &planned(&first, &Cells::new()));
        let mut read = sheets(bare, mode);
        let earlier: BTreeSet<String> = first.iter().map(|sheet| sheet.source.clone()).collect();
        for sheet in &mut read {
            sheet.earlier.clone_from(&earlier);
        }
        planned(&read, &cells)
    };
    let receiving = |protected: bool| {
        json!({
            "emails": [with(email_row(2), &[("protected", json!(protected)), ("password", json!(null))])],
            "phones": [], "computers": [],
            "ai_accounts": [{
                "id": "claude:box01@proton.example", "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "receiving_mailbox": "box02@proton.example",
                "tier": "1", "status": "active",
            }],
        })
    };
    let id = "claude:box01@proton.example";
    let placed = |tier: &str| {
        json!({
            "emails": [], "phones": [],
            "ai_accounts": [{"id": id, "provider": "anthropic", "service": "claude",
                "email": "box01@proton.example", "tier": tier, "status": "active"}],
            "computers": [{"label": "m3", "assigned_accounts": [id]}],
        })
    };
    let login = keys::address_pk("box01@proton.example").unwrap();
    let machine = keys::planned_machine_pk("m3", |_| false).unwrap();
    let account = keys::account_pk("claude", "box01@proton.example").unwrap();
    let preference = keys::preference_pk(&machine, &account).unwrap();
    for mode in [Mode::Authoritative, Mode::FillOnly] {
        let plan = edited(&receiving(false), &receiving(true), mode);
        assert_ne!(
            step(&plan, "address", &login, "mailbox"),
            Some(&Step::Write),
            "{mode:?}"
        );
        let plan = edited(&placed("1"), &placed("0"), mode);
        assert_eq!(
            value(&plan, "preference", &preference, "row"),
            None,
            "{mode:?}"
        );
        assert!(
            kinds(&plan).contains(&"differs"),
            "{mode:?}: {:?}",
            kinds(&plan)
        );
    }
}

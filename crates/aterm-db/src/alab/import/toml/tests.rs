// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ai-mailbox.toml` as an import source, over hand-built histories: synthetic files of the shape
//! the harness reads (`mailbox_routes.rs`, `headless_oauth.rs`), every address `proton.example`
//! or `example.invalid`, every password `synthetic-pass-<n>`, the TOTP seed RFC 6238's published
//! example.

use super::super::tests::{SEALER, Stand};
use super::*;
use crate::{
    alab::import::{Input, Plan, covered, plan_cells as plan},
    typed::{
        cell::{self, Cells},
        grid::tests::Book,
        secret::{self, Sealer as _},
    },
};

const NOW: u64 = 1_800_000_000_000;

/// A file naming every field this build carries: the top-level mailbox and two entries.
const FILE: &str = r#"
user = "owner@proton.example"
password = "synthetic-pass-01" # gitleaks:allow
mailbox_password = "synthetic-pass-02" # gitleaks:allow
totp_secret = "JBSWY3DPEHPK3PXP"
webmail_url = "https://mail.proton.me/u/0/inbox"
imap_url = "imap://127.0.0.1:1143"

[[mailboxes]]
login = "ann@example.invalid"
password = "synthetic-pass-03" # gitleaks:allow
mailbox_password = "synthetic-pass-04" # gitleaks:allow
totp_secret = "otpauth://totp/ann?secret=JBSWY3DPEHPK3PXP"
webmail_url = "https://mail.example.invalid/inbox"
imap_url = "imaps://imap.example.invalid:993"
http_url = "http://127.0.0.1:8025"
receives = ["ai1@example.invalid", "ai2+tag@example.invalid", "*@pm.example"]

[[mailboxes]]
user = "bob@example.invalid"
pass = "synthetic-pass-05"
receives = "*@bob.example"
"#;

fn sheet(text: &str) -> Sheet {
    mailbox_sheet("ai-mailbox.toml", text.as_bytes()).unwrap()
}

fn planned(sheets: &[Sheet], cells: &Cells) -> Plan {
    plan(
        &Input {
            sheets,
            macs: &[],
            vendors: &Stand,
            sealer: &SEALER,
            now_ms: NOW,
            births: crate::alab::import::Births::Named,
            unread: &[],
        },
        cells,
    )
    .unwrap()
}

fn apply(book: &mut Book, plan: &Plan) -> Cells {
    for changes in &plan.rows {
        book.write(1, &[], changes.clone());
    }
    book.cells()
}

/// The cells the registry does not cover once `sheets` were applied: the mailbox file's rule,
/// no flag ever agreeing.
fn uncovered(sheets: &[Sheet], cells: &Cells) -> Vec<covered::Uncovered> {
    covered::judge(&planned(sheets, cells), &|_| false)
}

fn written(plan: &Plan) -> String {
    plan.rows
        .iter()
        .flatten()
        .map(|c| aterm_json::to_string(c.value.as_ref().unwrap()).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn mailbox_of(login: &str) -> String {
    keys::mailbox_pk(&keys::address_pk(login).unwrap()).unwrap()
}

/// Whether the sealed cell `collection.pk.column` opens to `value`.
fn holds(
    cells: &Cells,
    collection: &crate::typed::Collection,
    pk: &str,
    column: &str,
    value: &str,
) -> bool {
    let projection = cell::project(collection, cells);
    let Some(token) = projection.rows[pk].get(column).and_then(Value::as_str) else {
        return false;
    };
    secret::is_sealed(token)
        && SEALER.holds(
            collection.name,
            pk,
            column,
            token,
            &Secret::new(value.to_owned()),
        )
}

/// Every field of the fixture lands where the module docs say: the login passwords in the login
/// address rows, the second passwords and TOTP seeds as the mailbox's `Secret`s, the URLs, each
/// exact receive address as a `receives` element and each domain pattern in `receives_domains`,
/// the top-level mailbox receiving `*`. No plaintext is written, the file is covered, and a
/// re-run writes nothing.
/// NEGATIVE CONTROL: with the `Field::MailboxPassword | Field::Totp` arm writing nothing, the
/// second password has no cell: `` assertion failed: holds(&cells, &MAILBOX, &owner,
/// "mailbox_password", … ``.
#[test]
fn every_field_of_a_mailbox_file_lands_and_the_file_is_covered() {
    let sheets = [sheet(FILE)];
    let first = planned(&sheets, &Cells::new());
    let text = written(&first);
    assert!(!text.contains("synthetic-pass"), "no plaintext is written");
    assert!(!text.contains("JBSWY3DPEHPK3PXP"), "no seed is written");
    let mut book = Book::new();
    let cells = apply(&mut book, &first);
    let owner = mailbox_of("owner@proton.example");
    let ann = mailbox_of("ann@example.invalid");
    let bob = mailbox_of("bob@example.invalid");
    let address = |login: &str| keys::address_pk(login).unwrap();
    assert!(holds(
        &cells,
        &ADDRESS,
        &address("owner@proton.example"),
        "password",
        "synthetic-pass-01"
    ));
    assert!(holds(
        &cells,
        &MAILBOX,
        &owner,
        "mailbox_password",
        "synthetic-pass-02"
    ));
    assert!(holds(&cells, &MAILBOX, &owner, "totp", "JBSWY3DPEHPK3PXP"));
    assert!(holds(
        &cells,
        &ADDRESS,
        &address("ann@example.invalid"),
        "password",
        "synthetic-pass-03"
    ));
    assert!(holds(
        &cells,
        &MAILBOX,
        &ann,
        "mailbox_password",
        "synthetic-pass-04"
    ));
    assert!(holds(
        &cells,
        &MAILBOX,
        &ann,
        "totp",
        "otpauth://totp/ann?secret=JBSWY3DPEHPK3PXP"
    ));
    assert!(holds(
        &cells,
        &ADDRESS,
        &address("bob@example.invalid"),
        "password",
        "synthetic-pass-05"
    ));
    let mailboxes = cell::project(&MAILBOX, &cells);
    let row = |pk: &str| &mailboxes.rows[pk];
    assert_eq!(
        row(&owner).get("login"),
        Some(&json!(address("owner@proton.example")))
    );
    assert_eq!(
        row(&owner).get("webmail"),
        Some(&json!("https://mail.proton.me/u/0/inbox"))
    );
    assert_eq!(
        row(&owner).get("imap"),
        Some(&json!("imap://127.0.0.1:1143"))
    );
    assert_eq!(row(&owner).get("receives_domains"), Some(&json!(["*"])));
    assert_eq!(
        row(&ann).get("webmail"),
        Some(&json!("https://mail.example.invalid/inbox"))
    );
    assert_eq!(
        row(&ann).get("imap"),
        Some(&json!("imaps://imap.example.invalid:993"))
    );
    assert_eq!(row(&ann).get("http"), Some(&json!("http://127.0.0.1:8025")));
    assert_eq!(
        row(&ann).get("receives_domains"),
        Some(&json!(["pm.example"]))
    );
    assert_eq!(
        row(&bob).get("receives_domains"),
        Some(&json!(["bob.example"]))
    );
    let receives = row(&ann).fields["receives"].members();
    let want = [
        address("ai1@example.invalid"),
        address("ai2+tag@example.invalid"),
    ];
    assert_eq!(
        receives,
        want.iter().map(String::as_str).collect::<BTreeSet<_>>()
    );
    // The harness's reader of the registry names every mailbox, its login and its patterns.
    let mail = crate::alab::mail::mailboxes(&cell::project(&ADDRESS, &cells), &mailboxes);
    let ann_mail = mail.iter().find(|m| m.pk == ann).unwrap();
    assert_eq!(ann_mail.login, "ann@example.invalid");
    assert!(ann_mail.receives.contains(&"*@pm.example".to_owned()));
    assert!(
        ann_mail
            .receives
            .contains(&"ai1@example.invalid".to_owned())
    );
    let owner_mail = mail.iter().find(|m| m.pk == owner).unwrap();
    assert_eq!(owner_mail.receives, vec!["*".to_owned()]);
    assert_eq!(
        owner_mail.held.len(),
        3,
        "password, second password and seed"
    );
    // Covered, and a re-run writes nothing.
    assert!(
        uncovered(&sheets, &cells).is_empty(),
        "{:?}",
        uncovered(&sheets, &cells)
    );
    let again = planned(&sheets, &cells);
    assert!(again.rows.is_empty(), "{:?}", again.findings);
}

/// A field with no column (a bearer token, a per-address map, a `*_file` pointer, a maildir)
/// is an `unmapped` finding by its name, never its value, and leaves the file uncovered while
/// every other field still lands.
/// NEGATIVE CONTROL: with `mailbox_toml` dropping its `unknown` findings, the file reads as
/// covered: `` assertion failed: !left.is_empty() ``.
#[test]
fn a_field_with_no_column_leaves_the_file_uncovered() {
    let text = format!(
        "{FILE}\n[passwords]\n\"carol@example.invalid\" = \"synthetic-pass-06\"\n"
    )
    .replacen(
        "user = \"owner@proton.example\"",
        "user = \"owner@proton.example\"\nbearer_token = \"synthetic-token-07\"\ndir = \"~/Maildir\"",
        1,
    )
    .replacen(
        "pass = \"synthetic-pass-05\"",
        "pass = \"synthetic-pass-05\"\npassword_file = \"~/.secrets/bob.txt\"",
        1,
    );
    let sheets = [sheet(&text)];
    for row in &sheets[0].rows {
        for cell in row {
            assert!(!cell.contains("synthetic-"), "no value is a sheet cell");
            assert!(!cell.contains("Maildir") && !cell.contains("bob.txt"));
        }
    }
    let first = planned(&sheets, &Cells::new());
    let mut book = Book::new();
    let cells = apply(&mut book, &first);
    assert!(holds(
        &cells,
        &MAILBOX,
        &mailbox_of("ann@example.invalid"),
        "totp",
        "otpauth://totp/ann?secret=JBSWY3DPEHPK3PXP"
    ));
    let left = uncovered(&sheets, &cells);
    assert!(!left.is_empty());
    let named: BTreeSet<&str> = left
        .iter()
        .filter(|u| u.kind == "unmapped")
        .filter_map(|u| u.column.as_deref())
        .collect();
    assert_eq!(
        named,
        BTreeSet::from(["bearer_token", "dir", "password_file", "passwords{table}"])
    );
    let shown = format!("{left:?}{:?}", first.findings);
    assert!(!shown.contains("synthetic-") && !shown.contains("carol"));
}

/// A URL naming a user (`imaps://ann:pw@host`) is held as a secret, never a sheet cell or a
/// written value, and refused as credential-shaped; a value the harness reads differently from
/// this build (`url` naming a webmail page) is refused by name.
/// NEGATIVE CONTROL: with `has_userinfo` always false, the URL is a sheet cell:
/// `` assertion failed: !cell.contains("synthetic-pass-08") ``.
#[test]
fn a_url_naming_a_user_is_refused_and_never_shown() {
    let text = "[[mailboxes]]\nlogin = \"ann@example.invalid\"\n\
                imap_url = \"imaps://ann:synthetic-pass-08@imap.example.invalid:993\"\n\
                url = \"https://mail.example.invalid/\"\n";
    let sheets = [sheet(text)];
    for cell in sheets[0].rows.iter().flatten() {
        assert!(!cell.contains("synthetic-pass-08"));
    }
    let plan = planned(&sheets, &Cells::new());
    assert!(!written(&plan).contains("synthetic-pass-08"));
    let refused: Vec<(&str, Option<&str>)> = plan
        .findings
        .iter()
        .map(|f| (f.kind.word(), f.column.as_deref()))
        .collect();
    assert!(
        refused.contains(&("credential-shaped", Some("imap_url"))),
        "{refused:?}"
    );
    assert!(refused.contains(&("unmapped", Some("url"))), "{refused:?}");
}

/// An entry and the top-level mailbox naming one login merge into one row with the entry's
/// value, the top-level's other value a disagreement left uncovered; two spellings of one field
/// that differ are one too. A flag-shaped password (`1`) is refused and never covered.
/// NEGATIVE CONTROL: with entries read after the top-level row (`order` unchanged), the
/// top-level's password wins: `` assertion failed: holds(&cells, &ADDRESS, &pk, "password",
/// "synthetic-pass-10") ``.
#[test]
fn an_entry_wins_over_the_top_level_and_a_disagreement_stays_uncovered() {
    let text = "user = \"ann@example.invalid\"\npassword = \"synthetic-pass-09\"\n\
                [[mailboxes]]\nlogin = \"ann@example.invalid\"\npassword = \"synthetic-pass-10\"\n\
                totp = \"JBSWY3DPEHPK3PXP\"\notp_secret = \"GEZDGNBVGY3TQOJQ\"\n\
                [[mailboxes]]\nlogin = \"bob@example.invalid\"\npassword = \"1\"\n";
    let sheets = [sheet(text)];
    let first = planned(&sheets, &Cells::new());
    let mut book = Book::new();
    let cells = apply(&mut book, &first);
    let pk = keys::address_pk("ann@example.invalid").unwrap();
    assert!(holds(
        &cells,
        &ADDRESS,
        &pk,
        "password",
        "synthetic-pass-10"
    ));
    assert!(holds(
        &cells,
        &MAILBOX,
        &mailbox_of("ann@example.invalid"),
        "totp",
        "JBSWY3DPEHPK3PXP"
    ));
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    assert!(
        cell::project(&ADDRESS, &cells).rows[&bob]
            .get("password")
            .is_none()
    );
    let left: BTreeSet<(&str, Option<String>)> = uncovered(&sheets, &cells)
        .into_iter()
        .map(|u| (u.kind, u.column))
        .collect();
    for (kind, column) in [
        ("sources-disagree", "password"),
        ("sources-disagree", "otp_secret"),
        ("secret-flag-like", "password"),
    ] {
        assert!(left.contains(&(kind, Some(column.to_owned()))), "{left:?}");
    }
}

/// The source is named by the file's fields with each secret as `set`: no SHA-256 of the file's
/// bytes (which would be an unsalted digest over its passwords) reaches the `source` row, and a
/// changed password leaves the same source, so its own earlier cell takes the new value.
/// NEGATIVE CONTROL: with the canonical form holding each secret's text, the two files' sources
/// differ: `` assertion `left == right` failed ``.
#[test]
fn the_source_digest_holds_no_secret() {
    let one = sheet(FILE);
    let two = sheet(&FILE.replace("synthetic-pass-03", "synthetic-pass-33"));
    assert_eq!(one.source, two.source);
    assert_eq!(one.sha256, two.sha256);
    assert_ne!(
        one.sha256,
        hex::encode_lower(&Sha256::digest(FILE.as_bytes()))
    );
    // The new password is its own earlier import's cell: written over, no `differs`.
    let mut book = Book::new();
    let cells = apply(&mut book, &planned(&[one], &Cells::new()));
    let sheets = [two];
    let again = planned(&sheets, &cells);
    assert!(
        !again.findings.iter().any(|f| f.kind == Kind::Differs),
        "{:?}",
        again.findings
    );
    let cells = apply(&mut book, &again);
    let pk = keys::address_pk("ann@example.invalid").unwrap();
    assert!(holds(
        &cells,
        &ADDRESS,
        &pk,
        "password",
        "synthetic-pass-33"
    ));
}

/// The harness's wrapper tables are read as it reads them, and what the root holds besides is
/// refused by name; a file that is not TOML is refused without quoting it.
/// NEGATIVE CONTROL: with `WRAPPERS` empty, `[ai.mailbox]`'s login is an unknown table and no
/// mailbox row is planned: `` assertion failed: plan.rows.iter().flatten().any(… ``.
#[test]
fn a_wrapped_file_reads_as_the_harness_reads_it() {
    let text = "[ai.mailbox]\nuser = \"owner@proton.example\"\npassword = \"synthetic-pass-11\"\n\
                [ai.other]\nx = 1\n";
    let sheets = [sheet(text)];
    let plan = planned(&sheets, &Cells::new());
    let owner = mailbox_of("owner@proton.example");
    assert!(
        plan.rows
            .iter()
            .flatten()
            .any(|c| c.collection == MAILBOX.name && c.key.starts_with(&owner))
    );
    let names: Vec<Option<&str>> = plan
        .findings
        .iter()
        .filter(|f| f.kind == Kind::Unmapped)
        .map(|f| f.column.as_deref())
        .collect();
    assert_eq!(names, vec![Some("ai.other{table}")]);
    let err = mailbox_sheet("ai-mailbox.toml", b"password = \"synthetic-pass-12").unwrap_err();
    assert!(!err.to_string().contains("synthetic"));
}

/// A protected login holds no mailbox, so a file's entry for it seals nothing: each field it gives
/// is refused by name, its login too, and the file stays uncovered, an entry giving only its login
/// as well.
/// NEGATIVE CONTROL: with `mailbox_toml` not asking `guarded`, the mailbox's second password is
/// sealed: `` assertion failed: !plan.rows.iter().flatten().any(… ``. With the guard skipping the
/// login: `` left: [Some("mailbox_password"), Some("totp_secret")] ``.
#[test]
fn a_protected_login_takes_no_mailbox_from_a_file() {
    let login = keys::address_pk("owner@proton.example").unwrap();
    let mut book = Book::new();
    book.write(
        2,
        &[],
        vec![
            crate::typed::grid::tests::put(&ADDRESS, &login, "addr", json!("owner@proton.example")),
            crate::typed::grid::tests::put(&ADDRESS, &login, "protected", json!(true)),
            crate::typed::grid::tests::put(&ADDRESS, &login, "row", json!("live")),
        ],
    );
    let text = "user = \"owner@proton.example\"\nmailbox_password = \"synthetic-pass-22\"\n\
                totp_secret = \"JBSWY3DPEHPK3PXP\"\n";
    let sheets = [sheet(text)];
    let plan = planned(&sheets, &book.cells());
    assert!(
        !plan
            .rows
            .iter()
            .flatten()
            .any(|c| c.collection == MAILBOX.name),
        "{:?}",
        plan.rows
    );
    let refused: Vec<Option<&str>> = plan
        .findings
        .iter()
        .filter(|f| f.kind == Kind::ProtectedSecret)
        .map(|f| f.column.as_deref())
        .collect();
    assert_eq!(
        refused,
        vec![Some("mailbox_password"), Some("totp_secret"), Some("user")]
    );
    assert!(!uncovered(&sheets, &book.cells()).is_empty());
    let alone = [sheet("user = \"owner@proton.example\"\n")];
    assert!(!uncovered(&alone, &book.cells()).is_empty());
}

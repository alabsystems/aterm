// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's views on a real lab: a scratch Commons whose registry a fixture fleet_db replica
//! filled, read in place. No live vault, no window: the grid's opener is a closure that edits
//! the file as the owner would, and the secret store is an in-memory one ([`Memory`]).

use super::*;
use crate::alab::command::tests::{NOW, email_row, replica_fixture, words};
use crate::alab::import::tests::Stand;
use crate::table::tests::Lab;
use crate::typed::{grid::tests::put, keys, secret::Memory};
use aterm_secrets::commons::roster::Role;
use std::path::Path;

/// A rekey cut short right after its rename leaves no `database.json` at the replica's path:
/// each spelling of a fleet_db verb names the rekey, and the import refuses alike.
#[test]
fn a_rekey_cut_short_after_its_rename_refuses_fleet_db_and_the_import() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let dir = replica_fixture(paths, 2, "active");
    let db = crate::local::Database::open(&dir).unwrap();
    let id = aterm_codec::hex::encode_lower(&db.credentials.manifest.id[..8]);
    drop(db);
    aterm_uds::private_file::write_private(&dir, crate::fleet_db::REKEY_UNFINISHED, b"0").unwrap();
    std::fs::rename(
        &dir,
        dir.with_file_name(format!("lab-db.before-rekey-{id}")),
    )
    .unwrap();

    for line in [
        &["scan", "emails"][..],
        &["get", "emails", "ada.fb@alpha.example"],
        &["tables"],
    ] {
        let held = dir.clone();
        let refused = crate::fleet_db::try_run_with(&words(line), move || Ok(held)).unwrap_err();
        assert!(refused.to_string().contains("aterm db rekey"), "{refused}");
    }
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let refused = crate::alab::command::tests::run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap_err();
    assert!(refused.to_string().contains("aterm db rekey"), "{refused}");
}

/// A lab whose registry holds `n` imported mailbox logins, each password
/// `synthetic-pass-<n>` in the replica.
fn imported(n: usize) -> Lab {
    let lab = Lab::new(&[Role::Writer]);
    crate::alab::tests::newest(&lab.paths[0], NOW);
    replica_fixture(&lab.paths[0], n, "active");
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    crate::alab::command::tests::run(
        &lab.paths[0],
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    lab
}

/// Run one view with the grid opener `open`, standard input `stdin`, and the secret store
/// `store`.
fn view(
    paths: &Paths,
    noun: Noun,
    list: &[&str],
    open: &mut dyn FnMut(&Path, usize) -> Result<()>,
    stdin: &[u8],
    store: &Memory,
) -> Result<Value> {
    let bytes = stdin.to_vec();
    let mut read = move || Ok(zeroize::Zeroizing::new(bytes.clone()));
    let mut io = ViewIo {
        open,
        read_stdin: &mut read,
        store,
        // A test's refusing floor stands for one Mac holding the roll back.
        holding: &|| {
            Ok(aterm_secrets::client::Holding {
                older: usize::from(store.admit().is_err()),
                ..aterm_secrets::client::Holding::default()
            })
        },
        now_ms: NOW,
    };
    run(paths, noun, &words(list), &mut io)
}

fn no_grid(_: &Path, _: usize) -> Result<()> {
    panic!("no grid opens")
}

/// The row of a listing whose `ad` key is `address`'s.
fn email(listed: &Value, address: &str) -> Value {
    let pk = keys::address_pk(address).unwrap();
    listed["emails"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["ad"] == json!(pk))
        .unwrap()
        .clone()
}

/// The emails view shows every row with its address as it is (ordinary data, no flag to show
/// it) and no password's text anywhere: a table holds no secret to show. The flags that showed a
/// secret or digested a table are gone and refused as unknown.
/// NEGATIVE CONTROL: with `shown_row` masking addresses (as `--show-addresses` once gated),
/// `` left: String("••••••") right: String("box01@proton.example") ``.
#[test]
fn emails_show_addresses_as_they_are_and_no_password() {
    let lab = imported(3);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let listed = view(paths, Noun::Emails, &[], &mut no_grid, b"", &store).unwrap();
    assert_eq!(listed["emails"].as_array().unwrap().len(), 3, "{listed}");
    assert_eq!(
        email(&listed, "box01@proton.example")["addr"],
        json!("box01@proton.example")
    );
    let text = aterm_json::to_string(&listed).unwrap();
    assert!(!text.contains("synthetic-pass"), "{text}");
    for gone in [
        "--show-addresses",
        "--show-passwords",
        "--show-numbers",
        "--digest",
    ] {
        let refused = view(paths, Noun::Emails, &[gone], &mut no_grid, b"", &store).unwrap_err();
        assert!(refused.to_string().contains("unknown option"), "{refused}");
    }
}

/// `set … password --stdin` puts the value into the secret store and writes only the reference
/// to its entry: the listing and the grid show the entry's name, the table's cells hold
/// `{vault, entry}`, and the JSON the verb prints names the entry and never the value. A value on
/// argv or shaped like a flag is refused with nothing stored. An empty value clears the cell and
/// removes the entry.
/// NEGATIVE CONTROL: every value [`Memory::every`] was ever handed is absent from each printed
/// output, every table cell and the grid file; with `set` writing the plaintext into the cell
/// (`Value::String(text)` in place of the reference), the stored value is in the table:
/// `` the value reached the table ``.
#[test]
fn set_puts_a_password_in_the_store_and_only_its_entry_name_in_the_table() {
    let lab = imported(2);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let mut printed = Vec::new();

    let on_argv = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "argv-password-1"],
        &mut no_grid,
        b"",
        &store,
    );
    assert!(on_argv.is_err());
    let flag = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "--stdin"],
        &mut no_grid,
        b"1\n",
        &store,
    );
    assert!(flag.unwrap_err().to_string().contains("flag"));
    assert!(store.every().is_empty(), "nothing refused was stored");

    let written = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "--stdin"],
        &mut no_grid,
        b"piped-password-new\n",
        &store,
    )
    .unwrap();
    // The new value takes a fresh tagged entry of the cell's, and the entry the replaced
    // candidate named is removed.
    let entry = written["entry"].as_str().unwrap().to_owned();
    assert!(
        secret::bound("address", "password", &box01, &entry),
        "{entry}"
    );
    assert_eq!(written["written"], json!(1), "{written}");
    assert_eq!(written["removed"], json!(1), "{written}");
    assert_eq!(
        store.held(&entry).as_deref(),
        Some("piped-password-new"),
        "the trailing newline is removed and the value is in the store"
    );
    printed.push(written);

    let cell = addresses(paths).rows[&box01].get("password").cloned();
    assert_eq!(
        cell,
        Some(secret::reference(Memory::VAULT, &entry)),
        "the cell holds the reference only"
    );
    let listed = view(paths, Noun::Emails, &[], &mut no_grid, b"", &store).unwrap();
    assert_eq!(
        email(&listed, "box01@proton.example")["password"],
        json!(entry)
    );
    printed.push(listed);

    let mut grid_text = String::new();
    let mut open = |file: &Path, _: usize| -> Result<()> {
        grid_text = std::fs::read_to_string(file).unwrap();
        Ok(())
    };
    printed.push(view(paths, Noun::Emails, &["--edit"], &mut open, b"", &store).unwrap());
    let header: Vec<&str> = grid_text.lines().next().unwrap().split('\t').collect();
    assert!(
        header.contains(&"=password"),
        "{header:?}: a secret column is read-only in the grid"
    );
    assert!(grid_text.contains(&entry), "the grid names the entry");

    // The negative control: no value the store was handed reached any output or any cell.
    let cells = format!(
        "{:?}",
        crate::alab::Registry::at(paths.clone())
            .view()
            .unwrap()
            .cells
    );
    let every = store.every();
    assert_eq!(every, ["piped-password-new"]);
    for value in &every {
        for out in &printed {
            let text = aterm_json::to_string(out).unwrap();
            assert!(!text.contains(value.as_str()), "printed: {text}");
        }
        assert!(
            !cells.contains(value.as_str()),
            "the value reached the table"
        );
        assert!(
            !grid_text.contains(value.as_str()),
            "the value reached the grid"
        );
    }

    // An empty value clears the cell and removes the entry.
    let cleared = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "--stdin"],
        &mut no_grid,
        b"",
        &store,
    )
    .unwrap();
    assert_eq!(cleared["written"], json!(1), "{cleared}");
    assert_eq!(cleared["entry"], Value::Null);
    assert_eq!(store.held(&entry), None, "the entry is removed");
    assert!(
        addresses(paths).rows[&box01]
            .get("password")
            .is_none_or(Value::is_null),
        "the reference is cleared"
    );
}

/// A revert of a secret write, a clear included, is refused, naming why: its preimage names the
/// entry the write removed once it landed, and the new entry would stay with no cell naming it.
/// The cell keeps naming what it named, and the store keeps its value.
/// NEGATIVE CONTROL: with `Handle::revert` skipping `refuse_secret_revert`, the revert lands and
/// the cell names the removed entry: `` called `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn a_revert_never_restores_a_secret_cell() {
    use crate::typed::cell::Field;
    let lab = imported(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let set = |stdin: &[u8]| {
        view(
            paths,
            Noun::Emails,
            &["set", "box01@proton.example", "password", "--stdin"],
            &mut no_grid,
            stdin,
            &store,
        )
        .unwrap()
    };
    let handle = crate::att::Handle::at(paths.clone(), &crate::att::REGISTRY);
    let last_write = || match addresses(paths).rows[&box01].fields.get("password") {
        Some(Field::Value(candidate)) => candidate.operation,
        other => panic!("one candidate: {other:?}"),
    };
    let written = set(b"piped-password-new\n");
    let entry = written["entry"].as_str().unwrap().to_owned();
    let refused = handle
        .revert([7; 16], last_write())
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("a revert never restores a secret cell (address.password)"),
        "{refused}"
    );
    assert_eq!(
        addresses(paths).rows[&box01].get("password").cloned(),
        Some(secret::reference(Memory::VAULT, &entry))
    );
    assert_eq!(store.held(&entry).as_deref(), Some("piped-password-new"));

    set(b"");
    let refused = handle
        .revert([8; 16], last_write())
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("a revert never restores a secret cell"),
        "{refused}"
    );
    assert!(
        addresses(paths).rows[&box01]
            .get("password")
            .is_none_or(Value::is_null)
    );
}

/// A secret write that landed reports what it wrote even when the settle after it fails (the
/// owed record cannot be written): the cell names the new entry, the report says so, counts as
/// owed each entry the write replaced and its own put (whose payment the failed settle may not
/// have recorded), and names the settle's failure, never a value. A retry would put yet another
/// entry.
/// NEGATIVE CONTROL: with `settle_set(…)?` evaluated before `committed?` (before the fix), the
/// set fails with the settle's error: `` called `Result::unwrap()` on an `Err` value: Error("the
/// memory store's owed record cannot be written") ``.
#[test]
fn a_landed_set_reports_its_outcome_when_its_settle_fails() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    store.break_records();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let written = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "--stdin"],
        &mut no_grid,
        b"piped-password-new\n",
        &store,
    )
    .unwrap();
    assert_eq!(written["written"], json!(1), "{written}");
    let entry = written["entry"].as_str().unwrap().to_owned();
    assert_eq!(
        addresses(paths).rows[&box01].get("password").cloned(),
        Some(secret::reference(Memory::VAULT, &entry))
    );
    let why = written["settle_error"].as_str().unwrap();
    assert!(why.contains("owed record"), "{written}");
    assert!(!why.contains("piped-password-new"));
    assert_eq!(written["removed"], json!(0), "{written}");
    assert_eq!(
        written["owed"].as_u64(),
        written["replaced"].as_u64().map(|replaced| replaced + 1),
        "each replaced entry and the put itself: {written}"
    );
}

/// A protected row takes no secret: `set … password --stdin` on one is refused before anything
/// is stored.
/// NEGATIVE CONTROL: with `set` skipping the protected check, the value is stored:
/// `` assertion failed: store.every().is_empty() ``.
#[test]
fn a_protected_row_takes_no_password() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    crate::alab::Registry::at(paths.clone())
        .write(vec![vec![put(&ADDRESS, &box01, "protected", json!(true))]])
        .unwrap();
    let refused = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "--stdin"],
        &mut no_grid,
        b"protected-row-password\n",
        &store,
    )
    .unwrap_err();
    assert!(refused.to_string().contains("protected"), "{refused}");
    assert!(!refused.to_string().contains("protected-row-password"));
    assert!(store.every().is_empty());
}

/// A phone's number is ordinary data: `set <row> number <value>` takes it from argv, stores the
/// registry's one form of it (its digits), the listing shows it as stored, and a value that is
/// no phone number is refused. Nothing goes to the secret store.
/// NEGATIVE CONTROL: with `number` a secret column, the argv value is refused: `` called
/// `Result::unwrap()` on an `Err` value: Error("a number never goes on the command line…") ``.
#[test]
fn a_phone_number_is_written_and_shown_plainly() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let phone = keys::random_pk(keys::Random::Phone).unwrap();
    crate::alab::Registry::at(paths.clone())
        .write(vec![vec![
            put(&PHONE, &phone, "last4", json!("1234")),
            put(&PHONE, &phone, "label", json!("desk")),
            put(&PHONE, &phone, "row", json!("live")),
        ]])
        .unwrap();
    let written = view(
        paths,
        Noun::Phones,
        &["set", "desk", "number", "+15550101234"],
        &mut no_grid,
        b"",
        &store,
    )
    .unwrap();
    assert_eq!(written["written"], json!(1), "{written}");
    assert!(written.get("entry").is_none(), "{written}");
    let listed = view(paths, Noun::Phones, &[], &mut no_grid, b"", &store).unwrap();
    let row = listed["phones"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["ph"] == json!(phone))
        .unwrap()
        .clone();
    assert_eq!(row["number"], json!("5550101234"), "{listed}");
    assert!(
        view(
            paths,
            Noun::Phones,
            &["set", "desk", "number", "not-a-number"],
            &mut no_grid,
            b"",
            &store,
        )
        .is_err()
    );
    assert!(store.every().is_empty());
}

/// A lab whose registry is at the generation a new lab starts at, one builds up to 0.200.0 read
/// (its secret columns and a phone's number sealed tokens), with `n` imported mailbox logins,
/// each with its password sealed into its cell.
fn imported_sealed(n: usize) -> Lab {
    let lab = Lab::new(&[Role::Writer]);
    replica_fixture(&lab.paths[0], n, "active");
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    crate::alab::command::tests::run(
        &lab.paths[0],
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    lab
}

/// A phone row labelled `desk` in `paths`' registry, with no number yet: its key.
fn desk_phone(paths: &Paths) -> String {
    let phone = keys::random_pk(keys::Random::Phone).unwrap();
    crate::alab::Registry::at(paths.clone())
        .write(vec![vec![
            put(&PHONE, &phone, "last4", json!("1234")),
            put(&PHONE, &phone, "label", json!("desk")),
            put(&PHONE, &phone, "row", json!("live")),
        ]])
        .unwrap();
    phone
}

/// Seals `text` into `column` of `noun`'s row `pk`, as a build up to 0.200.0 sealed whatever its
/// `set … --stdin` or its grid was given.
fn seal_cell(paths: &Paths, noun: Noun, pk: &str, column: &str, text: &str) {
    crate::alab::Registry::at(paths.clone())
        .seal_as_older(noun.collection().name, pk, column, text)
        .unwrap();
}

/// What the sealed token in `column` of `noun`'s row `pk` opens to: a test's own read of it.
fn opened_cell(paths: &Paths, noun: Noun, pk: &str, column: &str) -> Option<String> {
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let collection = noun.collection_at(&handle).name;
    let token = stored_cell(paths, noun, pk, column)?.as_str()?.to_owned();
    handle
        .open_cells(&[(collection, pk, column, token.as_str())])
        .unwrap()
        .pop()
        .flatten()
        .map(|secret| secret.plaintext().to_owned())
}

/// What `noun`'s row `pk` holds in `column`, as stored.
fn stored_cell(paths: &Paths, noun: Noun, pk: &str, column: &str) -> Option<Value> {
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let held = handle.view().unwrap().project(noun.collection_at(&handle));
    held.rows[pk].get(column).cloned()
}

/// The `phones` view's row `pk`.
fn phone_row(paths: &Paths, pk: &str) -> Value {
    let store = Memory::default();
    let listed = view(paths, Noun::Phones, &[], &mut no_grid, b"", &store).unwrap();
    listed["phones"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["ph"] == json!(pk))
        .unwrap()
        .clone()
}

/// Where the generation holds a phone's number sealed (the form builds up to 0.200.0 read), it
/// is still ordinary data: `phones` shows it with no flag, and `set <row> number` takes it on
/// the command line or from `--stdin`, holds what is typed to a number's shape, and stores the
/// registry's one form of it (the importer's digits) with the row's `last4`, sealed, so the
/// stored cell is still a token. An empty value clears the number and leaves `last4`. Nothing
/// goes to the secret store.
/// NEGATIVE CONTROL: with `run` skipping `att::open_ordinary` for a noun's ordinary column
/// (`grid::Opened::new()` in its place), the view shows the sealed number as a mask:
/// `` left: String("••••••") right: String("5550101234") ``.
#[test]
fn a_sealed_generation_shows_a_phone_number_and_seals_the_one_set() {
    let lab = imported_sealed(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let phone = desk_phone(paths);
    let set = |value: &[&str], stdin: &[u8]| {
        let mut words = vec!["set", "desk", "number"];
        words.extend_from_slice(value);
        view(paths, Noun::Phones, &words, &mut no_grid, stdin, &store)
    };
    assert_eq!(phone_row(paths, &phone)["number"], Value::Null);
    let written = set(&[" +15550101234 "], b"").unwrap();
    assert_eq!(written["written"], json!(1), "last4 already says 1234");
    assert!(written.get("entry").is_none(), "{written}");
    assert_eq!(phone_row(paths, &phone)["number"], json!("5550101234"));
    let token = stored_cell(paths, Noun::Phones, &phone, "number");
    assert!(
        token
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(crate::typed::secret::is_sealed),
        "this generation holds the number sealed"
    );
    let again = set(&["+15550101234"], b"").unwrap();
    assert_eq!(again["written"], json!(0), "the number it already holds");
    assert_eq!(stored_cell(paths, Noun::Phones, &phone, "number"), token);
    let piped = set(&["--stdin"], b"15550109999\n").unwrap();
    assert_eq!(piped["written"], json!(2), "the number and its last4");
    let row = phone_row(paths, &phone);
    assert_eq!(row["number"], json!("5550109999"));
    assert_eq!(row["last4"], json!("9999"));
    for shape in ["555 0101", "123456", "not-a-number", "+1 (202) 555-0149 x2"] {
        let refused = set(&[shape], b"").unwrap_err().to_string();
        assert!(refused.contains("7 to 15 digits"), "{shape}: {refused}");
    }
    assert_eq!(
        phone_row(paths, &phone)["number"],
        json!("5550109999"),
        "a refusal wrote none"
    );
    let cleared = set(&[""], b"").unwrap();
    assert_eq!(cleared["written"], json!(1), "{cleared}");
    let row = phone_row(paths, &phone);
    assert_eq!(row["number"], Value::Null);
    assert_eq!(row["last4"], json!("9999"), "a clear leaves last4");
    assert!(store.every().is_empty());
}

/// What a sealed `number` cell holds is shown only when it reads as a phone number: text a build
/// up to 0.200.0 sealed there under a secret's contract (any `--stdin` text, a credential
/// included) stays masked, and so does a token that does not open at its own coordinates.
/// NEGATIVE CONTROL: with `att::open_ordinary` keeping every opened value (`numbers` always
/// true in `open_with`), the view prints the sealed sentence: `` left: String("5550149 pin
/// 482913") right: String("••••••") ``.
#[test]
fn a_sealed_number_cell_that_holds_no_number_stays_masked() {
    // In halves, so the export guard never reads a token here.
    const TOKEN: &str = concat!("ghp", "_cccccccccccccccccccccccc");
    let lab = imported_sealed(1);
    let paths = &lab.paths[0];
    let phone = desk_phone(paths);
    for held in ["5550149 pin 482913", TOKEN] {
        seal_cell(paths, Noun::Phones, &phone, "number", held);
        assert_eq!(
            phone_row(paths, &phone)["number"],
            json!(crate::typed::secret::MASK),
            "a sealed cell that is no number"
        );
    }
    // A number as an older build's `--stdin` took it, separators and all, still shows.
    seal_cell(paths, Noun::Phones, &phone, "number", "+1 (202) 555-0149");
    assert_eq!(
        phone_row(paths, &phone)["number"],
        json!("+1 (202) 555-0149")
    );
    // Another row's token does not open at this row's coordinates.
    let token = stored_cell(paths, Noun::Phones, &phone, "number").unwrap();
    let other = keys::random_pk(keys::Random::Phone).unwrap();
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let sealed_phone = Noun::Phones.collection_at(&handle);
    crate::alab::Registry::at(paths.clone())
        .write(vec![vec![
            put(&PHONE, &other, "last4", json!("0149")),
            put(&PHONE, &other, "label", json!("spare")),
            put(&PHONE, &other, "row", json!("live")),
            put(sealed_phone, &other, "number", token),
        ]])
        .unwrap();
    assert_eq!(
        phone_row(paths, &other)["number"],
        json!(crate::typed::secret::MASK),
        "a token that does not open"
    );
}

/// Only a column whose values are ordinary data opens with nothing journaled: asked for a
/// password, the same opener opens nothing, whatever the cell holds (here text that would read
/// as a number).
/// NEGATIVE CONTROL: with `open_with` taking every `Ty::Secret` column (`sealed_ordinary`
/// answering true for a secret-named one), the password is opened: `` address.password ``.
#[test]
fn the_unjournaled_opener_opens_no_secret_column() {
    let lab = imported_sealed(1);
    let paths = &lab.paths[0];
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    seal_cell(paths, Noun::Emails, &box01, "password", "5550104321");
    let phone = desk_phone(paths);
    seal_cell(paths, Noun::Phones, &phone, "number", "5550101234");
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    let cells = handle.view().unwrap().cells;
    let address = Noun::Emails.collection_at(&handle);
    let phones = Noun::Phones.collection_at(&handle);
    assert!(
        stored_cell(paths, Noun::Emails, &box01, "password")
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(crate::typed::secret::is_sealed),
        "control: the lab holds a sealed password"
    );
    for (collection, column) in [
        (address, "password"),
        (address, "number"),
        (phones, "label"),
    ] {
        let opened = att::open_ordinary(&handle, collection, column, &cells).unwrap();
        assert!(opened.is_empty(), "{}.{column}", collection.name);
    }
    assert!(
        att::open_every_ordinary(&handle, address, &cells)
            .unwrap()
            .0
            .is_empty()
    );
    let opened = att::open_ordinary(&handle, phones, "number", &cells).unwrap();
    assert_eq!(opened.len(), 1, "control: the number opens");
}

/// Where the generation holds a password as a sealed token (the form builds up to 0.200.0 read),
/// no view shows it and none reveals it: the listing says the mask, and the flags that showed a
/// secret are unknown options. A `set … --stdin` there seals the value into its cell, the one
/// form those builds read: the secret store is not asked and the reply names no entry. The same
/// value again is sealed again, so a write never says whether a value was the held one. A value
/// on the command line and one shaped like a flag are refused, and an empty one clears the cell.
/// NEGATIVE CONTROL: with `set`'s `Holds::SealedSecret` arm writing `Value::Null` for any value,
/// the cell holds no token after the set: `the value is sealed into its cell`.
#[test]
fn a_sealed_generation_masks_a_password_and_seals_one_from_stdin() {
    let lab = imported_sealed(2);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let token = |paths: &Paths| {
        stored_cell(paths, Noun::Emails, &box01, "password")
            .as_ref()
            .and_then(Value::as_str)
            .filter(|text| crate::typed::secret::is_sealed(text))
            .map(str::to_owned)
    };
    let imported = token(paths).expect("the import sealed the fixture's password");
    assert_eq!(
        opened_cell(paths, Noun::Emails, &box01, "password").as_deref(),
        Some("synthetic-pass-01")
    );
    let listed = view(paths, Noun::Emails, &[], &mut no_grid, b"", &store).unwrap();
    for address in ["box01@proton.example", "box02@proton.example"] {
        assert_eq!(
            email(&listed, address)["password"],
            json!(crate::typed::secret::MASK)
        );
    }
    let text = aterm_json::to_string(&listed).unwrap();
    assert!(!text.contains("synthetic-pass") && !text.contains(&imported));
    for gone in ["--show-passwords", "--digest"] {
        let refused = view(paths, Noun::Emails, &[gone], &mut no_grid, b"", &store).unwrap_err();
        assert!(refused.to_string().contains("unknown option"), "{refused}");
    }
    let set = |stdin: &[u8]| {
        view(
            paths,
            Noun::Emails,
            &["set", "box01@proton.example", "password", "--stdin"],
            &mut no_grid,
            stdin,
            &store,
        )
    };
    let written = set(b"piped-password-9\n").unwrap();
    assert_eq!(written["written"], json!(1), "{written}");
    assert!(written.get("entry").is_none(), "{written}");
    assert!(
        !aterm_json::to_string(&written)
            .unwrap()
            .contains("piped-password-9")
    );
    let sealed = token(paths).expect("the value is sealed into its cell");
    assert_ne!(sealed, imported);
    assert_eq!(
        opened_cell(paths, Noun::Emails, &box01, "password").as_deref(),
        Some("piped-password-9")
    );
    assert!(
        store.every().is_empty(),
        "a sealed generation asks nothing of the store"
    );
    let again = set(b"piped-password-9\n").unwrap();
    assert_eq!(again["written"], json!(1), "{again}");
    assert_ne!(token(paths).expect("sealed again"), sealed);
    let held = token(paths);
    let flag = set(b"yes\n").unwrap_err().to_string();
    assert!(flag.contains("shaped like a flag"), "{flag}");
    let on_argv = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "typed-on-argv"],
        &mut no_grid,
        b"",
        &store,
    )
    .unwrap_err();
    assert!(on_argv.to_string().contains("command line"), "{on_argv}");
    assert_eq!(token(paths), held, "a refused set leaves the cell");
    let listed = view(paths, Noun::Emails, &[], &mut no_grid, b"", &store).unwrap();
    assert_eq!(
        email(&listed, "box01@proton.example")["password"],
        json!(crate::typed::secret::MASK)
    );
    let cleared = set(b"").unwrap();
    assert_eq!(cleared["written"], json!(1), "{cleared}");
    assert!(cleared.get("entry").is_none(), "{cleared}");
    assert_eq!(
        stored_cell(paths, Noun::Emails, &box01, "password").filter(|cell| !cell.is_null()),
        None,
        "an empty value clears it"
    );
    assert!(store.every().is_empty());
}

/// `set` writes an ordinary column from argv.
#[test]
fn set_writes_an_ordinary_column_from_argv() {
    let lab = imported(2);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let status = view(
        paths,
        Noun::Emails,
        &["set", "box02@proton.example", "status", "disabled"],
        &mut no_grid,
        b"",
        &store,
    )
    .unwrap();
    assert_eq!(status["written"], json!(1), "{status}");
    let listed = view(paths, Noun::Emails, &[], &mut no_grid, b"", &store).unwrap();
    assert_eq!(
        email(&listed, "box02@proton.example")["status"],
        json!("disabled")
    );
}

/// The address rows of `paths`' registry.
fn addresses(paths: &Paths) -> cell::Projection {
    crate::alab::Registry::at(paths.clone())
        .view()
        .unwrap()
        .project(&ADDRESS)
}

/// `emails set` names a row by the address its `addr` says now: a renamed row goes by its new
/// address and keeps its birth key, its old address names no row (the refusal names the row born
/// with it), and the rename writes the row's `domain` in the same record. A rename onto an address
/// another row says, onto one whose birth key another row holds, or of a row a live account
/// signs in with is refused with nothing written and no address in the refusal; a rename back to
/// the row's birth address passes.
/// NEGATIVE CONTROLS: with `row_named`'s `emails` arm keyed by `keys::address_pk` (as before),
/// the new address names no row: `` called `Result::unwrap()` on an `Err` value: Error("no email
/// row holds that address") ``; with `commit_planned` skipping the guard for the owner (`if false
/// && owner && let Some(refused)`), the rename writes no `domain`: `` assertion `left == right`
/// failed: addr and domain: {"ad":"ad-…","column":"addr","table":"emails","written":1} ``.
#[test]
fn set_names_a_renamed_row_by_its_current_address_and_refuses_a_held_one() {
    let lab = imported(3);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let set = |list: &[&str]| view(paths, Noun::Emails, list, &mut no_grid, b"", &store);
    let pk = |address: &str| keys::address_pk(address).unwrap();
    let (box01, box02, box03) = (
        pk("box01@proton.example"),
        pk("box02@proton.example"),
        pk("box03@proton.example"),
    );
    let renamed = set(&[
        "set",
        "box01@proton.example",
        "addr",
        "renamed01@example.test",
    ])
    .unwrap();
    assert_eq!(renamed["ad"], json!(box01));
    assert_eq!(renamed["written"], json!(2), "addr and domain: {renamed}");
    let rows = addresses(paths);
    assert_eq!(
        rows.rows[&box01].get("addr"),
        Some(&json!("renamed01@example.test"))
    );
    assert_eq!(
        rows.rows[&box01].get("domain"),
        Some(&json!("example.test"))
    );
    assert!(!rows.rows.contains_key(&pk("renamed01@example.test")));
    // The new address names the row; the old one names none.
    let status = set(&["set", "renamed01@example.test", "status", "disabled"]).unwrap();
    assert_eq!(status["ad"], json!(box01));
    let old = set(&["set", "box01@proton.example", "status", "active"])
        .unwrap_err()
        .to_string();
    assert!(old.contains(&box01) && !old.contains('@'), "{old}");
    // Onto an address another row says, onto a renamed row's birth address, or of an account's
    // login: refused, nothing written.
    let before = addresses(paths);
    let onto_held = set(&[
        "set",
        "box02@proton.example",
        "addr",
        "renamed01@example.test",
    ]);
    assert!(onto_held.is_err());
    let onto_key = set(&[
        "set",
        "box02@proton.example",
        "addr",
        "box01@proton.example",
    ]);
    assert!(onto_key.is_err());
    let account = keys::account_pk("claude", "box03@proton.example").unwrap();
    crate::alab::Registry::at(paths.clone())
        .write(vec![vec![
            put(&ACCOUNT, &account, "realm", json!("claude")),
            put(&ACCOUNT, &account, "login", json!(box03)),
            put(&ACCOUNT, &account, "row", json!("live")),
        ]])
        .unwrap();
    let login = set(&[
        "set",
        "box03@proton.example",
        "addr",
        "renamed03@example.test",
    ]);
    for refused in [&onto_held, &onto_key, &login] {
        let text = refused.as_ref().unwrap_err().to_string();
        assert!(!text.contains('@'), "{text}");
    }
    assert!(login.unwrap_err().to_string().contains(&account));
    let after = addresses(paths);
    for key in [&box01, &box02, &box03] {
        assert_eq!(after.rows[key].get("addr"), before.rows[key].get("addr"));
    }
    // Back to its birth address.
    let back = set(&[
        "set",
        "renamed01@example.test",
        "addr",
        "box01@proton.example",
    ])
    .unwrap();
    assert_eq!(back["ad"], json!(box01));
    let rows = addresses(paths);
    assert_eq!(
        rows.rows[&box01].get("domain"),
        Some(&json!("proton.example"))
    );
}

/// Type `to` into the `addr` cell of the grid row that says `from` in `file`; returns that row's
/// 1-based line.
fn type_addr(file: &Path, from: &str, to: &str) -> usize {
    let held = std::fs::read_to_string(file).unwrap();
    let mut lines: Vec<String> = held.lines().map(str::to_owned).collect();
    let header: Vec<&str> = lines[0].split('\t').collect();
    let at = header.iter().position(|h| *h == "addr").unwrap();
    let row = lines.iter().position(|l| l.contains(from)).unwrap();
    let mut cells: Vec<String> = lines[row].split('\t').map(str::to_owned).collect();
    cells[at] = to.to_owned();
    lines[row] = cells.join("\t");
    std::fs::write(file, format!("{}\n", lines.join("\n"))).unwrap();
    row + 1
}

/// The grid's own edit of an `addr` cell passes the same rules as `set`: a save that types
/// another row's address into a row writes nothing and reopens the owner's file as they saved it,
/// the caret on that row, the refusal reported by row and column and never an address (closing it
/// unchanged ends the session, abandoned); one that types a fresh address writes it with the
/// row's `domain` beside it, though the save never typed the domain.
/// NEGATIVE CONTROLS: with `Handle::apply` skipping the guard (`if false && let Some(refused)`),
/// the colliding save is written and the grid never reopens: `` assertion `left == right` failed:
/// {"abandoned":false,"born":0,"cells":1,"problems":[],"records":1,"retired":0,"rounds":1,
/// "table":"emails"} left: Number(Number(Unsigned(1))) right: Number(Number(Unsigned(2))) ``;
/// with the edit session ignoring a refused save (`if false && let Some(refused) =
/// &applied.refused`), it ends there, nothing written and nothing reported: `` assertion `left
/// == right` failed: {"abandoned":false,"born":0,"cells":0,"problems":[],"records":0,
/// "retired":0,"rounds":1,"table":"emails"} left: Number(Number(Unsigned(1))) right:
/// Number(Number(Unsigned(2))) ``.
#[test]
fn the_grids_edit_of_an_address_passes_the_owners_rules() {
    let lab = imported(2);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let mut saved: Option<(usize, String)> = None;
    let mut reopened: Option<(usize, String)> = None;
    let mut colliding = |file: &Path, line: usize| -> Result<()> {
        match saved {
            None => {
                let line = type_addr(file, "box01@proton.example", "box02@proton.example");
                saved = Some((line, std::fs::read_to_string(file).unwrap()));
            }
            Some(_) => {
                reopened = Some((line, std::fs::read_to_string(file).unwrap()));
            }
        }
        Ok(())
    };
    let collided = view(
        paths,
        Noun::Emails,
        &["--edit"],
        &mut colliding,
        b"",
        &store,
    )
    .unwrap();
    assert_eq!(collided["rounds"], json!(2), "{collided}");
    assert_eq!(collided["abandoned"], json!(true));
    assert_eq!(collided["cells"], json!(0));
    assert_eq!(reopened, saved);
    let problems = collided["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{collided}");
    let problem = problems[0].as_str().unwrap();
    let box02 = keys::address_pk("box02@proton.example").unwrap();
    assert!(
        problem.contains("column addr") && problem.contains(&box02) && !problem.contains('@'),
        "{problem}"
    );
    assert_eq!(
        addresses(paths).rows[&box01].get("addr"),
        Some(&json!("box01@proton.example"))
    );
    let mut fresh = |file: &Path, _: usize| -> Result<()> {
        type_addr(file, "box01@proton.example", "fresh01@example.test");
        Ok(())
    };
    let edited = view(paths, Noun::Emails, &["--edit"], &mut fresh, b"", &store).unwrap();
    assert_eq!(edited["cells"], json!(2), "addr and domain: {edited}");
    let rows = addresses(paths);
    assert_eq!(
        rows.rows[&box01].get("addr"),
        Some(&json!("fresh01@example.test"))
    );
    assert_eq!(
        rows.rows[&box01].get("domain"),
        Some(&json!("example.test"))
    );
}

/// A grid folder a killed session left (its maker's pid no longer runs) is swept when the next
/// grid opens; one whose maker runs is kept.
#[test]
fn a_killed_sessions_grid_folder_is_swept_at_the_next_edit() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let mut planted = Vec::new();
    let mut open = |file: &Path, _: usize| -> Result<()> {
        let parent = file.parent().unwrap().parent().unwrap();
        if planted.is_empty() {
            // A pid no process holds (above the macOS pid ceiling) and our own.
            for pid in [99_999_999u32, std::process::id()] {
                let folder = parent.join(format!(
                    "att-edit-{pid}_0000000000000000_0{}",
                    aterm_types::private_edit::DIR_SUFFIX
                ));
                std::fs::create_dir_all(&folder).unwrap();
                std::fs::write(folder.join("address.tsv"), "left behind").unwrap();
                planted.push(folder);
            }
        }
        Ok(())
    };
    view(paths, Noun::Emails, &["--edit"], &mut open, b"", &store).unwrap();
    view(paths, Noun::Emails, &["--edit"], &mut open, b"", &store).unwrap();
    assert!(!planted[0].exists(), "the dead maker's folder is swept");
    assert!(planted[1].exists(), "a running maker's folder is kept");
    std::fs::remove_dir_all(&planted[1]).unwrap();
}

/// No noun reads fleet_db, whatever this Mac's registry holds: on a Mac with a fleet_db replica
/// and an empty registry (where the nouns used to fall through to the replica, and on a Mac
/// whose registry refused answered its 0 rows), `emails` answers no rows and `never` with the
/// reason, and `ai-accounts` answers its shared file's snapshot; once an import fills the
/// registry, `emails` answers its rows `live`.
/// NEGATIVE CONTROL: with `run` returning the registry read's error for `emails`, as it did
/// before: `` called `Result::unwrap()` on an `Err` value: Error("table enrollment claim is
/// missing or damaged") ``.
#[test]
fn the_nouns_never_read_fleet_db_whatever_the_registry_holds() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    replica_fixture(paths, 1, "active");
    let store = Memory::default();
    let read = |noun: Noun| view(paths, noun, &[], &mut no_grid, b"", &store).unwrap();

    let emails = read(Noun::Emails);
    assert_eq!(emails["state"].as_str(), Some("never"), "{emails}");
    assert_eq!(emails["why"].as_str(), Some("catalog"), "{emails}");
    assert_eq!(emails["emails"], json!([]), "the replica's row is not read");
    let accounts = read(Noun::AiAccounts);
    assert_eq!(accounts["file"].as_str(), Some("ai/accounts"), "{accounts}");
    assert_eq!(accounts["state"].as_str(), Some("never"), "{accounts}");
    assert_eq!(accounts["ai-accounts"], json!([]));
    let computers = read(Noun::Computers);
    assert_eq!(
        computers["file"].as_str(),
        Some("lab/machines"),
        "{computers}"
    );
    crate::alab::tests::newest(paths, NOW);
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    crate::alab::command::tests::run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    let emails = read(Noun::Emails);
    assert_eq!(emails["state"].as_str(), Some("live"), "{emails}");
    assert_eq!(
        emails["emails"].as_array().map(Vec::len),
        Some(1),
        "{emails}"
    );
    let (email, _) = email_row(1, "active");
    assert_eq!(Noun::parse("emails"), Some(Noun::Emails));
    assert_eq!(Noun::parse("ai_accounts"), Some(Noun::AiAccounts));
    assert_eq!(Noun::parse(&email), None);
}

/// E1: on a loaded Mac a read of a registry that was there failed ("application pass deadline
/// expired", or busy while a sync landed), and the view and `check` answered
/// `"registry": "not imported"` with an import as the next step. "Not imported" is now the
/// catalog's word: a declared registry's failed read is that error.
/// NEGATIVE CONTROL: with `unread` answering "not imported" for every error (its old rule), the
/// first assertion fails; observed.
#[test]
fn a_failed_read_of_a_declared_registry_is_that_error_and_never_not_imported() {
    let lab = imported(1);
    for error in [
        Error::new("application pass deadline expired"),
        Error::snapshot_changed(),
    ] {
        let said = error.to_string();
        let answer = super::super::unread(&lab.paths[0], error, "next");
        assert!(answer.is_err_and(|e| e.to_string() == said), "{said}");
    }
    let bare = Lab::new(&[Role::Writer]);
    let missing = super::super::unread(
        &bare.paths[0],
        Error::new("application table is not initialized"),
        "next",
    )
    .unwrap();
    assert_eq!(missing["registry"].as_str(), Some("not imported"));
    assert_eq!(missing["next"].as_str(), Some("next"));
}

/// One `account-obs` row of `account` written by the Mac `uuid`, under `key`.
fn obs_row(
    key: &str,
    account: &str,
    state: &str,
    tabs: u64,
    at: u64,
) -> (String, crate::typed::owned::OwnedRow) {
    (
        key.to_owned(),
        crate::typed::owned::OwnedRow {
            machine: [0; 16],
            actor: [0; 48],
            value: json!({"r": 1, "n": 1, "account": account, "cli": "claude", "state": state,
                "tabs": tabs, "first_at": at, "at": at, "src": "profile"}),
            rank: (0, 1, 1),
            operation: [0; 32],
        },
    )
}

const M65: &str = "65656565656565656565656565656565";
const M22: &str = "22222222222222222222222222222222";
const UNICORN: &str = "ac-00000000000000a1";
const BRONTO: &str = "ac-00000000000000b2";

fn named(uuid: &str) -> String {
    match uuid {
        M65 => "m65 (cheetah)".into(),
        M22 => "m22 (brontosaurus)".into(),
        other => other[..8].into(),
    }
}

fn fleet() -> crate::typed::owned::OwnedRead {
    crate::typed::owned::OwnedRead {
        rows: [
            obs_row(
                &format!("{UNICORN}.{M65}"),
                UNICORN,
                "running",
                1,
                NOW - 12_000,
            ),
            obs_row(
                &format!("{BRONTO}.{M22}"),
                BRONTO,
                "running",
                2,
                NOW - 3_600_000,
            ),
            obs_row(
                &format!("{BRONTO}.{M65}"),
                BRONTO,
                "idle",
                0,
                NOW - 7_200_000,
            ),
            // Not its own account's key: no Mac's word.
            obs_row(&format!("{UNICORN}.{M22}"), BRONTO, "running", 5, NOW),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

/// `ai-accounts` says where each account runs and when it was last seen, from every Mac's
/// `account-obs` rows, each Mac a tab runs it on by name with its tabs (an idle row dates the
/// account but runs it nowhere); a row whose key is not its own account's is skipped, and an
/// account no Mac observed shows neither.
/// NEGATIVE CONTROL: with `seen_on` skipping no row by its key, unicorn's last sighting is the
/// stray row's: `` left: "0 s" right: "12 s" ``.
#[test]
fn ai_accounts_seen_on_and_last_seen() {
    let seen = seen_on(&fleet(), &[], &named, NOW);
    let mut rows = vec![
        json!({"ac": UNICORN, "status": "active"}),
        json!({"ac": BRONTO, "status": "active"}),
        json!({"ac": "ac-00000000000000c3", "status": "active"}),
    ];
    shown_seen(&mut rows, &seen, NOW);
    assert_eq!(rows[0]["seen_on"], json!("m65 (cheetah) \u{d7}1"));
    assert_eq!(rows[0]["last_seen"], json!("12 s"));
    assert_eq!(rows[1]["seen_on"], json!("m22 (brontosaurus) \u{d7}2"));
    assert_eq!(rows[1]["last_seen"], json!("1 h"));
    assert_eq!(rows[2]["seen_on"], Value::Null);
    assert_eq!(rows[2]["last_seen"], Value::Null);
    assert_eq!(
        rows[0]["status"],
        json!("active"),
        "an owner's status stands"
    );
}

/// A catalog `pending` account a Mac runs now reads as signed in there: observation wins over
/// belief. One only idle somewhere stays pending.
/// NEGATIVE CONTROL: with `shown_seen` leaving `status` alone, `` left: "pending" right: "signed
/// in · on m65 (cheetah)" ``.
#[test]
fn observed_signed_in_beats_catalog_pending() {
    let seen = seen_on(&fleet(), &[], &named, NOW);
    let mut rows = vec![
        json!({"ac": UNICORN, "status": "pending"}),
        json!({"ac": BRONTO, "status": Value::Null}),
    ];
    shown_seen(&mut rows, &seen, NOW);
    assert_eq!(
        rows[0]["status"],
        json!("signed in \u{b7} on m65 (cheetah)")
    );
    assert_eq!(
        rows[1]["status"],
        json!("signed in \u{b7} on m22 (brontosaurus)")
    );
    let idle = crate::typed::owned::OwnedRead {
        rows: [obs_row(
            &format!("{UNICORN}.{M65}"),
            UNICORN,
            "idle",
            0,
            NOW,
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let mut rows = vec![json!({"ac": UNICORN, "status": "pending"})];
    shown_seen(&mut rows, &seen_on(&idle, &[], &named, NOW), NOW);
    assert_eq!(rows[0]["status"], json!("pending"));
}

/// A held account a Mac runs keeps the owner's word first and says the Mac runs it: a paused,
/// broken or banned row is never shown as plainly signed in (which reads as usable), nor as its
/// hold alone (which hides that a Mac runs it anyway). An active one is left as it is.
/// NEGATIVE CONTROLS: with `shown_seen` leaving a held status alone (as before), `` left:
/// "paused" right: "paused · signed in on m65 (cheetah)" ``; with every status replaced, the
/// active row reads `` left: "signed in · on m65 (cheetah)" right: "active" ``.
#[test]
fn a_held_account_a_mac_runs_keeps_its_hold_and_says_so() {
    let seen = seen_on(&fleet(), &[], &named, NOW);
    for hold in ["paused", "broken", "banned"] {
        let mut rows = vec![json!({"ac": UNICORN, "status": hold})];
        shown_seen(&mut rows, &seen, NOW);
        assert_eq!(
            rows[0]["status"],
            json!(format!("{hold} \u{b7} signed in on m65 (cheetah)"))
        );
    }
    let mut rows = vec![json!({"ac": UNICORN, "status": "active"})];
    shown_seen(&mut rows, &seen, NOW);
    assert_eq!(rows[0]["status"], json!("active"));
}

fn heard(uuid: &str, account: &str, tab: &str, t_ms: u64, ended: bool) -> Heard {
    Heard {
        uuid: uuid.to_owned(),
        account: account.to_owned(),
        tab: Some(tab.to_owned()),
        t_ms,
        ended,
    }
}

/// A Mac stops running an account once nothing says so within two hours: a `running` row no
/// later sighting refreshed lapses, however long ago it was written, while a tab's heard `seen`
/// keeps it on, counted by distinct tabs, until that tab's `session-end`.
/// NEGATIVE CONTROL: with `seen_on` taking every running row whatever its age, the lapsed row
/// still runs on m65: `` left: "m65 (cheetah) ×1" right: Null ``; with heard events ignored,
/// m22's two tabs are not counted: `` left: Null right: "m22 (brontosaurus) ×2" ``.
#[test]
fn seen_on_lapses_without_a_sighting_and_counts_heard_tabs() {
    let lapsed = crate::typed::owned::OwnedRead {
        rows: [obs_row(
            &format!("{UNICORN}.{M65}"),
            UNICORN,
            "running",
            1,
            NOW - 3 * 3_600_000,
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let events = [
        heard(M22, UNICORN, "aaaaaaaa", NOW - 60_000, false),
        heard(M22, UNICORN, "bbbbbbbb", NOW - 3_000_000, false),
        heard(M22, UNICORN, "aaaaaaaa", NOW - 120_000, false),
        heard(M22, UNICORN, "cccccccc", NOW - 30_000, false),
        heard(M22, UNICORN, "cccccccc", NOW - 10_000, true),
        heard(M65, BRONTO, "dddddddd", NOW - 3 * 3_600_000, false),
    ];
    let seen = seen_on(&lapsed, &events, &named, NOW);
    let mut rows = vec![
        json!({"ac": UNICORN, "status": "pending"}),
        json!({"ac": BRONTO, "status": "pending"}),
    ];
    shown_seen(&mut rows, &seen, NOW);
    assert_eq!(rows[0]["seen_on"], json!("m22 (brontosaurus) \u{d7}2"));
    assert_eq!(rows[0]["last_seen"], json!("30 s"));
    assert_eq!(
        rows[0]["status"],
        json!("signed in \u{b7} on m22 (brontosaurus)")
    );
    assert_eq!(rows[1]["seen_on"], Value::Null, "seen three hours ago");
    assert_eq!(rows[1]["last_seen"], json!("3 h"));
    assert_eq!(rows[1]["status"], json!("pending"));
    let alone = seen_on(&lapsed, &[], &named, NOW);
    assert!(alone[UNICORN].macs.is_empty(), "{alone:?}");
}

/// A row that says `signed-out` is no sighting in the registry view either: an account only
/// such a row names shows no Mac and no `last_seen`, and a Mac whose row says so runs the
/// account no more, though the tab it was seen in still dates when it was last seen.
/// NEGATIVE CONTROL: with `seen_on` keeping an account no Mac sighted, at time 0, unicorn's
/// `last_seen` reads `` left: String("20833 d") right: Null ``.
#[test]
fn a_signed_out_row_is_no_sighting_and_runs_nothing_in_the_registry_view() {
    let out = crate::typed::owned::OwnedRead {
        rows: [
            obs_row(
                &format!("{UNICORN}.{M65}"),
                UNICORN,
                "signed-out",
                0,
                NOW - 60_000,
            ),
            obs_row(
                &format!("{BRONTO}.{M65}"),
                BRONTO,
                "signed-out",
                0,
                NOW - 60_000,
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let events = [heard(M65, BRONTO, "aaaaaaaa", NOW - 120_000, false)];
    let mut rows = vec![
        json!({"ac": UNICORN, "status": "pending"}),
        json!({"ac": BRONTO, "status": "pending"}),
    ];
    shown_seen(&mut rows, &seen_on(&out, &events, &named, NOW), NOW);
    assert_eq!(rows[0]["seen_on"], Value::Null);
    assert_eq!(rows[0]["last_seen"], Value::Null);
    assert_eq!(rows[0]["status"], json!("pending"));
    assert_eq!(rows[1]["seen_on"], Value::Null, "the row ended the tab");
    assert_eq!(rows[1]["last_seen"], json!("2 min"));
    assert_eq!(rows[1]["status"], json!("pending"));
}

/// Replacing or clearing a secret cell removes the entries its candidates named, whatever entry
/// that was (a roll's `.2` included), and a write never lands on an entry that holds another
/// value no candidate names (a concurrent writer's): that value survives.
/// NEGATIVE CONTROLS: the entry recomputed from the coordinates alone (the cell's own) holds the
/// concurrent value after the replace and the clear; with `set` removing that recomputed entry,
/// `` the concurrent value survives `` fails, and with `held_entries` empty the rolled `.2`
/// survives: `` the replaced entry is removed ``.
#[test]
fn replacing_or_clearing_a_secret_cell_removes_the_entries_its_candidates_named() {
    use crate::typed::secret::Store;
    let lab = imported(2);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let own = secret::entry_name("address", "password", &box01).unwrap();
    let rolled = crate::typed::secret::test_entry(("address", "password", &box01), 2);
    store
        .put(&rolled, &secret::Secret::new("rolled-value-2".into()))
        .unwrap();
    crate::alab::Registry::at(paths.clone())
        .write(vec![vec![
            cell::change(
                &ADDRESS,
                CellKey {
                    pk: &box01,
                    col: "password",
                    elem: None,
                },
                &CellValue::owner(ADDRESS.version, secret::reference(Memory::VAULT, &rolled)),
            )
            .unwrap(),
        ]])
        .unwrap();
    // Another writer's value sits in the cell's own entry, which no candidate here names.
    store
        .put(&own, &secret::Secret::new("concurrent-value-1".into()))
        .unwrap();

    let set = |input: &[u8]| {
        view(
            paths,
            Noun::Emails,
            &["set", "box01@proton.example", "password", "--stdin"],
            &mut no_grid,
            input,
            &store,
        )
        .unwrap()
    };
    let replaced = set(b"replaced-value-3\n");
    let third = replaced["entry"].as_str().unwrap().to_owned();
    assert!(
        secret::bound("address", "password", &box01, &third),
        "{third}"
    );
    assert!(third != own && third != rolled, "{third}");
    assert_eq!(replaced["removed"], json!(1), "{replaced}");
    assert_eq!(store.held(&rolled), None, "the replaced entry is removed");
    assert_eq!(store.held(&third).as_deref(), Some("replaced-value-3"));
    assert_eq!(
        store.held(&own).as_deref(),
        Some("concurrent-value-1"),
        "the concurrent value survives"
    );
    assert_eq!(
        addresses(paths).rows[&box01].get("password"),
        Some(&secret::reference(Memory::VAULT, &third))
    );

    let cleared = set(b"");
    assert_eq!(cleared["removed"], json!(1), "{cleared}");
    assert_eq!(store.held(&third), None, "the cleared entry is removed");
    assert_eq!(
        store.held(&own).as_deref(),
        Some("concurrent-value-1"),
        "the concurrent value survives"
    );
}

/// A write naming another cell's entry, or a credential's, is refused before it is written, and a
/// candidate another writer stored naming one makes the column raw, and the grid shows the mask,
/// never the entry it names (the sign-in never reads it: `alab::mail`'s tests).
/// NEGATIVE CONTROL: the same write naming the cell's own entry passes.
#[test]
fn a_secret_cell_naming_an_entry_it_does_not_own_is_refused_and_never_read() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let key = CellKey {
        pk: &box01,
        col: "password",
        elem: None,
    };
    let at =
        |entry: &str| CellValue::owner(ADDRESS.version, secret::reference(Memory::VAULT, entry));
    for stolen in ["gh_access_token", "alab-address-password-ad-other"] {
        assert!(
            cell::change(&ADDRESS, key, &at(stolen)).is_err(),
            "{stolen}"
        );
    }
    let own = secret::entry_name("address", "password", &box01).unwrap();
    assert!(cell::change(&ADDRESS, key, &at(&own)).is_ok());
    // A writer that skipped the check: its candidate is unreadable here and shown masked.
    let stray = vec![crate::database::Change {
        collection: ADDRESS.name.to_owned(),
        key: key.encode().unwrap(),
        value: Some(at("gh_access_token").encode()),
    }];
    crate::alab::Registry::at(paths.clone())
        .write(vec![stray])
        .unwrap();
    assert!(addresses(paths).rows[&box01].raw.contains("password"));
    let view = crate::alab::Registry::at(paths.clone()).view().unwrap();
    let handle = crate::att::Handle::at(paths.clone(), &crate::att::REGISTRY);
    let grid =
        crate::typed::grid::export(handle.compiled().table, &ADDRESS, &view.cells, &[]).unwrap();
    assert!(!grid.text.contains("gh_access_token"), "{}", grid.text);
    assert!(grid.text.contains(secret::MASK));
}

/// A set removes exactly the entry its landed write replaced; one whose removal fails stays owed.
/// A value another run put and has not committed yet survives every set while that run is live,
/// and once it has ended a later set's sweep removes it with what the failed removal left: no
/// value stays in the store with no cell naming it, and no run removes another's put.
/// NEGATIVE CONTROLS: the stuck entry still holds its value after the second set (`owed` 1);
/// with `settle_set` skipping the retire, the second set owes nothing (`owed` 0); with `sweep`
/// reading every owed entry instead of `orphaned`, the second set collects what live runs owe
/// (`owed` 2); with `settle_set` skipping the sweep, `` a removal that failed is collected ``
/// fails.
#[test]
fn a_set_retires_what_it_replaced_and_never_another_live_runs_put() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let set = |input: &[u8]| {
        view(
            paths,
            Noun::Emails,
            &["set", "box01@proton.example", "password", "--stdin"],
            &mut no_grid,
            input,
            &store,
        )
        .unwrap()
    };
    let first = set(b"first-value-1\n");
    let first_entry = first["entry"].as_str().unwrap().to_owned();
    store.stick(&first_entry);
    let second = set(b"second-value-2\n");
    assert_eq!(second["owed"], json!(1), "{second}");
    assert_eq!(store.held(&first_entry).as_deref(), Some("first-value-1"));
    let second_entry = second["entry"].as_str().unwrap().to_owned();
    // Another run's write, its value put and its reference not yet committed.
    let uncommitted = secret::store_cell(
        &store,
        "address",
        "password",
        &box01,
        &secret::Secret::new("uncommitted-value-3".into()),
    )
    .unwrap();
    let uncommitted = secret::entry_of(&uncommitted).unwrap().to_owned();

    store.unstick(&first_entry);
    let third = set(b"third-value-4\n");
    assert_eq!(
        store.held(&second_entry),
        None,
        "the replaced entry is removed"
    );
    assert_eq!(
        store.held(&uncommitted).as_deref(),
        Some("uncommitted-value-3"),
        "another live run's put survives"
    );
    let third_entry = third["entry"].as_str().unwrap().to_owned();

    store.end_runs();
    let fourth = set(b"fourth-value-5\n");
    assert_eq!(fourth["owed"], json!(0), "{fourth}");
    assert_eq!(
        store.held(&first_entry),
        None,
        "a removal that failed is collected"
    );
    assert_eq!(
        store.held(&uncommitted),
        None,
        "the uncommitted value is removed"
    );
    assert_eq!(store.held(&third_entry), None);
    let fourth_entry = fourth["entry"].as_str().unwrap();
    assert_eq!(store.held(fourth_entry).as_deref(), Some("fourth-value-5"));
    assert_eq!(store.names(), [fourth_entry.to_owned()]);
    assert!(store.owed().is_empty());
}

/// On a registry at a generation builds up to 0.200.0 read (the first, then the second), a
/// secret this build writes is a sealed token in its cell, the one form those builds read: an
/// import seals each password it writes and an `emails set` seals the one piped to it, nothing
/// goes into the store, and no reference reaches the registry. The roll from the first
/// generation to the second runs; the one to the third, whose secret cells name store entries,
/// is held back by this Mac's fleet floor (no release is named as the first that writes no
/// secret file, so every Mac holds it), and the registry stays at the second.
/// NEGATIVE CONTROL: with `Registry::roll_with` not asking the floor, the registry rolls to the
/// third generation: `` called `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn below_the_third_generation_a_secret_is_sealed_in_its_cell_and_no_roll_reaches_the_third() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    replica_fixture(paths, 1, "active");
    let registry = crate::alab::Registry::at(paths.clone());
    registry.open().unwrap();
    let store = Memory::default();
    let import = || {
        let mut never = |_: &Path| -> Result<()> { panic!("no review") };
        crate::alab::command::tests::run(
            paths,
            &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
            Some(&Stand),
            &mut never,
            NOW,
        )
    };
    let set = || {
        view(
            paths,
            Noun::Emails,
            &["set", "box01@proton.example", "password", "--stdin"],
            &mut no_grid,
            b"piped-password-9\n",
            &store,
        )
    };
    // The secret cells that hold a sealed token, and those that hold anything else.
    let secrets = || {
        let cells = crate::alab::Registry::at(paths.clone())
            .view()
            .unwrap()
            .cells;
        let mut sealed = 0;
        let mut other = 0;
        for ((_, key), cell) in &cells {
            if !CellKey::parse(key).is_ok_and(|key| typed::secret_named(key.col)) {
                continue;
            }
            for (_, value) in crate::typed::cell::origins_refs(cell) {
                match &value["x"] {
                    Value::Null => {}
                    Value::String(text) if crate::typed::secret::is_sealed(text) => sealed += 1,
                    _ => other += 1,
                }
            }
        }
        (sealed, other)
    };
    for generation in [1, 2] {
        assert_eq!(registry.generation().generation, generation);
        let report = import().unwrap();
        assert!(report.get("secrets_waiting").is_none(), "{report}");
        let written = set().unwrap();
        assert_eq!(written["written"], json!(1), "{generation}: {written}");
        assert!(written.get("entry").is_none(), "{written}");
        assert!(
            store.every().is_empty(),
            "a sealed generation asks nothing of the store"
        );
        let (sealed, other) = secrets();
        assert!(sealed >= 1, "generation {generation} holds the password");
        assert_eq!(
            other, 0,
            "a secret of generation {generation} is not sealed"
        );
        if generation == 1 {
            let rolled = registry.roll(true, NOW).unwrap();
            assert!(rolled.sealed, "{rolled:?}");
        }
    }
    let refused = registry.roll(true, NOW).unwrap_err().to_string();
    assert!(
        refused.contains("no registry secret is written until every Mac reading the vault"),
        "{refused}"
    );
    assert_eq!(registry.generation().generation, 2);
    assert!(registry.generation().successor.is_some());
}

/// `--registry` keeps the nouns' own projection, read through custody, with what the snapshot
/// does not hold yet: `ai-accounts` carries `seen_on` and `last_seen`, `computers` each live seat
/// no machine row binds. The plain view is the shared file's snapshot and carries neither. It
/// goes with the plain view only, and only with a noun that has a shared file.
/// NEGATIVE CONTROL: with `run` answering the snapshot for `--registry` too, the view is the
/// file's, which no pass has written: `` left: Some("never") right: Some("live") ``.
#[test]
fn the_registry_view_keeps_the_observed_columns_and_the_unbound_seats() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = crate::alab::Registry::at(paths.clone());
    registry.open().unwrap();
    let machine = keys::machine_pk("m7");
    let (address, account) = (
        keys::address_pk("ada.fb@alpha.example").unwrap(),
        keys::account_pk("claude", "ada.fb@alpha.example").unwrap(),
    );
    registry
        .write(vec![vec![
            put(&MACHINE, &machine, "label", json!("m7")),
            put(&MACHINE, &machine, "status", json!("active")),
            put(&ADDRESS, &address, "addr", json!("ada.fb@alpha.example")),
            put(&ACCOUNT, &account, "provider", json!("anthropic")),
            put(&ACCOUNT, &account, "realm", json!("claude")),
            put(&ACCOUNT, &account, "login", json!(address)),
            put(&ACCOUNT, &account, "status", json!("pending")),
        ]])
        .unwrap();
    let store = Memory::default();
    let read = |noun: Noun, list: &[&str]| view(paths, noun, list, &mut no_grid, b"", &store);

    let accounts = read(Noun::AiAccounts, &["--registry"]).unwrap();
    assert_eq!(accounts["state"].as_str(), Some("live"), "{accounts}");
    assert!(accounts.get("file").is_none(), "{accounts}");
    let rows = accounts["ai-accounts"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{accounts}");
    let row = rows[0].as_object().unwrap();
    assert_eq!(row["ac"], json!(account));
    assert_eq!(
        row["status"],
        json!("pending"),
        "no Mac was seen running it"
    );
    assert!(row.contains_key("seen_on") && row.contains_key("last_seen"));

    let computers = read(Noun::Computers, &["--registry"]).unwrap();
    let listed = computers["computers"].as_array().unwrap();
    assert!(
        listed
            .iter()
            .any(|row| row["mc"] == json!(machine) && row["seats"] == json!([])),
        "{computers}"
    );
    assert!(
        listed.iter().any(|row| row["mc"].is_null()
            && row["seats"]
                .as_array()
                .is_some_and(|seats| seats.len() == 1)),
        "this Mac's seat, which no machine row binds: {computers}"
    );

    // The plain view is the file's snapshot: no pass ran, so it answers `never`, and it has no
    // observed column.
    let plain = read(Noun::AiAccounts, &[]).unwrap();
    assert_eq!(plain["file"].as_str(), Some("ai/accounts"), "{plain}");
    assert_eq!(plain["state"].as_str(), Some("never"), "{plain}");

    for (noun, list) in [
        (Noun::Emails, &["--registry"][..]),
        (Noun::AiAccounts, &["--registry", "set", "x", "label", "y"]),
        (Noun::Computers, &["--registry", "--edit"]),
    ] {
        assert!(read(noun, list).is_err(), "{noun:?} {list:?}");
    }
}

/// `set <machine> nickname_aliases <members>` writes registry.4's text set member by member
/// (LT-15B): each member the text names that the set lacks is added, each it holds that the text
/// leaves out is removed, and one another writer added since stands unless the text drops it.
/// NEGATIVE CONTROL (run 2026-11-15): with `set` reading every set as holding nothing (`before`
/// empty), the second write adds both members again and removes none: `` left: Number(…(2))
/// right: Number(…(1)) ``.
#[test]
fn a_text_set_is_set_member_by_member() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let store = Memory::default();
    let machine = keys::machine_pk("m70");
    let registry = crate::alab::Registry::at(paths.clone());
    registry
        .write(vec![vec![
            put(&registry::MACHINE, &machine, "label", json!("m70")),
            put(&registry::MACHINE, &machine, "status", json!("active")),
            put(&registry::MACHINE, &machine, "row", json!("live")),
        ]])
        .unwrap();
    let set = |members: &str| {
        view(
            paths,
            Noun::Computers,
            &["set", "m70", "nickname_aliases", members],
            &mut no_grid,
            b"",
            &store,
        )
        .unwrap()
    };
    let members = || {
        let view = crate::alab::Registry::at(paths.clone()).view().unwrap();
        let machines = view.project(&registry::MACHINE);
        let column = view
            .collection(&registry::MACHINE)
            .column("nickname_aliases")
            .unwrap();
        let mut held: Vec<String> = cell::members(
            column,
            machines.rows[&machine].fields.get("nickname_aliases"),
        )
        .into_iter()
        .map(str::to_owned)
        .collect();
        held.sort_unstable();
        held
    };
    assert_eq!(set("fb, Red Balloon")["written"], json!(2));
    assert_eq!(members(), ["Red Balloon", "fb"]);
    // Another writer's member, added since.
    registry
        .write(vec![vec![crate::typed::grid::tests::elem(
            &registry::MACHINE,
            &machine,
            "nickname_aliases",
            "studio",
        )]])
        .unwrap();
    assert_eq!(set("fb, studio")["written"], json!(1));
    assert_eq!(members(), ["fb", "studio"]);
    assert_eq!(set("studio, fb")["written"], json!(0));
}

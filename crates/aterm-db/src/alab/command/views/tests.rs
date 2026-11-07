// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's views on a real lab: a scratch Commons whose registry a fixture fleet_db replica
//! filled, read in place. No live vault, no window: the grid's opener is a closure that edits
//! the file as the owner would, and the session's answer is injected.

use super::*;
use crate::alab::command::tests::{NOW, email_row, replica_fixture, words};
use crate::alab::import::tests::Stand;
use crate::table::tests::Lab;
use crate::typed::{grid::tests::put, keys};
use aterm_secrets::commons::roster::Role;
use std::path::Path;

/// A rekey cut short right after its rename leaves no `database.json` at the replica's path: the
/// nouns stay with fleet_db, each spelling of a verb names the rekey, and the import refuses alike.
/// NEGATIVE CONTROL: with `serves` reading the missing file as no replica:
/// `` assertion failed: !serves(paths) ``.
#[test]
fn a_rekey_cut_short_after_its_rename_keeps_the_nouns_with_fleet_db() {
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

    assert!(!serves(paths));
    for line in [
        &["emails"][..],
        &["emails", "--edit"],
        &["emails", "--digest"],
    ] {
        let held = dir.clone();
        let refused = crate::fleet_db::try_run_with(&words(line), move || Ok(held)).unwrap_err();
        assert!(refused.to_string().contains("aterm db rekey"), "{refused}");
    }
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let refused = super::super::run(
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
/// `synthetic-pass-<n>`.
fn imported(n: usize) -> Lab {
    let lab = Lab::new(&[Role::Writer]);
    replica_fixture(&lab.paths[0], n, "active");
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    super::super::run(
        &lab.paths[0],
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    lab
}

/// Run one view with the session's answer `gate`, the grid opener `open`, standard input
/// `stdin`, and the reveal journal in `journal`.
fn view(
    paths: &Paths,
    noun: Noun,
    list: &[&str],
    gate: &dyn Fn() -> Result<Place>,
    open: &mut dyn FnMut(&Opening<'_>) -> Result<()>,
    stdin: &[u8],
    journal: &Journal,
) -> Result<Value> {
    let bytes = stdin.to_vec();
    let mut read = move || Ok(zeroize::Zeroizing::new(bytes.clone()));
    let mut io = ViewIo {
        gate,
        open,
        read_stdin: &mut read,
        journal,
        now_ms: NOW,
    };
    run(paths, noun, &words(list), &mut io)
}

/// The session's answer outside aterm: no session holds the caller, so its terminal is the
/// person's own.
fn allowed() -> Result<Place> {
    Ok(Place::Outside)
}

/// The session's answer in a person's aterm tab.
fn in_tab() -> Result<Place> {
    Ok(Place::Tab(TAB.to_owned()))
}

/// The person's tab [`in_tab`] names.
const TAB: &str = "s-0a17";

fn spawned() -> Result<Place> {
    Err(Error::new("plaintext is not shown here: spawned"))
}

fn no_grid(_: &Opening<'_>) -> Result<()> {
    panic!("no grid opens")
}

/// The reveal journal's records: one line per revealed cell, coordinates only.
fn journaled(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("reveals.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| aterm_json::from_str(line).unwrap())
        .collect()
}

/// The emails view shows every row with its password masked and addresses masked; with
/// `--show-passwords` in a session that may reveal, each password in plaintext, each reveal
/// journaled first by coordinates only; in a session that may not (a `ctl spawn`ed one), it
/// refuses before any cell is opened and journals nothing.
/// NEGATIVE CONTROL: with `run` skipping the gate for `--show-passwords`, the spawned session
/// reveals: `` assertion failed: refused.is_err() ``.
#[test]
fn emails_mask_passwords_and_reveal_only_where_the_session_may() {
    let lab = imported(3);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let plain = view(
        paths,
        Noun::Emails,
        &[],
        &spawned,
        &mut no_grid,
        b"",
        &journal,
    )
    .unwrap();
    let rows = plain["emails"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{plain}");
    let text = aterm_json::to_string(&plain).unwrap();
    assert!(
        !text.contains("synthetic-pass") && !text.contains("box01@"),
        "{text}"
    );
    assert!(
        rows.iter().all(|r| r["password"] == json!(MASK_TEXT)),
        "{plain}"
    );
    let refused = view(
        paths,
        Noun::Emails,
        &["--show-passwords"],
        &spawned,
        &mut no_grid,
        b"",
        &journal,
    );
    assert!(refused.is_err());
    assert!(journaled(scratch.path()).is_empty(), "nothing was opened");
    let shown = view(
        paths,
        Noun::Emails,
        &["--show-passwords"],
        &allowed,
        &mut no_grid,
        b"",
        &journal,
    )
    .unwrap();
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let row = shown["emails"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["ad"] == json!(box01))
        .unwrap();
    assert_eq!(row["password"], json!("synthetic-pass-01"));
    assert_eq!(row["addr"], json!("box01@proton.example"));
    let records = journaled(scratch.path());
    assert_eq!(records.len(), 3);
    let record = aterm_json::to_string(&records).unwrap();
    assert!(!record.contains("synthetic-pass"), "{record}");
    assert!(records.iter().all(|r| r["surface"] == json!("Terminal")));
}

/// In a person's aterm tab, `--show-passwords` prints no value: it opens the `--edit` grid
/// read-only, every password revealed, in a private folder beside that tab's session, returns
/// nothing to print, and removes the folder when the tab closes. What the file holds when it
/// closes is never read back: the table is as it was. Each reveal is journaled as the grid's.
/// NEGATIVE CONTROL: with `run` printing whatever the gate's place, the tab gets the values:
/// `` printed in the tab: {"complete":… ``.
#[test]
fn in_a_persons_tab_show_passwords_opens_a_read_only_grid_beside_it() {
    let lab = imported(3);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let mut folders = Vec::new();
    let mut open = |opening: &Opening<'_>| -> Result<()> {
        assert_eq!(opening.beside, Some(TAB), "beside the person's tab");
        assert!(opening.read_only, "opened to be read");
        assert_eq!(opening.line, 1);
        // The password column, by the header the grid writes, is named to the window as
        // concealed (`edit … secret=`): no reader of it but the person's eyes gets those cells.
        assert_eq!(opening.secret, ["password".to_string()]);
        let path = opening.file;
        let header = std::fs::read_to_string(path).unwrap();
        let header = header.lines().next().unwrap_or_default();
        assert!(
            header.split('\t').any(|named| named == "password"),
            "the named header is the grid's own: {header}"
        );
        assert!(
            aterm_types::private_edit::is_private(path),
            "{}",
            path.display()
        );
        let shown = path.to_string_lossy();
        assert!(
            !shown.contains("synthetic") && !shown.contains('@'),
            "{shown}"
        );
        folders.push(path.parent().unwrap().to_owned());
        let held = std::fs::read_to_string(path).unwrap();
        for n in 1..=3 {
            assert!(held.contains(&format!("synthetic-pass-0{n}")), "{held}");
        }
        assert!(held.contains("box01@proton.example"), "addresses shown");
        // Whatever the person's buffer did, nothing is read back.
        std::fs::write(path, held.replace("synthetic-pass-01", "typed-in-the-grid")).unwrap();
        Ok(())
    };
    let shown = view(
        paths,
        Noun::Emails,
        &["--show-passwords"],
        &in_tab,
        &mut open,
        b"",
        &journal,
    )
    .unwrap();
    assert_eq!(
        shown,
        Value::Null,
        "printed in the tab: {}",
        aterm_json::to_string(&shown).unwrap()
    );
    assert_eq!(folders.len(), 1);
    assert!(!folders[0].exists(), "the folder goes when the tab closes");
    let records = journaled(scratch.path());
    assert_eq!(records.len(), 3, "{records:?}");
    assert!(records.iter().all(|r| r["surface"] == json!("Grid")));
    assert!(
        !aterm_json::to_string(&records)
            .unwrap()
            .contains("synthetic")
    );
    let printed = view(
        paths,
        Noun::Emails,
        &["--show-passwords"],
        &allowed,
        &mut no_grid,
        b"",
        &journal,
    )
    .unwrap();
    let text = aterm_json::to_string(&printed).unwrap();
    assert!(
        text.contains("synthetic-pass-01") && !text.contains("typed-in-the-grid"),
        "outside aterm it prints, and the read-only grid wrote nothing: {text}"
    );
}

/// In a person's aterm tab, `--edit` opens the editable grid beside that tab's session, never in
/// the terminal; outside aterm, in the terminal. A refused gate opens nothing.
#[test]
fn in_a_persons_tab_the_editable_grid_opens_beside_it() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    for (gate, beside) in [
        (&in_tab as &dyn Fn() -> Result<Place>, Some(TAB)),
        (&allowed, None),
    ] {
        let mut opened = Vec::new();
        let mut open = |opening: &Opening<'_>| -> Result<()> {
            opened.push((opening.beside.map(str::to_owned), opening.read_only));
            assert_eq!(
                opening.secret,
                ["password".to_string()],
                "revealed, editable"
            );
            Ok(())
        };
        let edited = view(
            paths,
            Noun::Emails,
            &["--edit"],
            gate,
            &mut open,
            b"",
            &journal,
        )
        .unwrap();
        assert_eq!(edited["cells"], json!(0), "{edited}");
        assert_eq!(opened, [(beside.map(str::to_owned), false)]);
    }
    assert!(
        view(
            paths,
            Noun::Phones,
            &["--show-numbers"],
            &spawned,
            &mut no_grid,
            b"",
            &journal
        )
        .is_err()
    );
}

/// A grid names to the window, as concealed, each live `Secret` column by the header it writes:
/// the plain name where the column is revealed, `=`-marked where it shows masked and read-only;
/// a table with no `Secret` column names none.
#[test]
fn a_grid_names_its_secret_columns_by_the_headers_it_writes() {
    use crate::alab::registry::{ADDRESS, MACHINE, PHONE};
    assert_eq!(grid::secret_headers(&ADDRESS, true), ["password"]);
    assert_eq!(grid::secret_headers(&ADDRESS, false), ["=password"]);
    assert_eq!(grid::secret_headers(&PHONE, true), ["number"]);
    assert!(grid::secret_headers(&MACHINE, true).is_empty());
}

/// The one line a tab gets names the Editor tab and what it holds, never a value.
#[test]
fn the_tab_gets_one_line_naming_the_editor_tab() {
    let line = opened_line(Noun::Emails, 7);
    assert_eq!(
        line,
        "aterm db emails: the passwords are in the read-only Editor tab beside this one (view 7); \
         close it when done"
    );
    assert!(!line.contains('\n'));
    assert!(opened_line(Noun::Phones, 9).contains("the numbers are in"));
}

/// The masked spelling every surface shows a set `Secret` cell in.
const MASK_TEXT: &str = crate::typed::secret::MASK;

/// `set` writes one cell as the owner: a password only from standard input (a trailing newline
/// from a pipe removed), never on argv, never a flag-shaped value; any other column from argv.
/// It is not session-gated: it shows no value.
/// NEGATIVE CONTROL: with `set` taking a `Secret` value from argv, the password on the command
/// line is written: `` assertion failed: on_argv.is_err() ``.
#[test]
fn set_takes_a_password_only_from_stdin() {
    let lab = imported(2);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let on_argv = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "argv-password-1"],
        &spawned,
        &mut no_grid,
        b"",
        &journal,
    );
    assert!(on_argv.is_err());
    let flag = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "--stdin"],
        &spawned,
        &mut no_grid,
        b"1\n",
        &journal,
    );
    assert!(flag.unwrap_err().to_string().contains("flag"));
    let written = view(
        paths,
        Noun::Emails,
        &["set", "box01@proton.example", "password", "--stdin"],
        &spawned,
        &mut no_grid,
        b"piped-password-new\n",
        &journal,
    )
    .unwrap();
    assert_eq!(written["written"], json!(1), "{written}");
    assert!(!aterm_json::to_string(&written).unwrap().contains("piped"));
    let status = view(
        paths,
        Noun::Emails,
        &["set", "box02@proton.example", "status", "disabled"],
        &spawned,
        &mut no_grid,
        b"",
        &journal,
    )
    .unwrap();
    assert_eq!(status["written"], json!(1), "{status}");
    let shown = view(
        paths,
        Noun::Emails,
        &["--show-passwords"],
        &allowed,
        &mut no_grid,
        b"",
        &journal,
    )
    .unwrap();
    let pk = |address: &str| keys::address_pk(address).unwrap();
    let row = |address: &str| {
        shown["emails"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["ad"] == json!(pk(address)))
            .unwrap()
            .clone()
    };
    assert_eq!(
        row("box01@proton.example")["password"],
        json!("piped-password-new")
    );
    assert_eq!(row("box02@proton.example")["status"], json!("disabled"));
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
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let set = |list: &[&str]| {
        view(
            paths,
            Noun::Emails,
            list,
            &spawned,
            &mut no_grid,
            b"",
            &journal,
        )
    };
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
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    let mut saved: Option<(usize, String)> = None;
    let mut reopened: Option<(usize, String)> = None;
    let mut colliding = |opening: &Opening<'_>| -> Result<()> {
        match saved {
            None => {
                let line = type_addr(opening.file, "box01@proton.example", "box02@proton.example");
                saved = Some((line, std::fs::read_to_string(opening.file).unwrap()));
            }
            Some(_) => {
                reopened = Some((opening.line, std::fs::read_to_string(opening.file).unwrap()));
            }
        }
        Ok(())
    };
    let collided = view(
        paths,
        Noun::Emails,
        &["--edit"],
        &allowed,
        &mut colliding,
        b"",
        &journal,
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
    let mut fresh = |opening: &Opening<'_>| -> Result<()> {
        type_addr(opening.file, "box01@proton.example", "fresh01@example.test");
        Ok(())
    };
    let edited = view(
        paths,
        Noun::Emails,
        &["--edit"],
        &allowed,
        &mut fresh,
        b"",
        &journal,
    )
    .unwrap();
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

/// `--digest` names the `[everyone]` epoch it was keyed under and the rows with a password, is
/// stable, moves when a password does, and refuses where no `[everyone]` key is held; it prints
/// no value.
/// NEGATIVE CONTROL: with `digest` keyed by a constant in place of the epoch key, two epochs'
/// digests agree: `` assertion `left != right` failed ``.
#[test]
fn the_emails_digest_names_its_epoch_and_moves_with_a_password() {
    use crate::alab::command::tests::EVERYONE;
    let lab = imported(2);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let go = |list: &[&str], stdin: &[u8]| {
        view(
            paths,
            Noun::Emails,
            list,
            &spawned,
            &mut no_grid,
            stdin,
            &journal,
        )
    };
    EVERYONE.with(|held| held.set(None));
    assert!(go(&["--digest"], b"").is_err(), "no [everyone] key held");
    EVERYONE.with(|held| held.set(Some(([0x5e; 32], [0x33; 32]))));
    let first = go(&["--digest"], b"").unwrap();
    assert_eq!(first, go(&["--digest"], b"").unwrap());
    assert_eq!(first["epoch"], json!("5e5e5e5e"), "{first}");
    assert_eq!(first["with_password"], json!(2), "{first}");
    assert!(!aterm_json::to_string(&first).unwrap().contains("synthetic"));
    EVERYONE.with(|held| held.set(Some(([0x6f; 32], [0x44; 32]))));
    assert_ne!(go(&["--digest"], b"").unwrap()["digest"], first["digest"]);
    EVERYONE.with(|held| held.set(Some(([0x5e; 32], [0x33; 32]))));
    go(
        &["set", "box02@proton.example", "password", "--stdin"],
        b"another-password\n",
    )
    .unwrap();
    let moved = go(&["--digest"], b"").unwrap();
    assert_eq!(moved["epoch"], first["epoch"]);
    assert_ne!(moved["digest"], first["digest"]);
    EVERYONE.with(|held| held.set(None));
}

/// The grid round trip on a `Secret` column: `--edit` opens the emails grid with the password
/// column revealed and editable in a private folder (`*.private.noindex`, which amacs keeps no
/// draft of), whose path carries no value; the owner's changed password is sealed and written,
/// an unchanged one is not, a flag-shaped one reopens the file with nothing written, and the
/// folder is gone once the session ends. A session that may not reveal never opens the grid.
/// NEGATIVE CONTROL: with `export_opened` ignoring `opened` (the column masked and read-only),
/// the grid's password column is read-only: `` […, "=password", "=pk"]: editable, not
/// `=password` ``.
#[test]
fn the_grid_round_trips_a_password_column_in_a_private_folder() {
    let lab = imported(2);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let refused = view(
        paths,
        Noun::Emails,
        &["--edit"],
        &spawned,
        &mut no_grid,
        b"",
        &journal,
    );
    assert!(refused.is_err());
    let mut folders = Vec::new();
    let mut rounds = 0;
    let mut open = |opening: &Opening<'_>| -> Result<()> {
        let path = opening.file;
        assert_eq!(opening.beside, None, "outside aterm, in this terminal");
        assert!(!opening.read_only);
        rounds += 1;
        assert!(
            aterm_types::private_edit::is_private(path),
            "{}",
            path.display()
        );
        let shown = path.to_string_lossy();
        assert!(
            !shown.contains("synthetic") && !shown.contains('@'),
            "{shown}"
        );
        folders.push(path.parent().unwrap().to_owned());
        let held = std::fs::read_to_string(path).unwrap();
        let mut lines: Vec<String> = held.lines().map(str::to_owned).collect();
        let header: Vec<&str> = lines[0].split('\t').collect();
        assert!(
            header.contains(&"password"),
            "{header:?}: editable, not `=password`"
        );
        let at = header.iter().position(|h| *h == "password").unwrap();
        let row = lines
            .iter()
            .position(|l| l.contains("box01@proton.example"))
            .unwrap();
        if rounds == 1 {
            assert!(held.contains("synthetic-pass-01"));
        }
        let mut cells: Vec<String> = lines[row].split('\t').map(str::to_owned).collect();
        cells[at] = if rounds == 1 {
            "1".to_owned()
        } else {
            "grid-password-01".to_owned()
        };
        lines[row] = cells.join("\t");
        std::fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
        Ok(())
    };
    let edited = view(
        paths,
        Noun::Emails,
        &["--edit"],
        &allowed,
        &mut open,
        b"",
        &journal,
    )
    .unwrap();
    assert_eq!(rounds, 2, "the flag-shaped save reopened: {edited}");
    assert_eq!(edited["cells"], json!(1), "{edited}");
    let text = aterm_json::to_string(&edited).unwrap();
    assert!(
        !text.contains("grid-password") && !text.contains("synthetic"),
        "{text}"
    );
    for folder in &folders {
        assert!(
            !folder.exists(),
            "the grid's folder is gone: {}",
            folder.display()
        );
    }
    assert!(
        journaled(scratch.path())
            .iter()
            .all(|r| r["surface"] == json!("Grid"))
    );
    let shown = view(
        paths,
        Noun::Emails,
        &["--show-passwords"],
        &allowed,
        &mut no_grid,
        b"",
        &journal,
    )
    .unwrap();
    let password = |address: &str| {
        let pk = keys::address_pk(address).unwrap();
        shown["emails"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["ad"] == json!(pk))
            .unwrap()["password"]
            .clone()
    };
    assert_eq!(password("box01@proton.example"), json!("grid-password-01"));
    assert_eq!(password("box02@proton.example"), json!("synthetic-pass-02"));
}

/// A grid folder a killed session left (its maker's pid no longer runs) is swept when the next
/// grid opens; one whose maker runs is kept.
#[test]
fn a_killed_sessions_grid_folder_is_swept_at_the_next_edit() {
    let lab = imported(1);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let mut planted = Vec::new();
    let mut open = |opening: &Opening<'_>| -> Result<()> {
        let parent = opening.file.parent().unwrap().parent().unwrap();
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
    view(
        paths,
        Noun::Emails,
        &["--edit"],
        &allowed,
        &mut open,
        b"",
        &journal,
    )
    .unwrap();
    view(
        paths,
        Noun::Emails,
        &["--edit"],
        &allowed,
        &mut open,
        b"",
        &journal,
    )
    .unwrap();
    assert!(!planted[0].exists(), "the dead maker's folder is swept");
    assert!(planted[1].exists(), "a running maker's folder is kept");
    std::fs::remove_dir_all(&planted[1]).unwrap();
}

/// The four nouns serve from the registry once any import filled it, or where no fleet_db
/// replica is; before that, a Mac holding a replica keeps fleet_db's views.
#[test]
fn the_registry_takes_the_nouns_once_imported_or_where_no_replica_is() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    assert!(serves(paths), "no replica here");
    let dir = replica_fixture(paths, 1, "active");
    assert!(
        !serves(paths),
        "a replica and an empty registry: fleet_db keeps them"
    );
    // A record of this Mac's own import keeps the nouns with the registry, whatever it reads.
    let record = crate::alab::import::covered::Record {
        covered: false,
        uncovered: 1,
        digest: None,
        epoch: None,
        at_ms: NOW,
        sources: std::collections::BTreeSet::new(),
    };
    crate::alab::import::covered::write(&dir, &record, &[]).unwrap();
    assert!(serves(paths), "this Mac's import ran");
    std::fs::remove_file(dir.join(crate::alab::import::covered::FILE)).unwrap();
    assert!(
        !serves(paths),
        "the negative control: no record, an empty registry"
    );
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    super::super::run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert!(serves(paths));
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

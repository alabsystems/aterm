// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab` on real Commons labs: isolated scratch homes, no live vault, fleet or
//! Commons, synthetic sheets with the real layouts. The vendor reading is the stand-in of
//! `import::tests`; the review opener records what it was asked to open and what each file held
//! while open (the folder goes once the review ends), so no window opens.

use super::*;
use crate::{
    alab::{
        Registry,
        import::tests::{LOGINS_HEADER, Stand, VAULT_HEADER},
        registry::{ACCOUNT, ADDRESS, MACHINE, MAILBOX, SEAT},
    },
    table::tests::Lab,
    typed::{
        grid::tests::{Book, put},
        keys,
    },
};
use aterm_secrets::commons::roster::Role;
use std::os::unix::fs::PermissionsExt as _;

const NOW: u64 = 1_800_000_000_000;

/// Write the two fixture sheets into `dir`.
fn sheets(dir: &Path) -> (PathBuf, PathBuf) {
    let vault = dir.join("ai-accounts.csv");
    let logins = dir.join("ai-account-logins.csv");
    std::fs::write(
        &vault,
        format!(
            "{VAULT_HEADER}\n\
             ,ann@example.invalid,m3,,1,browser,,,,,,,\n\
             ,bob@example.invalid,m4.2,m3,1,,1,,global,,kimi banned,,\n\
             ,ann@example.invalid,m5,,1,,,,,,,,\n\
             ,not an address,m6,,1,,,,,,,,\n"
        ),
    )
    .unwrap();
    std::fs::write(
        &logins,
        format!(
            "{LOGINS_HEADER}\n\
             m3,ann@example.invalid,claude + chatgpt/codex,box@proton.example,lead's reasoning,\n\
             m9,cy@example.invalid,claude,box@proton.example,lead's reasoning,\n"
        ),
    )
    .unwrap();
    (vault, logins)
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|w| (*w).to_owned()).collect()
}

/// One opened review file: its path, its text while open, and its folder's mode.
type Opened = (PathBuf, String, u32);

fn import(
    paths: &Paths,
    extra: &[&str],
    vault: &Path,
    logins: &Path,
    opened: &mut Vec<Opened>,
) -> Value {
    let mut list = vec![
        "import",
        "--vault-sheet",
        vault.to_str().unwrap(),
        "--logins",
        logins.to_str().unwrap(),
    ];
    list.extend_from_slice(extra);
    let mut open = |file: &Path| {
        let mode = std::fs::metadata(file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        opened.push((
            file.to_path_buf(),
            std::fs::read_to_string(file).unwrap(),
            mode,
        ));
        Ok(())
    };
    run(paths, &words(&list), Some(&Stand), &mut open, NOW).unwrap()
}

/// `import --apply` writes the sheets once: the accounts, addresses, mailbox, machines and seats
/// are rows, the mailbox key is kept privately on this Mac (no address in it), and a second
/// `--apply` of the same sheets writes no record.
/// NEGATIVE CONTROLS: with `cell::import_step` never answering `Unchanged` (`if false &&
/// candidate.value.x == *new`), the rerun rewrites every import-held cell: `` left: Some(2)
/// right: Some(0) `` at the second run's `records`; with `import` never saving the mailbox keys
/// (`if false { save_keys(..)?; }`), no key file is left for the next run: `` called
/// `Result::unwrap()` on an `Err` value: Os { code: 2, kind: NotFound, message: "No such file
/// or directory" } `` (reading it back).
#[test]
fn import_applies_once_then_a_rerun_writes_nothing() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    let first = import(paths, &["--apply", "--first"], &vault, &logins, &mut opened);
    assert_eq!(first["dry_run"], json!(false));
    assert!(first["records"].as_u64().unwrap() > 0, "{first}");
    assert!(opened.is_empty(), "--apply opens no review");
    let view = Registry::at(paths.clone()).view().unwrap();
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    assert!(view.project(&ADDRESS).rows.contains_key(&ann));
    let claude = keys::account_pk("claude", "ann@example.invalid").unwrap();
    assert_eq!(
        view.project(&ACCOUNT).rows[&claude].get("status"),
        Some(&json!("pending"))
    );
    assert_eq!(view.project(&MAILBOX).rows.len(), 1);
    assert!(!view.project(&MACHINE).rows.is_empty());
    assert!(!view.project(&SEAT).rows.is_empty());
    let file = paths.dir.join(MAILBOX_KEYS);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains('@'));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let second = import(paths, &["--apply"], &vault, &logins, &mut opened);
    assert_eq!(second["records"].as_u64(), Some(0), "{second}");
    let dry = import(paths, &["--dry-run"], &vault, &logins, &mut opened);
    assert!(opened.is_empty(), "a dry run opens no review unless asked");
    assert_eq!(dry["records"].as_u64(), Some(0), "{dry}");
    assert_eq!(dry["registry"], json!("held"));
    assert!(view.quota.over.is_empty());
}

/// `import --apply` writes the compiled providers, CLIs and plans first, in records of their
/// own, so no imported account's `provider` dangles in `check`; a re-run seeds nothing more.
/// NEGATIVE CONTROL: with `import` never seeding (`let seeded =
/// Err::<crate::alab::Seeded, _>(Error::new("off"));`), every imported account's provider
/// dangles: `` [Dangling { collection: "account", pk: "ac-1cadb4856cd5e220", column:
/// "provider", target: "provider" }, … `` (the assertion's message).
#[test]
fn an_import_seeds_the_providers_its_accounts_name() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    let dry = import(paths, &[], &vault, &logins, &mut opened);
    assert!(dry["seeds"].as_u64().unwrap() >= 1, "{dry}");
    let first = import(paths, &["--apply", "--first"], &vault, &logins, &mut opened);
    let view = Registry::at(paths.clone()).view().unwrap();
    let (_, generic) = checks::lab(&view.cells);
    let dangling: Vec<_> = generic
        .iter()
        .filter(|f| matches!(f, check::Finding::Dangling { .. }))
        .collect();
    assert!(dangling.is_empty(), "{dangling:?}");
    assert_eq!(
        view.project(&crate::alab::registry::PROVIDER).rows.len(),
        keys::PROVIDERS.len()
    );
    assert!(first["seeds"].as_u64().unwrap() >= 1, "{first}");
    let again = import(paths, &["--apply"], &vault, &logins, &mut opened);
    assert_eq!(again["seeds"].as_u64(), Some(0), "{again}");
}

/// `import --apply` waits until this Mac holds the registry: on a Mac that holds none it writes
/// nothing unless `--first` says no Mac in the lab holds it yet; and a Mac that holds the
/// registry's mailboxes but not their keys (they stay on the Mac that imported first) refuses
/// rather than mint duplicate mailboxes.
/// NEGATIVE CONTROLS: with both empty-registry checks removed from `import` (`false &&`), the
/// unsynced Mac writes: `` assertion failed: refused.is_err() ``; with the mailbox-key gate
/// removed (`if false && plan.keys.len() > keys.len()`), the second Mac writes with mailbox
/// keys of its own: `` assertion failed: again.is_err() ``.
#[test]
fn import_apply_waits_until_this_mac_holds_the_registry() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let words_for = |extra: &[&str]| {
        let mut list = vec![
            "import",
            "--vault-sheet",
            vault.to_str().unwrap(),
            "--logins",
            logins.to_str().unwrap(),
        ];
        list.extend_from_slice(extra);
        words(&list)
    };
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let refused = run(
        &lab.paths[0],
        &words_for(&["--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    );
    assert!(refused.is_err());
    assert!(
        Registry::at(lab.paths[0].clone()).view().is_err(),
        "a refused import does not even declare the registry"
    );
    run(
        &lab.paths[0],
        &words_for(&["--apply", "--first"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    // The second Mac holds the registry after a repair round, but not the mailbox keys.
    Registry::at(lab.paths[1].clone()).open().unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    let second = Registry::at(lab.paths[1].clone()).view().unwrap();
    assert_eq!(second.project(&MAILBOX).rows.len(), 1);
    let again = run(
        &lab.paths[1],
        &words_for(&["--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    );
    assert!(again.is_err());
    let after = Registry::at(lab.paths[1].clone()).view().unwrap();
    assert_eq!(after.project(&MAILBOX).rows.len(), 1);
    assert!(!lab.paths[1].dir.join(MAILBOX_KEYS).exists());
}

/// No report shows an address: the dry run's summary and its finding list carry counts, kinds,
/// keys and coordinates only; the review (`--review`) masks addresses unless `--show-addresses`,
/// and it is the one place a note's text appears. A Mac that does not hold the registry yet plans
/// against an empty one.
/// NEGATIVE CONTROL: with `review_text` ignoring `reveal` (always the flat text), the masked
/// review shows the address: `` assertion failed: !findings.contains("ann@example.invalid") ``.
#[test]
fn no_import_report_shows_an_address() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    let report = import(
        paths,
        &["--findings", "--review"],
        &vault,
        &logins,
        &mut opened,
    );
    assert_eq!(report["registry"], json!("not held here"));
    assert_eq!(report["dry_run"], json!(true));
    let text = aterm_json::to_string(&report).unwrap();
    assert!(!text.contains('@'), "{text}");
    assert!(report["findings"]["duplicate-address"].as_u64().unwrap() >= 1);
    assert!(report["findings"]["not-an-address"].as_u64().unwrap() >= 1);
    let names: Vec<&str> = opened
        .iter()
        .map(|(file, _, _)| file.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, ["findings.tsv", "planned.tsv"]);
    assert!(opened.iter().all(|(_, _, mode)| *mode == 0o700));
    let findings = &opened[0].1;
    let planned = &opened[1].1;
    assert!(!findings.contains("ann@example.invalid"));
    assert!(!planned.contains("ann@example.invalid"));
    assert!(findings.contains("an…@…"));
    assert!(
        findings.contains("kimi banned"),
        "the local review shows the note"
    );
    opened.clear();
    import(
        paths,
        &["--review", "--show-addresses"],
        &vault,
        &logins,
        &mut opened,
    );
    assert!(opened[0].1.contains("ann@example.invalid"));
}

/// The review never shows a secret, not even locally: a credential-shaped cell and a note that
/// names a secret read as withheld in `findings.tsv`, while an ordinary note still shows; and
/// the review folder is gone once the review ends, so no note's text outlives it.
/// NEGATIVE CONTROL: with `review_text` skipping its `guard::withheld` check and the
/// `CredentialShaped` arm removed from `write_review`, the token reaches the review: ``
/// assertion failed: !findings.contains(TOKEN) ``; with `import` keeping the review folder
/// (`write_review(..)?.keep()`), it outlives the review: `` assertion failed: !folder.exists()
/// ``.
#[test]
fn a_review_never_shows_a_secret_and_goes_with_its_session() {
    // In halves, so the export guard never reads a token here.
    const TOKEN: &str = concat!("ghp", "_bbbbbbbbbbbbbbbbbbbbbbbb");
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    std::fs::write(
        &vault,
        format!(
            "{VAULT_HEADER}\n\
             ,ann@example.invalid,m3,,1,,,,,,{TOKEN},,\n\
             ,bob@example.invalid,m4,,1,,,,,,banned pw: hunter2,,\n\
             ,cy@example.invalid,m5,,,,,,global,,kimi banned,,\n"
        ),
    )
    .unwrap();
    let mut opened = Vec::new();
    let report = import(paths, &["--review"], &vault, &logins, &mut opened);
    assert_eq!(report["review"], json!(true));
    assert!(report["findings"]["credential-shaped"].as_u64().unwrap() >= 1);
    let findings = &opened[0].1;
    assert!(!findings.contains(TOKEN));
    assert!(!findings.contains("hunter2"));
    assert!(findings.contains(WITHHELD));
    assert!(findings.contains("kimi banned"));
    let folder = opened[0].0.parent().unwrap();
    assert!(!folder.exists());
}

/// The importer needs the harness's reading: without it (`aterm db` reached outside the `aterm`
/// executable) `import` refuses, even a dry run of readable sheets, and opens no review.
/// NEGATIVE CONTROL: with `run` falling back to the test's own reading (`vendors.unwrap_or(
/// &crate::alab::import::tests::Stand)`) instead of refusing, the dry run plans the sheets:
/// `` assertion failed: refused.is_err() ``.
#[test]
fn import_without_the_harness_reading_refuses() {
    let lab = Lab::new(&[Role::Writer]);
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut open = |_: &Path| -> Result<()> { panic!("no review") };
    let list = [
        "import",
        "--vault-sheet",
        vault.to_str().unwrap(),
        "--logins",
        logins.to_str().unwrap(),
        "--dry-run",
    ];
    let refused = run(&lab.paths[0], &words(&list), None, &mut open, NOW);
    assert!(refused.is_err());
}

/// `check` reports the lab's rules over converged rows: two home seats for one address, more
/// than three seats, a seat on a retired machine, a live account whose login is retired, and a
/// label that is another live machine's alias, each by key, never by value.
/// NEGATIVE CONTROL: with the home-seat rule never firing in `checks::lab` (`if false &&
/// homes.len() > 1`), the two homes go unreported: `` two-home-seats: ["account-login-retired",
/// "seat-on-retired-machine", "too-many-seats"] `` (the assertion's message).
#[test]
fn check_reports_the_lab_rules() {
    let m3 = keys::machine_pk("m3");
    let m4 = keys::machine_pk("m4");
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    let account = keys::account_pk("claude", "bob@example.invalid").unwrap();
    let mut changes = vec![
        put(&MACHINE, &m3, "label", json!("m3")),
        put(&MACHINE, &m3, "status", json!("active")),
        put(&MACHINE, &m3, "row", json!("live")),
        put(&MACHINE, &m4, "label", json!("m4")),
        put(&MACHINE, &m4, "status", json!("active")),
        put(&MACHINE, &m4, "row", json!("retired")),
        crate::typed::grid::tests::elem(&MACHINE, &m3, "aliases", "m4"),
        put(&ADDRESS, &ann, "addr", json!("ann@example.invalid")),
        put(&ADDRESS, &ann, "row", json!("live")),
        put(&ADDRESS, &bob, "addr", json!("bob@example.invalid")),
        put(&ADDRESS, &bob, "row", json!("retired")),
        put(&ACCOUNT, &account, "login", json!(bob)),
        put(&ACCOUNT, &account, "row", json!("live")),
    ];
    for (machine, slot, role) in [
        (&m3, 1, "home"),
        (&m3, 2, "home"),
        (&m3, 3, "warm"),
        (&m4, 1, "warm"),
    ] {
        let seat = keys::seat_pk(machine, slot).unwrap();
        changes.push(put(&SEAT, &seat, "address", json!(ann)));
        changes.push(put(&SEAT, &seat, "role", json!(role)));
        changes.push(put(&SEAT, &seat, "row", json!("live")));
    }
    let mut book = Book::new();
    book.write(1, &[], changes);
    let (lab, _) = checks::lab(&book.cells());
    let kinds: Vec<&str> = lab.iter().map(|f| f.kind).collect();
    for kind in [
        "two-home-seats",
        "too-many-seats",
        "seat-on-retired-machine",
        "account-login-retired",
    ] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
    assert!(!kinds.contains(&"label-ambiguous"), "m4 is retired");
    let text = format!("{lab:?}");
    assert!(!text.contains('@'));
}

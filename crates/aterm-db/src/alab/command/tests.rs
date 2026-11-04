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
        registry::{ACCOUNT, ADDRESS, MACHINE, MAILBOX, PREFERENCE},
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

/// `import --apply` writes the sheets once: the accounts, addresses, mailbox, machines and
/// preferences
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
    assert!(!view.project(&PREFERENCE).rows.is_empty());
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
        .filter(|f| matches!(f, crate::typed::check::Finding::Dangling { .. }))
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
    // Even if the second Mac already holds an un-landed local mailbox key (e.g. from an earlier
    // interrupted run), importing against a registry holding another Mac's mailbox still refuses.
    let stale_plan = import::plan(
        &Input {
            sheets: &[Sheet::parse(
                SheetKind::Logins,
                "ai-account-logins.csv",
                &std::fs::read(&logins).unwrap(),
            )
            .unwrap()],
            macs: &[],
            vendors: &Stand,
            keys: MailboxKeys::default(),
            now_ms: NOW,
            births: true,
            unread: &[],
        },
        &Default::default(),
    )
    .unwrap();
    aterm_uds::private_file::write_private(
        &lab.paths[1].dir,
        MAILBOX_KEYS,
        &aterm_json::to_vec(&stale_plan.keys.to_json()).unwrap(),
    )
    .unwrap();
    let stale_again = run(
        &lab.paths[1],
        &words_for(&["--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    );
    assert!(stale_again.is_err());
    assert_eq!(
        Registry::at(lab.paths[1].clone())
            .view()
            .unwrap()
            .project(&MAILBOX)
            .rows
            .len(),
        1
    );
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

/// `check` reports the lab's rules over converged rows: a preference on a retired machine or for
/// a retired account, a live account whose login is retired, an alias link to a retired address or
/// round a cycle, a preference whose cells say another pair than its key, each by key, never by
/// value. A mapping has no cap: one account preferred on four
/// machines and at one rank twice is no finding.
/// NEGATIVE CONTROL: with the retired-machine rule never firing in `checks::lab` (`if false &&`
/// on its `if let`), the row goes unreported: this test's `` preference-on-retired-machine: `` message.
#[test]
fn check_reports_the_lab_rules() {
    let machines: Vec<String> = (3..=7)
        .map(|n| keys::machine_pk(&format!("m{n}")))
        .collect();
    let m4 = &machines[1];
    let ann = keys::address_pk("ann@example.invalid").unwrap();
    let bob = keys::address_pk("bob@example.invalid").unwrap();
    let cy = keys::address_pk("cy@example.invalid").unwrap();
    let dee = keys::address_pk("dee@example.invalid").unwrap();
    let eve = keys::address_pk("eve@example.invalid").unwrap();
    let live_account = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let account = keys::account_pk("claude", "bob@example.invalid").unwrap();
    let retired_box = "mb-0000000000000001";
    let mut changes = vec![
        put(&MAILBOX, retired_box, "row", json!("retired")),
        put(&ADDRESS, &ann, "addr", json!("ann@example.invalid")),
        put(&ADDRESS, &ann, "mailbox", json!(retired_box)),
        put(&ADDRESS, &ann, "row", json!("live")),
        put(&ADDRESS, &bob, "addr", json!("bob@example.invalid")),
        put(&ADDRESS, &bob, "row", json!("retired")),
        put(&ADDRESS, &cy, "alias_of", json!(dee)),
        put(&ADDRESS, &cy, "mailbox_hint", json!(retired_box)),
        put(&ADDRESS, &cy, "row", json!("live")),
        put(&ADDRESS, &dee, "alias_of", json!(cy)),
        put(&ADDRESS, &dee, "row", json!("live")),
        put(&ADDRESS, &eve, "alias_of", json!(bob)),
        put(&ADDRESS, &eve, "row", json!("live")),
        put(&ACCOUNT, &account, "login", json!(bob)),
        put(&ACCOUNT, &account, "row", json!("retired")),
        put(&ACCOUNT, &live_account, "login", json!(bob)),
        put(&ACCOUNT, &live_account, "identity", json!(bob)),
        put(&ACCOUNT, &live_account, "row", json!("live")),
    ];
    for (n, machine) in machines.iter().enumerate() {
        changes.push(put(
            &MACHINE,
            machine,
            "label",
            json!(format!("m{}", n + 3)),
        ));
        changes.push(put(&MACHINE, machine, "status", json!("active")));
        let word = if machine == m4 { "retired" } else { "live" };
        changes.push(put(&MACHINE, machine, "row", json!(word)));
        let preference = keys::preference_pk(machine, &live_account).unwrap();
        changes.push(put(&PREFERENCE, &preference, "machine", json!(machine)));
        changes.push(put(
            &PREFERENCE,
            &preference,
            "account",
            json!(live_account),
        ));
        changes.push(put(&PREFERENCE, &preference, "rank", json!(1)));
        changes.push(put(&PREFERENCE, &preference, "row", json!("live")));
    }
    // A row whose cells say another pair than its key: machines[3] under machines[2]'s key.
    let crossed = keys::preference_pk(&machines[2], &live_account).unwrap();
    changes.push(put(&PREFERENCE, &crossed, "machine", json!(machines[3])));
    let retired = keys::preference_pk(&machines[0], &account).unwrap();
    changes.push(put(&PREFERENCE, &retired, "machine", json!(machines[0])));
    changes.push(put(&PREFERENCE, &retired, "account", json!(account)));
    changes.push(put(&PREFERENCE, &retired, "row", json!("live")));
    let mut book = Book::new();
    book.write(1, &[], changes);
    let (lab, _) = checks::lab(&book.cells());
    let kinds: Vec<&str> = lab.iter().map(|f| f.kind).collect();
    for kind in [
        "preference-on-retired-machine",
        "preference-key-mismatch",
        "preference-for-retired-account",
        "account-login-retired",
        "account-identity-retired",
        "address-mailbox-retired",
        "address-mailbox-hint-retired",
        "alias-of-retired",
        "alias-cycle",
    ] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "preference-on-retired-machine")
            .count(),
        1,
        "only the row on the retired machine: {kinds:?}"
    );
    let text = format!("{lab:?}");
    assert!(!text.contains('@'));
}

/// `prefer` writes the owner's mapping and `unprefer` ends it: by label or legacy label, never by
/// address. A repeat writes nothing, and a re-prefer brings the same row back. Nothing but the
/// preference row is written: the account, its status and its login are as they were.
/// NEGATIVE CONTROL: with `prefer` not skipping a value already held (`if false && current...`),
/// the repeat rewrites the cells: `` left: Number(4) right: Number(0) `` at the repeat's `written`.
#[test]
fn prefer_and_unprefer_write_one_mapping() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    import(paths, &["--apply", "--first"], &vault, &logins, &mut opened);
    let claude = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let label = keys::default_account_label("claude", &claude).unwrap();
    let before = Registry::at(paths.clone())
        .view()
        .unwrap()
        .project(&ACCOUNT);
    let mut no_review = |_: &Path| Ok(());
    let mut verb = |list: &[&str]| run(paths, &words(list), None, &mut no_review, NOW);
    assert!(verb(&["prefer", "m4", "ann@example.invalid"]).is_err());
    assert!(verb(&["prefer", "m404", &label]).is_err());
    // A row the import wrote becomes the owner's on the owner's word.
    let m3 = verb(&["prefer", "m3", &label]).unwrap();
    let imported = m3["preference"].as_str().unwrap().to_owned();
    assert_eq!(m3["written"], json!(1));
    let first = verb(&["prefer", "m4", &label, "--rank", "2"]).unwrap();
    assert!(first["written"].as_u64().unwrap() > 0, "{first}");
    let pk = first["preference"].as_str().unwrap().to_owned();
    let read = || {
        Registry::at(paths.clone())
            .view()
            .unwrap()
            .project(&PREFERENCE)
    };
    assert_eq!(read().rows[&pk].get("rank"), Some(&json!(2)));
    assert_eq!(read().rows[&pk].get("why"), Some(&json!("owner")));
    assert_eq!(read().rows[&imported].get("why"), Some(&json!("owner")));
    let again = verb(&["prefer", "m4", &label, "--rank", "2"]).unwrap();
    assert_eq!(again["written"], json!(0));
    verb(&["unprefer", "m4", &label]).unwrap();
    assert_eq!(read().rows[&pk].lifecycle(), Some("ended"));
    assert_eq!(
        verb(&["unprefer", "m4", &label]).unwrap()["written"],
        json!(0)
    );
    verb(&["prefer", "m4", &label]).unwrap();
    assert!(read().rows[&pk].live());
    assert_eq!(
        Registry::at(paths.clone())
            .view()
            .unwrap()
            .project(&ACCOUNT),
        before
    );
    let text = aterm_json::to_string(&first).unwrap();
    assert!(!text.contains('@'), "{text}");
}

/// The harness's files are read through `Vendors::stated`: named ones refuse when unreadable,
/// ones found beside the vault sheet are reported in `unread` and retract nothing, and the warm
/// list becomes one preference per machine.
/// NEGATIVE CONTROL: with `harness_sheets` skipping the files it found (`.filter(|_| false)`), the
/// scratch home's files are not read and there is nothing to import: `` Error("no sheet to import:
/// give --vault-sheet or --logins, or put the sheets in their places") ``.
#[test]
fn harness_files_are_read_and_a_bad_one_is_reported() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let secrets = paths.home.join(".secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(
        secrets.join("ai-pool.toml"),
        "claude,ann@example.invalid,,m8 m9\n",
    )
    .unwrap();
    std::fs::write(secrets.join("fleet-accounts.toml"), "not rows\n").unwrap();
    let mut open = |_: &Path| Ok(());
    let list = words(&["import", "--apply", "--first"]);
    let report = run(paths, &list, Some(&Stand), &mut open, NOW).unwrap();
    assert_eq!(report["unread"], json!(["fleet-accounts.toml"]));
    let view = Registry::at(paths.clone()).view().unwrap();
    assert_eq!(view.project(&PREFERENCE).rows.len(), 2);
    // The pool drops m9 while the fleet file still fails: what the failed file may state stands.
    std::fs::write(
        secrets.join("ai-pool.toml"),
        "claude,ann@example.invalid,,m8\n",
    )
    .unwrap();
    let again = words(&["import", "--apply"]);
    run(paths, &again, Some(&Stand), &mut open, NOW).unwrap();
    let view = Registry::at(paths.clone()).view().unwrap();
    assert!(
        view.project(&PREFERENCE)
            .rows
            .values()
            .all(|row| row.live()),
        "a run with an unread file ended a preference"
    );
    let named = words(&[
        "import",
        "--fleet-accounts",
        secrets.join("fleet-accounts.toml").to_str().unwrap(),
    ]);
    let refused = run(paths, &named, Some(&Stand), &mut open, NOW).unwrap_err();
    assert!(!refused.to_string().contains("not rows"), "{refused}");
}
/// `prefer` refuses a retired machine or account even when named by its primary key, while
/// `unprefer` ends a lingering preference on a retired machine or account by label or key.
/// NEGATIVE CONTROL: with `named_row` accepting any present `pk` without checking `require_live`
/// and filtering labels to `row.live()` unconditionally, `prefer` on a retired `pk` succeeds and
/// `unprefer` by label on a retired machine fails.
#[test]
fn prefer_refuses_retired_keys_while_unprefer_ends_preferences_on_retired_labels() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    import(paths, &["--apply", "--first"], &vault, &logins, &mut opened);
    let claude = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let label = keys::default_account_label("claude", &claude).unwrap();
    let m4 = keys::machine_pk("m4");
    let mut no_review = |_: &Path| Ok(());
    let mut verb = |list: &[&str]| run(paths, &words(list), None, &mut no_review, NOW);

    let pref = verb(&["prefer", "m4", &label]).unwrap()["preference"]
        .as_str()
        .unwrap()
        .to_owned();
    Registry::at(paths.clone())
        .write(vec![vec![put(&MACHINE, &m4, "row", json!("retired"))]])
        .unwrap();

    // `prefer` by retired machine `pk` is refused instead of creating `preference-on-retired-machine`.
    assert!(verb(&["prefer", &m4, &label]).is_err());

    // `unprefer` by retired machine label still ends the lingering preference.
    let ended = verb(&["unprefer", "m4", &label]).unwrap();
    assert_eq!(ended["row"], json!("ended"));
    assert_eq!(
        Registry::at(paths.clone())
            .view()
            .unwrap()
            .project(&PREFERENCE)
            .rows[&pref]
            .lifecycle(),
        Some("ended")
    );

    // Retiring the account also blocks `prefer` by account `pk`.
    Registry::at(paths.clone())
        .write(vec![vec![put(&ACCOUNT, &claude, "row", json!("retired"))]])
        .unwrap();
    assert!(verb(&["prefer", "m3", &claude]).is_err());
}

/// `prefer` refuses a retired machine or account even when named by key, while `unprefer` still
/// ends a lingering preference on a retired row; `import` refuses `--apply --review`, duplicate
/// single-value flags, and world-readable mailbox key files; and `--rank` requires canonical
/// digits.
/// NEGATIVE CONTROL: with `named_row` returning any key present in `rows` without checking
/// `require_live` (`if rows.rows.contains_key(name) { return Ok(..); }`), `prefer` on a retired
/// machine key succeeds: `` assertion failed: verb(&["prefer", &m3_pk, &claude]).is_err() ``.
#[test]
fn retired_keys_cannot_gain_new_preferences_and_strict_flags_or_open_keys_refuse() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    import(paths, &["--apply", "--first"], &vault, &logins, &mut opened);
    let claude = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let m3_pk = keys::machine_pk("m3");
    let mut no_review = |_: &Path| Ok(());
    let mut verb = |list: &[&str]| run(paths, &words(list), Some(&Stand), &mut no_review, NOW);
    for bad in [
        &["import", "--apply", "--review"][..],
        &["import", "--vault-sheet", "a.csv", "--vault-sheet", "b.csv"],
        &["import", "--logins", "a.csv", "--logins", "b.csv"],
        &["import", "--ai-pool", "a.toml", "--ai-pool", "b.toml"],
        &["prefer", "m3", &claude, "--rank", "02"],
        &["prefer", "m3", &claude, "--rank", "+2"],
        &["prefer", "m3", &claude, "--rank", "1", "--rank", "2"],
    ] {
        assert!(verb(bad).is_err(), "{bad:?}");
    }
    // Retire m3 in the registry: `prefer` by key refuses, while `unprefer` by key or label ends
    // the existing preference.
    Handle::at(paths.clone(), &att::REGISTRY)
        .commit_derived(false, |_| {
            Ok((
                (),
                vec![vec![
                    cell::change(
                        &MACHINE,
                        CellKey {
                            pk: &m3_pk,
                            col: "row",
                            elem: None,
                        },
                        &CellValue::owner(MACHINE.version, json!("retired")),
                    )
                    .unwrap(),
                ]],
            ))
        })
        .unwrap();
    assert!(verb(&["prefer", &m3_pk, &claude]).is_err());
    let ended = verb(&["unprefer", "m3", &claude]).unwrap();
    assert_eq!(ended["row"], json!("ended"));
    // A world-readable mailbox key file is refused by `read_private`.
    let key_file = paths.dir.join(MAILBOX_KEYS);
    std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        verb(&[
            "import",
            "--vault-sheet",
            vault.to_str().unwrap(),
            "--logins",
            logins.to_str().unwrap(),
        ])
        .is_err()
    );
}

/// Denied swarm-roster members are excluded from `known_macs`, so neither `import` nor `adopt`
/// births or adopts a retired Mac's roster entry.
/// NEGATIVE CONTROL: with `known_macs` omitting `.filter(|member| !roster.is_member_denied(member))`,
/// `report["known_macs"]` is `2` and `m2` is born active in `MACHINE`.
#[test]
fn denied_roster_macs_are_excluded_from_import_and_adopt() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let paths = &lab.paths[0];
    let mut roster = aterm_secrets::client::admin_seed_swarm_roster(paths, &[]).unwrap();
    roster.deny_labels.push("m2".to_owned());
    let admin = aterm_secrets::client::admin_open(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    aterm_secrets::client::admin_publish(paths, &admin, &doc).unwrap();
    aterm_secrets::client::sync(paths, None).unwrap();

    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    let report = import(paths, &["--apply", "--first"], &vault, &logins, &mut opened);
    assert_eq!(report["known_macs"], json!(1), "{report}");
    let view = Registry::at(paths.clone()).view().unwrap();
    assert!(
        !view
            .project(&MACHINE)
            .rows
            .values()
            .any(|row| row.get("label") == Some(&json!("m2"))),
        "denied roster member m2 must not birth a machine row"
    );

    let mut no_review = |_: &Path| Ok(());
    let adopted = run(
        paths,
        &words(&["adopt", "--dry-run"]),
        None,
        &mut no_review,
        NOW,
    )
    .unwrap();
    assert_eq!(adopted["known_macs"], json!(1), "{adopted}");
}

/// A harness stand-in that renders two exports from the view: each lists the accounts the lab
/// holds (by key) and the place rows the journal holds, so the writer's path is the one under test.
struct Render;

impl Vendors for Render {
    fn mark(&self, provider: &str, cell: &str, notes: &str) -> import::Mark {
        Stand.mark(provider, cell, notes)
    }

    fn export_names(&self) -> Vec<String> {
        vec!["fleet-accounts.toml".to_owned(), "ai-pool.toml".to_owned()]
    }

    fn exports(
        &self,
        view: &super::super::view::LabView,
        places: &super::super::journal::place::Places,
        current: &BTreeMap<String, String>,
    ) -> std::result::Result<Vec<(String, String)>, String> {
        if current.values().any(|text| text.contains("unparseable")) {
            return Err("a current file does not parse".to_owned());
        }
        let keys: Vec<&str> = view.accounts.keys().map(String::as_str).collect();
        Ok(self
            .export_names()
            .into_iter()
            .map(|name| {
                let body = format!(
                    "# {name}\n# {}\n# places {}\n",
                    keys.join(" "),
                    places.rows.len()
                );
                (name, body)
            })
            .collect())
    }
}

/// `export` renders through the harness, publishes the changed files the vault holds in one admin
/// publish of the scratch vault and never creates one it does not hold, and a second run publishes
/// nothing; once the vault is synced `check` reports the held file `same` and the other `missing`,
/// an edit by hand `edited`, and a dry run never publishes. Nothing here touches a real vault: the
/// lab's paths are scratch homes.
/// NEGATIVE CONTROL: with `run` publishing whatever `plan` says (no compare-before-write: `plan`
/// keeping every file), the second `--apply` publishes again (`left: true right: false`); with
/// `plan` writing a missing file, the dry run plans `ai-pool.toml` (`"write": Bool(true)`).
#[test]
fn the_export_publishes_once_and_check_reports_its_drift() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    import(paths, &["--apply", "--first"], &vault, &logins, &mut opened);
    // The scratch vault holds the fleet file an older Mac reads; it holds no pool.
    let admin = aterm_secrets::client::admin_open(paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        "fleet-accounts.toml",
        b"# by hand\n",
    )
    .unwrap();
    aterm_secrets::client::admin_publish(paths, &admin, &doc).unwrap();
    let mut none = |_: &Path| Ok(());
    let mut export = |extra: &[&str]| {
        let mut list = vec!["export"];
        list.extend_from_slice(extra);
        run(paths, &words(&list), Some(&Render), &mut none, NOW).unwrap()
    };
    let dry = export(&["--dry-run"]);
    assert_eq!(dry["published"], json!(false), "{dry}");
    assert_eq!(
        dry["files"],
        json!([
            {"file": "fleet-accounts.toml", "state": "unsealed", "write": true},
            {"file": "ai-pool.toml", "state": "missing", "write": false},
        ]),
        "{dry}"
    );
    let first = export(&["--apply"]);
    assert_eq!(first["published"], json!(true), "{first}");
    let second = export(&["--apply"]);
    assert_eq!(
        second["published"],
        json!(false),
        "compare before write: {second}"
    );
    aterm_secrets::client::sync(paths, None).unwrap();
    let held = std::fs::read_to_string(paths.home.join(".secrets/fleet-accounts.toml")).unwrap();
    assert!(
        held.starts_with("# @generated by aterm db alab export --apply (seal "),
        "{held}"
    );
    assert!(
        !paths.home.join(".secrets/ai-pool.toml").exists(),
        "a file the vault did not hold is never created"
    );
    let check = || {
        run(
            paths,
            &words(&["check"]),
            Some(&Render),
            &mut |_: &Path| Ok(()),
            NOW,
        )
        .unwrap()
    };
    let states = |report: &Value| -> Vec<String> {
        report["exports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["state"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(states(&check()), ["same", "missing"]);
    let file = paths.home.join(".secrets/fleet-accounts.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("# places", "# hand edit, places")).unwrap();
    assert_eq!(states(&check()), ["edited", "missing"]);
    // A current file the harness cannot parse refuses the export and is reported, not hidden.
    std::fs::write(&file, "unparseable").unwrap();
    assert!(check()["exports"]["error"].as_str().is_some());
}

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

/// The verbs' scripted [`Io`]: an opener that runs `edit`, stdin that answers `stdin`, no review
/// and no heard archive (the window alone, never this Mac's real spool).
fn verb_io<'a>(
    paths: &Paths,
    list: &[&str],
    open: &'a mut dyn FnMut(&Path, usize) -> Result<()>,
    stdin: &str,
) -> Result<Value> {
    let mut review = |_: &Path| Ok(());
    let line = stdin.to_owned();
    let mut read = move || Ok(line.clone());
    let mut io = Io {
        review: &mut review,
        open,
        stdin: &mut read,
        heard: None,
    };
    run_io(paths, &words(list), None, &mut io, NOW)
}

fn verb(paths: &Paths, list: &[&str]) -> Result<Value> {
    verb_io(paths, list, &mut |_: &Path, _| Ok(()), "")
}

/// The readers print the registry by key and label: `accounts` gives each account's label and
/// the machines that prefer it, `mail` each address's key, its mailbox's key and label and the
/// accounts that sign in with it, and no reader prints an address.
/// NEGATIVE CONTROL: with `mail` adding each row's `addr` to its entry (`"addr": text(row,
/// "addr")`), the report holds an address: `` mail: {"addresses":[{"accounts":… ``.
#[test]
fn the_readers_print_keys_and_labels_never_an_address() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    import(
        paths,
        &["--apply", "--first"],
        &vault,
        &logins,
        &mut Vec::new(),
    );
    let claude = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let label = keys::default_account_label("claude", &claude).unwrap();
    for name in [
        "machines",
        "accounts",
        "preferences",
        "mail",
        "signins",
        "quota",
    ] {
        let out = verb(paths, &[name]).unwrap();
        let text = aterm_json::to_string(&out).unwrap();
        assert!(!text.contains('@'), "{name}: {text}");
    }
    let accounts = verb(paths, &["accounts"]).unwrap();
    let ann = accounts["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["ac"] == json!(claude))
        .unwrap();
    assert_eq!(ann["label"], json!(label));
    assert!(!ann["preferred_by"].as_array().unwrap().is_empty());
    let mail = verb(paths, &["mail"]).unwrap();
    let ad = keys::address_pk("ann@example.invalid").unwrap();
    let entry = mail["addresses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["ad"] == json!(ad))
        .unwrap();
    assert!(
        entry["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["ac"] == json!(claude))
    );
    assert_eq!(mail["mailboxes"].as_array().unwrap().len(), 1);
    let machines = verb(paths, &["machines"]).unwrap();
    assert!(
        machines["machines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["label"] == json!("m3") && m["prefers"].as_u64().unwrap() > 0)
    );
    assert!(verb(paths, &["machines", "extra"]).is_err());
    assert!(verb(paths, &["who", "ann@example.invalid"]).is_err());
}

/// `add machine` writes a planned row once and refuses a label another machine holds; `relabel`
/// moves the label and keeps the old one as an alias; `retire` writes the row's lifecycle and the
/// reason into its note, and a retire said again after the row came back is written again (the
/// owner's verbs take a random request id), while a repeat of what the row holds writes nothing.
/// NEGATIVE CONTROL: with `commit_owner` taking content-derived request ids (`self.commit_planned
/// (false, false, derive)`), the second retire is read as a retry of the first and the row stays
/// live: `` left: Some("live") right: Some("retired") ``. With `add_machine` keying a row only
/// by `machine_pk(label)` (refusing a key a retired row holds), planning the retired Mac's label
/// again is refused: the `unwrap` on `replaced` panics. With `retire` skipping the note guard on
/// `--why`, the `pin` reason is written: `assertion failed: verb(...).is_err()`.
#[test]
fn the_owner_adds_relabels_and_retires_a_machine() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    import(
        paths,
        &["--apply", "--first"],
        &vault,
        &logins,
        &mut Vec::new(),
    );
    let added = verb(paths, &["add", "machine", "m87", "--role", "worker"]).unwrap();
    let pk = added["mc"].as_str().unwrap().to_owned();
    assert_eq!(pk, keys::machine_pk("m87"));
    let rows = || {
        Registry::at(paths.clone())
            .view()
            .unwrap()
            .project(&MACHINE)
    };
    assert_eq!(rows().rows[&pk].get("status"), Some(&json!("planned")));
    assert_eq!(rows().rows[&pk].get("role"), Some(&json!("worker")));
    assert!(verb(paths, &["add", "machine", "m87"]).is_err());
    assert!(
        verb(paths, &["add", "machine", "m3"]).is_err(),
        "m3 is held"
    );
    assert!(verb(paths, &["add", "machine", "m88", "--model", "Mac99-9"]).is_err());
    assert!(
        verb(paths, &["relabel", "m87", "m3"]).is_err(),
        "m3 is held"
    );
    let moved = verb(paths, &["relabel", "m87", "m107"]).unwrap();
    assert_eq!(moved["from"], json!("m87"));
    assert_eq!(rows().rows[&pk].get("label"), Some(&json!("m107")));
    assert!(rows().rows[&pk].fields["aliases"].members().contains("m87"));
    assert_eq!(
        verb(paths, &["relabel", "m107", "m107"]).unwrap()["written"],
        json!(0)
    );
    let retired = verb(paths, &["retire", "machine", "m87", "--why", "sold"]).unwrap();
    assert_eq!(retired["pk"], json!(pk));
    assert_eq!(rows().rows[&pk].lifecycle(), Some("retired"));
    assert_eq!(rows().rows[&pk].get("note"), Some(&json!("retired: sold")));
    // A reason the note guard withholds, and one that would pass the note's 512 bytes, are
    // refused before anything is written.
    let note = || rows().rows[&pk].get("note").cloned();
    assert!(
        verb(
            paths,
            &["retire", "machine", "m107", "--why", "pin 4821 reset"]
        )
        .is_err()
    );
    let long = "x".repeat(500);
    assert!(verb(paths, &["retire", "machine", "m107", "--why", &long]).is_err());
    assert_eq!(note(), Some(json!("retired: sold")));
    // The retired Mac's label planned again, for a replacement: a row of its own, under the first
    // free `<label>/<n>` birth, the retired row and its history untouched.
    let replaced = verb(paths, &["add", "machine", "m87"]).unwrap();
    let fresh = replaced["mc"].as_str().unwrap().to_owned();
    assert_eq!(fresh, keys::machine_pk("m87/2"));
    assert_eq!(rows().rows[&fresh].get("label"), Some(&json!("m87")));
    assert_eq!(rows().rows[&pk].lifecycle(), Some("retired"));
    // Brought back by hand, then retired again with no reason: the same change is written again.
    Registry::at(paths.clone())
        .write(vec![vec![put(&MACHINE, &pk, "row", json!("live"))]])
        .unwrap();
    let again = verb(paths, &["retire", "machine", "m107"]).unwrap();
    assert_eq!(again["written"], json!(1));
    assert_eq!(rows().rows[&pk].lifecycle(), Some("retired"));
    // The same change set a second time: what a content-derived id would take for a retry.
    Registry::at(paths.clone())
        .write(vec![vec![put(&MACHINE, &pk, "row", json!("live"))]])
        .unwrap();
    verb(paths, &["retire", "machine", "m107"]).unwrap();
    assert_eq!(
        rows().rows[&pk].lifecycle(),
        Some("retired"),
        "a retire said again is written again"
    );
    assert_eq!(
        verb(paths, &["retire", "machine", "m107"]).unwrap()["written"],
        json!(0)
    );
}

/// `add account` reads its address from stdin, never argv, births the address row and a
/// `pending` account under the canonical key, prints only keys and the default label, and refuses
/// the same account twice.
/// NEGATIVE CONTROL: with `add_account` printing the address it read (`"addr": addr` in its
/// report), the report holds an `@`: `` {"ac":"ac-…","addr":"new@example.test",… ``.
#[test]
fn an_account_is_added_from_stdin_and_its_address_never_printed() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    import(
        paths,
        &["--apply", "--first"],
        &vault,
        &logins,
        &mut Vec::new(),
    );
    let mut open = |_: &Path, _| Ok(());
    let added = verb_io(
        paths,
        &["add", "account", "claude"],
        &mut open,
        "new@example.test\n",
    )
    .unwrap();
    let text = aterm_json::to_string(&added).unwrap();
    assert!(!text.contains('@'), "{text}");
    let ac = keys::account_pk("claude", "new@example.test").unwrap();
    assert_eq!(added["ac"], json!(ac));
    assert_eq!(added["address_born"], json!(true));
    let view = Registry::at(paths.clone()).view().unwrap();
    let row = &view.project(&ACCOUNT).rows[&ac];
    assert_eq!(row.get("status"), Some(&json!("pending")));
    assert_eq!(
        row.get("login"),
        Some(&json!(keys::address_pk("new@example.test").unwrap()))
    );
    assert!(
        verb_io(
            paths,
            &["add", "account", "claude"],
            &mut open,
            "new@example.test\n"
        )
        .is_err()
    );
    assert!(verb(paths, &["add", "account", "x@example.test"]).is_err());
    assert!(
        verb_io(
            paths,
            &["add", "account", "nobody"],
            &mut open,
            "new@example.test\n"
        )
        .is_err()
    );
    // An existing address gets an account of another realm without a second address row.
    let codex = verb_io(
        paths,
        &["add", "account", "codex"],
        &mut open,
        "new@example.test",
    )
    .unwrap();
    assert_eq!(codex["address_born"], json!(false));
}

/// `edit machines` opens the machine grid in the scripted editor and saves the owner's cell
/// through the grid's three-way save; `edit mail` opens the address grid, its addresses masked.
/// NEGATIVE CONTROL: with `edited` opening the account collection for `machines`, the opened
/// file is `account.tsv` and `record` finds no machine row there: `` called `Option::unwrap()`
/// on a `None` value ``.
#[test]
fn edit_saves_the_owners_cell_through_the_grid() {
    use crate::typed::grid::tests::{record, type_in};
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    import(
        paths,
        &["--apply", "--first"],
        &vault,
        &logins,
        &mut Vec::new(),
    );
    let m3 = keys::machine_pk("m3");
    let mut opened = Vec::new();
    let mut open = |path: &Path, _: usize| -> Result<()> {
        opened.push(path.file_name().unwrap().to_string_lossy().into_owned());
        let held = std::fs::read_to_string(path).unwrap();
        if opened.len() == 1 {
            let row = record(&held, &m3);
            std::fs::write(path, type_in(&held, row, "role", "desk")).unwrap();
        } else {
            assert!(!held.contains("@example"), "masked: {held}");
        }
        Ok(())
    };
    let edited = verb_io(paths, &["edit", "machines"], &mut open, "").unwrap();
    assert_eq!(edited["cells"], json!(1), "{edited}");
    assert_eq!(
        Registry::at(paths.clone())
            .view()
            .unwrap()
            .project(&MACHINE)
            .rows[&m3]
            .get("role"),
        Some(&json!("desk"))
    );
    let mail = verb_io(paths, &["edit", "mail"], &mut open, "").unwrap();
    assert_eq!(mail["records"], json!(0));
    assert_eq!(opened, ["machine.tsv", "address.tsv"]);
    assert!(verb(paths, &["edit", "phones"]).is_err());
}

/// `who`, `quota`, `log` and `signins` read the journal window: after one Mac's flushed session,
/// sign-in and reading, `who` names that Mac's enrollment and the time, `quota` the window's
/// usage, `signins` its sign-in word, and `log` the account's three events.
/// NEGATIVE CONTROL: with `log` matching events by the machine filter alone (`(Some(pk), _) =>
/// false`), the account's log is empty: `left: 0 right: 3`.
#[test]
fn the_journal_readers_say_who_used_an_account_and_its_quota() {
    use crate::alab::journal::{
        Event, Kind, Pk, Presence, Res, Source, Spool, Window, Word,
        flush::{self, Flushed},
    };
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    import(
        paths,
        &["--apply", "--first"],
        &vault,
        &logins,
        &mut Vec::new(),
    );
    let claude = keys::account_pk("claude", "ann@example.invalid").unwrap();
    let label = keys::default_account_label("claude", &claude).unwrap();
    let ann = Pk::new(&claude).unwrap();
    let now = NOW / 1000;
    let base = Event {
        t: now - 600,
        kind: Kind::SessionStart,
        account: Some(ann),
        cli: Word::new("claude").unwrap(),
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        plan: None,
        src: Source::CliOutput,
        why: None,
        to: None,
    };
    let spool = Spool::at(scratch.path().join("alab"));
    spool.append(&base).unwrap();
    spool
        .append(&Event {
            t: now - 590,
            kind: Kind::SignedIn,
            ..base
        })
        .unwrap();
    spool
        .append(&Event {
            t: now - 580,
            kind: Kind::Quota,
            window: Some(Window::FiveHour),
            pct_x10: Some(250),
            reset: Some(now + 3_600),
            res: Some(Res::Exact),
            src: Source::Statusline,
            ..base
        })
        .unwrap();
    let pass = |at| flush::pass(paths, &spool, &Presence::default, false, at).unwrap();
    assert!(matches!(pass(now - 120), Flushed::Wrote { .. }));
    assert!(matches!(pass(now - 60), Flushed::Wrote { .. }));
    let who = verb(paths, &["who", &label]).unwrap();
    assert_eq!(who["last_use"]["t"], json!(now - 600));
    assert_eq!(who["last_use"]["ago_s"], json!(600));
    assert!(
        who["last_use"]["uuid"]
            .as_str()
            .is_some_and(|u| u.len() == 32)
    );
    let quota = verb(paths, &["quota", &label]).unwrap();
    let window = &quota["accounts"][0]["windows"][0];
    assert_eq!(window["window"], json!("5h"));
    assert_eq!(window["usage"]["state"], json!("current"));
    assert_eq!(window["usage"]["used_pct_x10"], json!(250));
    // What runs on m3 is its preferences with each account's quota from the same window.
    let m3 = verb(paths, &["machines", "m3"]).unwrap();
    let on = m3["prefers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["ac"] == json!(claude))
        .expect("m3 prefers ann's account");
    assert_eq!(on["quota"][0]["usage"]["used_pct_x10"], json!(250));
    assert!(!aterm_json::to_string(&m3).unwrap().contains('@'));
    let signins = verb(paths, &["signins"]).unwrap();
    assert_eq!(signins["accounts"][0]["machines"][0]["state"], json!("in"));
    let log = verb(paths, &["log", &label, "--days", "1"]).unwrap();
    assert_eq!(log["events"].as_array().unwrap().len(), 3, "{log}");
    assert_eq!(log["events"][0]["kind"], json!("session-start"));
    let text = aterm_json::to_string(&log).unwrap();
    assert!(!text.contains('@'), "{text}");
    assert!(verb(paths, &["log", "nobody"]).is_err());
}

/// `add account` reads its address from stdin's first line and returns at its newline (a
/// terminal's Enter), never waiting for end of input.
/// NEGATIVE CONTROL: with `first_line` reading to end of input (`read_to_end`), it takes the
/// following line too: `left: "x@example.test\nnext" right: "x@example.test\n"`.
#[test]
fn the_address_is_stdins_first_line_without_waiting_for_its_end() {
    let mut input = std::io::Cursor::new(b"x@example.test\nnext".to_vec());
    assert_eq!(first_line(&mut input).unwrap(), "x@example.test\n");
    let mut unterminated = std::io::Cursor::new(b"y@example.test".to_vec());
    assert_eq!(first_line(&mut unterminated).unwrap(), "y@example.test");
}

/// `machines m2` answers §7's "What runs on m2?": the machine's words, the hardware of the facts
/// row bound to it (never its host names), its preferences by rank with each account's label,
/// status and plan, the account it runs and does not prefer (its place row, with why), and the
/// latest `sum` row of the Mac bound to it; another Mac's facts, place and sum rows stay out.
/// NEGATIVE CONTROL: with `runs_on` reading every `sum` row rather than the bound Macs'
/// (`seen.sums.keys()` in place of `uuids`), m3's sum shows on m2: `left: 2 right: 1`.
#[test]
fn what_runs_on_a_machine_is_its_preferences_places_facts_and_sum() {
    use crate::alab::{
        journal::{
            place::{Placed, Places, Why},
            seen::Seen,
        },
        view::{Account, LabView, Machine, Preference},
    };
    let (a, b) = ("a".repeat(32), "b".repeat(32));
    let mut lab = LabView::default();
    for (pk, label) in [("mc-2", "m2"), ("mc-3", "m3")] {
        lab.machines.insert(
            pk.into(),
            Machine {
                pk: pk.into(),
                label: Some(label.into()),
                aliases: Vec::new(),
                live: true,
            },
        );
    }
    lab.bound = [
        (a.clone(), "mc-2".to_owned()),
        (b.clone(), "mc-3".to_owned()),
    ]
    .into();
    for (pk, label) in [
        ("ac-1", "claude-111111"),
        ("ac-2", "claude-222222"),
        ("ac-3", "claude-333333"),
    ] {
        lab.accounts.insert(
            pk.into(),
            Account {
                pk: pk.into(),
                realm: Some("claude".into()),
                label: Some(label.into()),
                legacy_label: None,
                login: Some(format!("{pk}@example.test")),
                identity: None,
                status: Some("active".into()),
                region: None,
                flow: None,
                browser: None,
                live: true,
            },
        );
    }
    lab.preferences = vec![
        Preference {
            machine: "mc-2".into(),
            account: "ac-1".into(),
            rank: Some(1),
        },
        Preference {
            machine: "mc-2".into(),
            account: "ac-2".into(),
            rank: Some(2),
        },
        Preference {
            machine: "mc-3".into(),
            account: "ac-3".into(),
            rank: Some(1),
        },
    ];
    let now = NOW / 1000;
    let mut seen = Seen::empty(now);
    seen.places = Places {
        rows: vec![
            Placed {
                uuid: a.clone(),
                account: "ac-3".into(),
                why: Why::Borrow,
                since: now - 600,
                until: now + 600,
            },
            Placed {
                uuid: b.clone(),
                account: "ac-1".into(),
                why: Why::Borrow,
                since: now - 600,
                until: now + 600,
            },
        ],
    };
    seen.sums = [
        (
            a.clone(),
            json!({"label": "m2", "aterm": "0.1.0", "journal": "on", "used": 2, "at": now - 60}),
        ),
        (
            b.clone(),
            json!({"label": "m3", "aterm": "0.1.0", "journal": "off", "used": 1, "at": now - 60}),
        ),
    ]
    .into();
    let facts = [
        (
            a.clone(),
            json!({"chip": "M4", "ram_gb": 64, "hosts": ["host-a"]}),
        ),
        (
            b.clone(),
            json!({"chip": "M1", "ram_gb": 8, "hosts": ["host-b"]}),
        ),
    ]
    .into();
    let plans = |pk: &str| {
        if pk == "ac-1" {
            json!("pl-max")
        } else {
            Value::Null
        }
    };
    let out = verbs::runs_on(&lab, None, &facts, &plans, &seen, "mc-2");
    assert_eq!(out["label"], json!("m2"));
    let labels = |list: &Value| -> Vec<Value> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|e| e["label"].clone())
            .collect()
    };
    assert_eq!(
        labels(&out["prefers"]),
        [json!("claude-111111"), json!("claude-222222")]
    );
    assert_eq!(out["prefers"][0]["rank"], json!(1));
    assert_eq!(out["prefers"][0]["plan"], json!("pl-max"));
    assert_eq!(out["prefers"][0]["status"], json!("active"));
    assert_eq!(labels(&out["places"]), [json!("claude-333333")]);
    assert_eq!(out["places"][0]["why"], json!("borrow"));
    assert_eq!(out["places"][0]["since"]["ago_s"], json!(600));
    assert_eq!(out["facts"].as_array().unwrap().len(), 1);
    assert_eq!(out["facts"][0]["chip"], json!("M4"));
    assert!(out["facts"][0].get("hosts").is_none(), "no host names");
    assert_eq!(out["sum"].as_array().unwrap().len(), 1);
    assert_eq!(out["sum"][0]["journal"], json!("on"));
    assert_eq!(out["sum"][0]["at"]["ago_s"], json!(60));
    let text = aterm_json::to_string(&out).unwrap();
    assert!(!text.contains('@'), "{text}");
}

/// A label a retired row and its live replacement share (the importer births the replacement
/// for a retired m40 as `m40/2`, also labelled m40) names the live row, as the importer's
/// `live_by_label` reads it; a label only a retired row holds still names that row, for its
/// history; two live rows under one label name neither.
/// NEGATIVE CONTROL: with `machine_named` counting every row alike (`found` in place of `live`),
/// m40 names no row: `left: None right: Some("mc-new")`.
#[test]
fn a_label_a_retired_row_shares_names_the_live_replacement() {
    use crate::alab::view::{LabView, Machine};
    let mut lab = LabView::default();
    for (pk, label, live) in [
        ("mc-old", "m40", false),
        ("mc-new", "m40", true),
        ("mc-gone", "m41", false),
        ("mc-a", "m42", true),
        ("mc-b", "m42", true),
    ] {
        lab.machines.insert(
            pk.into(),
            Machine {
                pk: pk.into(),
                label: Some(label.into()),
                aliases: Vec::new(),
                live,
            },
        );
    }
    assert_eq!(verbs::machine_named(&lab, "m40").as_deref(), Some("mc-new"));
    assert_eq!(
        verbs::machine_named(&lab, "m41").as_deref(),
        Some("mc-gone")
    );
    assert_eq!(verbs::machine_named(&lab, "m42"), None);
    assert_eq!(
        verbs::machine_named(&lab, "mc-old").as_deref(),
        Some("mc-old")
    );
}

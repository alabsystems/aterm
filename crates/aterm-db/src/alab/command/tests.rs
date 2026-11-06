// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db alab` on real Commons labs: isolated scratch homes, no live vault, fleet or
//! Commons, synthetic sheets with the real layouts. The vendor reading is the stand-in of
//! `import::tests`; the review opener records what it was asked to open and what each file held
//! while open (the folder goes once the review ends), so no window opens.

use super::*;

#[test]
fn known_macs_waits_for_the_roster_writer_instead_of_reporting_no_macs() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    aterm_secrets::client::admin_seed_swarm_roster(paths, &[]).unwrap();
    let writer = std::rc::Rc::new(std::cell::RefCell::new(Some(
        aterm_secrets::client::test_hold_state_lock(paths).unwrap(),
    )));
    assert!(
        aterm_secrets::client::test_without_roster_wait(|| known_macs(paths))
            .unwrap()
            .is_empty(),
        "the negative control loses every Mac"
    );
    let release = writer.clone();
    let macs = aterm_secrets::client::test_with_denied_nodes_busy(
        move || drop(release.borrow_mut().take()),
        || known_macs(paths),
    )
    .unwrap();
    assert!(!macs.is_empty());
    assert!(
        writer.borrow().is_none(),
        "the reader observed the writer before release"
    );
}
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

pub(super) const NOW: u64 = 1_800_000_000_000;

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

pub(super) fn words(list: &[&str]) -> Vec<String> {
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
/// preferences are rows, the mailbox keyed by its login's address key, and a second `--apply` of
/// the same sheets writes no record.
/// NEGATIVE CONTROLS: with `cell::import_step` never answering `Unchanged` (`if false &&
/// candidate.value.x == *new`), the rerun rewrites every import-held cell: `` left: Some(2)
/// right: Some(0) `` at the second run's `records`.
#[test]
fn import_applies_once_then_a_rerun_writes_nothing() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut opened = Vec::new();
    let first = import(paths, &["--apply"], &vault, &logins, &mut opened);
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
    let mailbox = keys::mailbox_pk(&keys::address_pk("box@proton.example").unwrap()).unwrap();
    assert!(view.project(&MAILBOX).rows.contains_key(&mailbox));
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
    let first = import(paths, &["--apply"], &vault, &logins, &mut opened);
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

/// An import whose seed pass failed wrote rows that name providers nothing holds yet, so it is no
/// clean exit, and its report keeps `seeds` a number. NEGATIVE CONTROL: with [`exit_of`] answering
/// 0, or the failure put in `seeds` as main did, the failing half's assertions fail; the passing
/// half holds either way.
#[test]
fn a_seed_pass_that_failed_is_no_clean_exit() {
    let mut failed = json!({});
    report_seeds(
        &mut failed,
        Err(Error::new("application pass deadline expired")),
    );
    assert_eq!(failed["seeds"].as_u64(), Some(0), "{failed}");
    assert!(failed[SEEDS_FAILED].as_str().is_some(), "{failed}");
    assert_eq!(exit_of(&failed), 1, "{failed}");

    let mut seeded = json!({});
    report_seeds(
        &mut seeded,
        Ok(crate::alab::Seeded {
            records: 2,
            ..Default::default()
        }),
    );
    assert_eq!(seeded["seeds"].as_u64(), Some(2), "{seeded}");
    assert_eq!(exit_of(&seeded), 0, "{seeded}");
}

/// A seed pass runs to the deadline its caller names, which is what lets an import outlast a
/// product open's budget on a loaded Mac. NEGATIVE CONTROL: with `seed_until` ignoring `deadline`
/// (`application::with_store`), the expired deadline's pass writes and this fails.
#[test]
fn a_seed_pass_runs_to_the_deadline_its_caller_names() {
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    assert!(registry.seed_until(std::time::Instant::now()).is_err());
    let within = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    assert!(registry.seed_until(within).unwrap().records >= 1);
}

/// The first import in the lab needs no flag, and a second Mac that holds the registry after a
/// repair round imports the same sheets to the same rows: its mailbox keys derive from their
/// logins, so it writes nothing and mints no duplicate mailbox, with no key file anywhere.
/// NEGATIVE CONTROL: with `mailbox` keying a new mailbox by `keys::random_pk`, the second Mac
/// mints its own: `` left: 2 right: 1 ``.
#[test]
fn any_mac_imports_the_same_sheets_to_the_same_rows() {
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
    let first = run(
        &lab.paths[0],
        &words_for(&["--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert!(first["records"].as_u64().unwrap() > 0, "{first}");
    Registry::at(lab.paths[1].clone()).open().unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    let again = run(
        &lab.paths[1],
        &words_for(&["--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert_eq!(again["records"].as_u64(), Some(0), "{again}");
    let after = Registry::at(lab.paths[1].clone()).view().unwrap();
    assert_eq!(after.project(&MAILBOX).rows.len(), 1);
    for path in &lab.paths {
        assert!(!path.dir.join("alab-mailbox-keys.json").exists());
    }
}

/// An import into a registry this Mac holds none of waits while a peer's fresh round over the
/// registry's table says the lab holds what this Mac has not heard, and while no peer's round
/// says anything; it goes ahead where no peer is known or a fresh round exchanged nothing.
/// NEGATIVE CONTROL: with the `(known && !drained)` refusal dropped (always `None` at the end), a
/// stale round lets the import through: `` assertion failed: waits(round(stale, ...)).is_some() ``.
#[test]
fn an_empty_view_waits_only_while_a_peer_holds_the_registry() {
    use aterm_types::network_state::{RepairObservation, RepairOutcome, RepairStage};
    let table = [3u8; 32];
    let round = |at: u64, outcome, stage, on: [u8; 32], received, deferred| RepairObservation {
        completed_at_ms: at,
        outbound: true,
        stage,
        outcome,
        table: Some(on),
        sent: None,
        received,
        deferred,
        cause: None,
    };
    let recent = NOW - 60_000;
    let stale = NOW - 60 * 60_000;
    let waits =
        |repair: RepairObservation| empty_view_wait(Some(table), None, [Some(&repair)], NOW);
    let exchanged =
        |at, on, received| round(at, RepairOutcome::Exchanged, None, on, received, None);
    // A fresh round that brought, deferred or failed exchanging the registry.
    assert!(waits(exchanged(recent, table, Some(4))).is_some());
    assert!(
        waits(round(
            recent,
            RepairOutcome::Deferred,
            None,
            table,
            None,
            Some(2)
        ))
        .is_some()
    );
    assert!(
        waits(round(
            recent,
            RepairOutcome::Failed,
            Some(RepairStage::Application),
            table,
            None,
            None
        ))
        .is_some()
    );
    // No round that speaks: stale, over another table, failed before exchanging, or none.
    assert!(waits(exchanged(stale, table, Some(0))).is_some());
    assert!(waits(exchanged(recent, [4; 32], Some(0))).is_some());
    assert!(
        waits(round(
            recent,
            RepairOutcome::Failed,
            Some(RepairStage::Transport),
            table,
            None,
            None
        ))
        .is_some()
    );
    assert!(empty_view_wait(Some(table), None, [None], NOW).is_some());
    // A fresh round that exchanged and brought nothing, beside one that says nothing.
    assert_eq!(waits(exchanged(recent, table, Some(0))), None);
    let drained = exchanged(recent, table, Some(0));
    assert_eq!(
        empty_view_wait(Some(table), None, [None, Some(&drained)], NOW),
        None
    );
    // The lab's first import: no peer is known.
    assert_eq!(empty_view_wait(Some(table), None, [], NOW), None);
    // Where this Mac cannot name the registry's table, any table's round speaks.
    assert!(
        empty_view_wait(
            None,
            None,
            [Some(&exchanged(recent, [4; 32], Some(1)))],
            NOW
        )
        .is_some()
    );
    assert_eq!(
        empty_view_wait(
            None,
            None,
            [Some(&exchanged(recent, [4; 32], Some(0)))],
            NOW
        ),
        None
    );
}

/// Before the lab's first import no Mac holds the registry, so no repair round ever covers it,
/// and a round over the catalog speaks instead, where this Mac's catalog holds no other Mac's
/// declaration of the registry (the guard passes the catalog's id only then): one that exchanged
/// and brought nothing lets the first import go, beside a peer that says nothing; one that
/// deferred or failed exchanging the catalog waits; one that brought catalog records, a stale one
/// or one over another table says nothing. Measured on three Macs for E1 (2026-11-05): with only
/// the catalog and the journal held, every round was over one of them and the first import
/// refused for good.
/// NEGATIVE CONTROL: with the catalog arm of `empty_view_wait` removed (run 2026-11-05, then
/// restored), the drained catalog round says nothing and the first import waits: `` left:
/// Some("no peer's repair round over the registry or the catalog …") right: None ``. The same
/// refusal is asserted below for a guard handed no catalog.
#[test]
fn the_labs_first_import_goes_on_a_drained_catalog_round() {
    use aterm_types::network_state::{RepairObservation, RepairOutcome, RepairStage};
    let (table, catalog, journal) = ([3u8; 32], [5u8; 32], [6u8; 32]);
    let round = |at: u64, outcome, stage, on: [u8; 32], received, deferred| RepairObservation {
        completed_at_ms: at,
        outbound: true,
        stage,
        outcome,
        table: Some(on),
        sent: None,
        received,
        deferred,
        cause: None,
    };
    let recent = NOW - 60_000;
    let stale = NOW - 60 * 60_000;
    let exchanged =
        |at, on, received| round(at, RepairOutcome::Exchanged, None, on, received, None);
    let drained = exchanged(recent, catalog, Some(0));
    let quiet = exchanged(recent, journal, Some(0));
    assert_eq!(
        empty_view_wait(
            Some(table),
            Some(catalog),
            [Some(&drained), Some(&quiet)],
            NOW
        ),
        None
    );
    assert_eq!(
        empty_view_wait(Some(table), Some(catalog), [None, Some(&drained)], NOW),
        None
    );
    // Without the catalog's word (another Mac's declaration is here, or no catalog was read) the
    // same rounds say nothing and the import waits.
    assert!(empty_view_wait(Some(table), None, [Some(&drained), Some(&quiet)], NOW).is_some());
    let waits = |repair: RepairObservation| {
        empty_view_wait(Some(table), Some(catalog), [Some(&repair)], NOW)
    };
    assert!(
        waits(round(
            recent,
            RepairOutcome::Deferred,
            None,
            catalog,
            None,
            Some(1)
        ))
        .is_some()
    );
    assert!(
        waits(round(
            recent,
            RepairOutcome::Failed,
            Some(RepairStage::Application),
            catalog,
            None,
            None
        ))
        .is_some()
    );
    assert!(waits(exchanged(recent, catalog, Some(2))).is_some());
    assert!(waits(exchanged(stale, catalog, Some(0))).is_some());
    assert!(waits(quiet).is_some());
    // A peer that holds the registry still holds the import back whatever the catalog said.
    assert!(
        empty_view_wait(
            Some(table),
            Some(catalog),
            [Some(&drained), Some(&exchanged(recent, table, Some(3)))],
            NOW
        )
        .is_some()
    );
}

/// The catalog's word on a two-Mac lab: before any import neither Mac's catalog declares the
/// registry, so each import may read its peers' catalog rounds; once one Mac's import declared it
/// and a repair round carried the declaration, the other Mac's catalog holds another Mac's
/// declaration, and an import there into a registry it held nothing of is refused before any
/// round is read, while the importer's own declaration is not another Mac's.
/// NEGATIVE CONTROL: with `peek_declaration` counting this Mac's own declaration too, the
/// importer reads its own declaration as another's: `` left: Some((…, true)) right: Some((…,
/// false)) ``.
#[test]
fn another_macs_declaration_of_the_registry_holds_an_empty_import_back() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let word = |i: usize| Registry::at(lab.paths[i].clone()).catalog_word().unwrap();
    let (catalog, before) = word(1).expect("a Commons is here");
    assert!(!before);
    assert_eq!(word(0), Some((catalog, false)));
    assert_eq!(
        super::empty_view_catalog(&lab.paths[1], 0).unwrap(),
        Some(catalog)
    );
    let scratch = aterm_tempfile::tempdir().unwrap();
    let (vault, logins) = sheets(scratch.path());
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let first = run(
        &lab.paths[0],
        &words(&[
            "import",
            "--vault-sheet",
            vault.to_str().unwrap(),
            "--logins",
            logins.to_str().unwrap(),
            "--apply",
        ]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert!(first["records"].as_u64().unwrap() > 0, "{first}");
    assert_eq!(word(0), Some((catalog, false)));
    crate::table::tests::repair_commons(&lab, 6);
    assert_eq!(word(1), Some((catalog, true)));
    let refused = super::empty_view_catalog(&lab.paths[1], 0).unwrap_err();
    assert!(
        refused.to_string().contains("another Mac declared it"),
        "{refused}"
    );
    // Once it holds the registry's records, the catalog is not read at all.
    assert_eq!(super::empty_view_catalog(&lab.paths[1], 1).unwrap(), None);
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
    import(paths, &["--apply"], &vault, &logins, &mut opened);
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
/// list becomes one preference per machine. A Mac holding no `[everyone]` key reads none of them,
/// since a secrets file's source is keyed by it (`Sheet::stated`), and refuses one named.
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
    let list = words(&["import", "--apply"]);
    let pool = secrets.join("ai-pool.toml");
    let named_pool = words(&["import", "--ai-pool", pool.to_str().unwrap()]);
    let keyless = run(paths, &named_pool, Some(&Stand), &mut open, NOW).unwrap_err();
    assert!(keyless.to_string().contains("[everyone]"), "{keyless}");
    let keyless = run(paths, &list, Some(&Stand), &mut open, NOW).unwrap_err();
    assert!(
        keyless.to_string().contains("no sheet to import"),
        "{keyless}"
    );
    EVERYONE.with(|held| held.set(Some(([0xab; 32], [0x11; 32]))));
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
    import(paths, &["--apply"], &vault, &logins, &mut opened);
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
    import(paths, &["--apply"], &vault, &logins, &mut opened);
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
    let report = import(paths, &["--apply"], &vault, &logins, &mut opened);
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
    import(paths, &["--apply"], &vault, &logins, &mut opened);
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
fn verb_io<'a>(paths: &Paths, list: &[&str], open: att::Opener<'a>, stdin: &str) -> Result<Value> {
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
    verb_io(paths, list, &mut |_: &Path, _, _: &[String]| Ok(()), "")
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
    import(paths, &["--apply"], &vault, &logins, &mut Vec::new());
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
    import(paths, &["--apply"], &vault, &logins, &mut Vec::new());
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
    import(paths, &["--apply"], &vault, &logins, &mut Vec::new());
    let mut open = |_: &Path, _, _: &[String]| Ok(());
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
    import(paths, &["--apply"], &vault, &logins, &mut Vec::new());
    let m3 = keys::machine_pk("m3");
    let mut opened = Vec::new();
    let mut open = |path: &Path, _: usize, _: &[String]| -> Result<()> {
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
    import(paths, &["--apply"], &vault, &logins, &mut Vec::new());
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

thread_local! {
    /// The `[everyone]` key [`super::everyone`] hands this thread instead of reading the
    /// membership history: a lab Commons here has none, and a test names the epoch it digests
    /// under.
    pub(super) static EVERYONE: std::cell::Cell<Option<([u8; 32], [u8; 32])>> =
        const { std::cell::Cell::new(None) };
}

/// One synthetic fleet_db email row: `box<i>@proton.example`, password `synthetic-pass-<i>`.
pub(super) fn email_row(i: usize, status: &str) -> (String, Value) {
    let email = format!("box{i:02}@proton.example");
    let row = json!({
        "email": email, "provider": "proton", "account_type": "root", "root_account": null,
        "password": format!("synthetic-pass-{i:02}"),
        "webmail_url": "https://mail.proton.me/u/0/inbox", "status": status,
        "phone": null, "protected": false, "assigned_computers": [], "services": [],
        "notes": null,
    });
    (email, row)
}

/// Fill this lab Mac's own fleet_db replica (`~/.aterm/lab-db` under its scratch home, with a
/// fleet key of its own) with `n` mailbox logins in `status`, made or rewritten in place; the
/// replica's folder. This thread then holds an `[everyone]` key ([`EVERYONE`]), as a member
/// holding a replica does: its dump's source is keyed by it (`import::dump_sheets`).
pub(super) fn replica_fixture(paths: &Paths, n: usize, status: &str) -> PathBuf {
    EVERYONE.with(|held| held.set(Some(([0xab; 32], [0x11; 32]))));
    let dir = super::replica_dir(paths);
    if !dir.exists() {
        for folder in [dir.parent().unwrap(), dir.as_path()] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let key = aterm_codec::hex::encode_lower(&[0x5a; 32]);
        aterm_uds::private_file::write_private(&dir, "fleet.key", key.as_bytes()).unwrap();
    }
    let mut db = crate::fleet_db::open_at(&dir).unwrap();
    for i in 1..=n {
        let (email, row) = email_row(i, status);
        crate::fleet_db::put_row(&mut db, "emails", &email, row).unwrap();
    }
    dir
}

/// `import --fleet-db` on a real lab reads this Mac's replica in place: its mode must be named, a
/// Mac with no replica is refused and none is made, and an authoritative import of 39 passwords
/// lands as 39 `Secret` cells sealed by this Mac's registry key, which open there to each row's
/// password and show masked in every report. After `--apply` the import records beside the
/// replica that the registry covers it; a re-run writes nothing; an owner's later edit to the
/// replica, read fill-only, differs by key and column only and leaves the replica uncovered, its
/// cells listed; and `check` says "not imported" until the first import.
/// NEGATIVE CONTROL: with `covered::judge` skipping the plan's findings, the edited replica is
/// recorded covered: `` assertion failed: `(left == right)` left: Bool(true) right: Bool(false) ``.
#[test]
fn a_fleet_db_replica_imports_sealed_and_records_its_coverage() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let check = verb(paths, &["check"]).unwrap();
    assert_eq!(check["registry"], json!("not imported"), "{check}");
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let mut go = |list: &[&str]| run(paths, &words(list), Some(&Stand), &mut never, NOW);
    let none = go(&["import", "--fleet-db", "--authoritative"]).unwrap_err();
    assert!(none.to_string().contains("no fleet_db replica"), "{none}");
    assert!(!super::replica_dir(paths).exists(), "no replica is made");
    let dir = replica_fixture(paths, 39, "active");
    assert!(
        go(&["import", "--fleet-db", "--apply"]).is_err(),
        "the mode is named"
    );
    assert!(
        go(&["import", "--authoritative", "--apply"]).is_err(),
        "a mode goes with --fleet-db"
    );
    assert!(
        import::covered::read(&dir).unwrap().is_none(),
        "a dry run records nothing"
    );
    let first = go(&["import", "--fleet-db", "--authoritative", "--apply"]).unwrap();
    assert!(first["written"].as_u64().unwrap() > 0, "{first}");
    assert_eq!(first["replica"]["covered"], json!(true), "{first}");
    assert_eq!(first["replica"]["uncovered"], json!(0), "{first}");
    let text = aterm_json::to_string(&first).unwrap();
    assert!(
        !text.contains("synthetic-pass") && !text.contains('@'),
        "{text}"
    );
    let record = import::covered::read(&dir).unwrap().unwrap();
    assert!(record.covered && record.uncovered == 0, "{record:?}");
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    handle
        .view_sealed(|view, sealer| {
            let addresses = view.project(&registry::ADDRESS);
            for n in 1..=39 {
                let pk = keys::address_pk(&format!("box{n:02}@proton.example")).unwrap();
                let token = addresses.rows[&pk]
                    .get("password")
                    .unwrap()
                    .as_str()
                    .unwrap();
                assert!(crate::typed::secret::is_sealed(token));
                let want = Secret::new(format!("synthetic-pass-{n:02}"));
                assert!(sealer.holds(registry::ADDRESS.name, &pk, "password", token, &want));
            }
            Ok(())
        })
        .unwrap();
    let again = go(&["import", "--fleet-db", "--authoritative", "--apply"]).unwrap();
    assert_eq!(again["records"].as_u64(), Some(0), "{again}");
    assert_eq!(again["replica"]["covered"], json!(true), "{again}");
    let same = go(&["import", "--fleet-db", "--fill-only"]).unwrap();
    assert_eq!(same["matches_registry"], json!(true), "{same}");
    // The owner edits two rows of the replica after its import: read fill-only, they differ by
    // key and column, and the replica is no longer covered.
    replica_fixture(paths, 2, "disabled");
    let report = go(&["import", "--fleet-db", "--fill-only", "--findings"]).unwrap();
    assert_eq!(report["matches_registry"], json!(false), "{report}");
    assert_eq!(report["findings"]["differs"], json!(2), "{report}");
    let applied = go(&["import", "--fleet-db", "--fill-only", "--apply"]).unwrap();
    assert_eq!(applied["replica"]["covered"], json!(false), "{applied}");
    assert_eq!(applied["replica"]["uncovered"], json!(2), "{applied}");
    let cells = applied["replica"]["cells"].as_array().unwrap();
    for n in 1..=2 {
        let pk = keys::address_pk(&format!("box{n:02}@proton.example")).unwrap();
        assert!(
            cells
                .iter()
                .any(|c| c["pk"] == json!(crate::att::command::safe_key(&pk))
                    && c["column"] == json!("status")
                    && c["why"] == json!("differs")),
            "{applied}"
        );
    }
    for report in [&report, &applied] {
        let text = aterm_json::to_string(report).unwrap();
        assert!(
            !text.contains("disabled") && !text.contains("synthetic-pass") && !text.contains('@'),
            "{text}"
        );
    }
    assert!(!import::covered::read(&dir).unwrap().unwrap().covered);
    let check = verb(paths, &["check"]).unwrap();
    assert_eq!(check["registry"], json!("imported"), "{check}");
}

/// A replica password shaped like a flag is covered only where it is a flag word that agrees
/// with the registry: a none word (`0`) beside an empty cell is, a has-password word (`1`) beside
/// an empty cell is not, and a short password (`pa55w`), which the registry never holds, is not:
/// the owner sees both before the replica goes, by key and column, never by value.
/// NEGATIVE CONTROL: with `flag_says` answering `Some(false)` for a value it knows no word of,
/// the short password is recorded covered:
/// `` left: Number(Number(Unsigned(1))) right: Number(Number(Unsigned(2))) ``.
#[test]
fn a_flag_shaped_password_is_covered_only_when_its_word_agrees() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let dir = replica_fixture(paths, 3, "active");
    let mut db = crate::fleet_db::open_at(&dir).unwrap();
    for (i, password) in [(1, "1"), (2, "0"), (3, "pa55w")] {
        let (email, mut row) = email_row(i, "active");
        let fields = row.as_object_mut().unwrap();
        fields.insert("password".to_owned(), json!(null));
        crate::fleet_db::put_row(&mut db, "emails", &email, row.clone()).unwrap();
        row.as_object_mut()
            .unwrap()
            .insert("password".to_owned(), json!(password));
        crate::fleet_db::put_row(&mut db, "emails", &email, row).unwrap();
    }
    drop(db);
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let applied = run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert_eq!(applied["replica"]["covered"], json!(false), "{applied}");
    assert_eq!(applied["replica"]["uncovered"], json!(2), "{applied}");
    let cells = applied["replica"]["cells"].as_array().unwrap();
    let listed = |i: usize| {
        let pk = keys::address_pk(&format!("box{i:02}@proton.example")).unwrap();
        cells.iter().any(|c| {
            c["pk"] == json!(crate::att::command::safe_key(&pk))
                && c["column"] == json!("password")
                && c["why"] == json!("secret-flag-like")
        })
    };
    assert!(listed(1) && !listed(2) && listed(3), "{applied}");
    let text = aterm_json::to_string(&applied).unwrap();
    assert!(!text.contains("pa55w") && !text.contains('@'), "{text}");
}

/// A redaction mark a previous build stored over a password says the row had one, as a sheet's
/// `1` does: the replica is not covered until the registry's cell is set.
/// NEGATIVE CONTROL: with `drop_read_fields` nulling the mark, the import reads nothing and
/// records the replica covered: `` left: Bool(true) right: Bool(false) ``.
#[test]
fn a_stored_redaction_mark_is_uncovered_until_the_registry_holds_a_password() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let dir = replica_fixture(paths, 1, "active");
    let (email, mut row) = email_row(2, "active");
    let fields = row.as_object_mut().unwrap();
    fields.insert("password".to_owned(), json!("<redacted>"));
    fields.insert("has_password".to_owned(), json!(true));
    let mut db = crate::fleet_db::open_at(&dir).unwrap();
    db.commit(crate::database::Command::Write {
        request: crate::crypto::random::<16>().unwrap(),
        changes: vec![crate::database::Change {
            collection: "emails".into(),
            key: crate::fleet_db::storage_key("emails", &email).unwrap(),
            value: Some(row),
        }],
    })
    .unwrap();
    drop(db);
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let applied = run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert_eq!(applied["replica"]["covered"], json!(false), "{applied}");
    assert_eq!(applied["replica"]["uncovered"], json!(1), "{applied}");
    assert_eq!(
        applied["replica"]["cells"][0]["column"],
        json!("password"),
        "{applied}"
    );
}

/// A row marked protected on one Mac while another stored its password: the read shows no
/// password, but the cell is listed for the owner rather than recorded covered.
/// NEGATIVE CONTROL: with the conflict judged on the read alone, `tables` counts nothing:
/// `` left: Number(Number(Unsigned(0))) right: Number(Number(Unsigned(1))) ``.
#[test]
fn a_password_beside_a_concurrent_protected_mark_is_uncovered() {
    use crate::database::{Change, Command};
    let raw = |db: &mut crate::local::Database, email: &str, row: Value| {
        db.commit(Command::Write {
            request: crate::crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: crate::fleet_db::storage_key("emails", email).unwrap(),
                value: Some(row),
            }],
        })
        .unwrap();
    };
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let dir = replica_fixture(paths, 1, "active");
    let tmp = aterm_tempfile::tempdir().unwrap();
    let secret = crate::fleet_db::resolve_fleet_secret(&dir).unwrap();
    let peer_dir = tmp.path().join("peer");
    crate::local::ensure_fleet(&peer_dir, &secret).unwrap();
    let mut peer = crate::local::Database::open(&peer_dir).unwrap();
    let mut db = crate::fleet_db::open_at(&dir).unwrap();
    let (email, row) = email_row(2, "active");
    let with = |password: Value, protected: bool| {
        let mut row = row.clone();
        let fields = row.as_object_mut().unwrap();
        fields.insert("password".to_owned(), password);
        fields.insert("protected".to_owned(), json!(protected));
        row
    };
    let carry = |from: &crate::local::Database, to: &mut crate::local::Database| {
        let wires: Vec<Vec<u8>> = from
            .history
            .index
            .inventory()
            .map(|id| from.history.wire(id).unwrap().to_vec())
            .collect();
        for wire in wires {
            to.ingest(&wire).unwrap();
        }
    };
    raw(&mut db, &email, with(json!(null), false));
    carry(&db, &mut peer);
    raw(&mut db, &email, with(json!("synthetic-pass-02"), false));
    raw(&mut peer, &email, with(json!(null), true));
    carry(&peer, &mut db);
    let read = crate::fleet_db::get_row(&db.view(), "emails", &email)
        .unwrap()
        .unwrap();
    assert_eq!(read["password"], json!(null), "a protected row shows none");
    let summary = crate::fleet_db::tables_summary(&db).unwrap();
    assert_eq!(summary["conflicts"], json!(1), "{summary}");
    drop(db);
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let applied = run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert_eq!(applied["replica"]["covered"], json!(false), "{applied}");
    let text = aterm_json::to_string(&applied).unwrap();
    assert!(!text.contains("synthetic-pass"), "{text}");
}

/// Two Macs disagree on a row's notes, and their empty passwords are spelled differently: the
/// notes are reported, and the password is not.
/// NEGATIVE CONTROL: with `dump_sheets` comparing a secret column's stored spelling, the password
/// is reported beside the notes: `` left: ["notes", "password"] right: ["notes"] ``.
#[test]
fn a_spelling_is_not_reported_beside_a_column_that_really_disagrees() {
    use crate::database::{Change, Command};
    let raw = |db: &mut crate::local::Database, email: &str, row: Value| {
        db.commit(Command::Write {
            request: crate::crypto::random::<16>().unwrap(),
            changes: vec![Change {
                collection: "emails".into(),
                key: crate::fleet_db::storage_key("emails", email).unwrap(),
                value: Some(row),
            }],
        })
        .unwrap();
    };
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let dir = replica_fixture(paths, 1, "active");
    let tmp = aterm_tempfile::tempdir().unwrap();
    let secret = crate::fleet_db::resolve_fleet_secret(&dir).unwrap();
    let peer_dir = tmp.path().join("peer");
    crate::local::ensure_fleet(&peer_dir, &secret).unwrap();
    let mut peer = crate::local::Database::open(&peer_dir).unwrap();
    let mut db = crate::fleet_db::open_at(&dir).unwrap();
    let (email, row) = email_row(2, "active");
    let with = |password: Value, notes: &str| {
        let mut row = row.clone();
        let fields = row.as_object_mut().unwrap();
        fields.insert("password".to_owned(), password);
        fields.insert("notes".to_owned(), json!(notes));
        row
    };
    let carry = |from: &crate::local::Database, to: &mut crate::local::Database| {
        let wires: Vec<Vec<u8>> = from
            .history
            .index
            .inventory()
            .map(|id| from.history.wire(id).unwrap().to_vec())
            .collect();
        for wire in wires {
            to.ingest(&wire).unwrap();
        }
    };
    raw(&mut db, &email, with(json!(null), "first"));
    carry(&db, &mut peer);
    raw(&mut db, &email, with(json!("0"), "one Mac"));
    raw(&mut peer, &email, with(json!(null), "the other Mac"));
    carry(&peer, &mut db);
    drop(db);
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let applied = run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    let reported: Vec<&str> = applied["replica"]["cells"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|cell| cell["why"] == json!("dump-conflicted"))
        .map(|cell| cell["column"].as_str().unwrap())
        .collect();
    assert_eq!(reported, ["notes"], "{applied}");
}

/// fleet_db's write verbs on this Mac's replica work before its import and refuse, pointing to
/// `aterm db emails`, once its own import recorded it covered; its reads still work. A record
/// that says uncovered refuses nothing.
/// NEGATIVE CONTROL: with `refuse_writes` returning `Ok(())`, the covered replica takes the
/// `put`: `` called `Result::unwrap_err()` on an `Ok` value ``.
#[test]
fn fleet_db_writes_refuse_once_the_replica_is_covered_and_work_before() {
    use crate::alab::import::covered;
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let dir = replica_fixture(paths, 2, "active");
    let fleet = |list: &[&str]| {
        let dir = dir.clone();
        crate::fleet_db::try_run_with(&words(list), move || Ok(dir))
    };
    let (email, row) = email_row(3, "active");
    let row = aterm_json::to_string(&row).unwrap();
    assert!(fleet(&["put", "emails", &email, &row]).unwrap().is_some());
    let uncovered = covered::Record {
        covered: false,
        uncovered: 1,
        digest: None,
        epoch: None,
        at_ms: NOW,
    };
    covered::write(&dir, &uncovered, &[]).unwrap();
    assert!(fleet(&["delete", "emails", &email]).unwrap().is_some());
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let applied = run(
        paths,
        &words(&["import", "--fleet-db", "--authoritative", "--apply"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert_eq!(applied["replica"]["covered"], json!(true), "{applied}");
    let snapshot = dir.parent().unwrap().join("snapshot.json");
    std::fs::write(&snapshot, b"{}").unwrap();
    let dir_word = dir.to_str().unwrap();
    for refused in [
        fleet(&["put", "emails", &email, &row]),
        fleet(&["delete", "emails", "box01@proton.example"]),
        fleet(&["load", snapshot.to_str().unwrap()]),
        fleet(&[
            "delete",
            "emails",
            "box01@proton.example",
            "--dir",
            dir_word,
        ]),
        fleet(&["rekey"]),
        fleet(&["bootstrap"]),
    ] {
        let refused = refused.unwrap_err();
        assert!(refused.to_string().contains("aterm db emails"), "{refused}");
    }
    let read = fleet(&["get", "emails", "box01@proton.example"])
        .unwrap()
        .unwrap();
    assert_eq!(read["password"], json!("<redacted>"), "{read}");
}

/// `import --fleet-db --digest` prints the replica's keyed digest under the newest `[everyone]`
/// epoch held here, naming that epoch and the row counts and no value; it is stable across runs,
/// moves when a cell does, moves with the epoch, and writes nothing. With no `[everyone]` key it
/// refuses. The registry's views digest the same way (`aterm db emails --digest`).
/// NEGATIVE CONTROL: with `replica_digest` keyed by a constant in place of the epoch key, the
/// digests under two epochs agree: `` assertion `left != right` failed ``.
#[test]
fn a_replica_digest_is_stable_and_names_its_epoch() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let dir = replica_fixture(paths, 3, "active");
    EVERYONE.with(|held| held.set(None));
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let mut go = |list: &[&str]| run(paths, &words(list), Some(&Stand), &mut never, NOW);
    let none = go(&["import", "--fleet-db", "--digest"]).unwrap_err();
    assert!(none.to_string().contains("[everyone]"), "{none}");
    // Nor is the dump imported: its source would be a plain digest of its passwords.
    let unkeyed = go(&["import", "--fleet-db", "--fill-only"]).unwrap_err();
    assert!(unkeyed.to_string().contains("[everyone]"), "{unkeyed}");
    let epoch = [0xab; 32];
    EVERYONE.with(|held| held.set(Some((epoch, [0x11; 32]))));
    let first = go(&["import", "--fleet-db", "--digest"]).unwrap();
    let second = go(&["import", "--fleet-db", "--digest"]).unwrap();
    assert_eq!(first, second);
    assert_eq!(first["epoch"], json!("abababab"), "{first}");
    assert_eq!(first["rows"]["emails"], json!(3), "{first}");
    let digest = first["digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64);
    let text = aterm_json::to_string(&first).unwrap();
    assert!(
        !text.contains("synthetic-pass") && !text.contains('@'),
        "{text}"
    );
    assert!(
        import::covered::read(&dir).unwrap().is_none(),
        "the digest records nothing"
    );
    assert!(
        go(&["import", "--fleet-db", "--digest", "--apply"]).is_err(),
        "--digest imports nothing"
    );
    EVERYONE.with(|held| held.set(Some(([0xcd; 32], [0x22; 32]))));
    let rotated = go(&["import", "--fleet-db", "--digest"]).unwrap();
    assert_eq!(rotated["epoch"], json!("cdcdcdcd"));
    assert_ne!(rotated["digest"], first["digest"]);
    EVERYONE.with(|held| held.set(Some((epoch, [0x11; 32]))));
    let (email, mut row) = email_row(2, "active");
    row.as_object_mut()
        .unwrap()
        .insert("password".to_owned(), json!("synthetic-pass-changed"));
    let mut db = crate::fleet_db::open_at(&dir).unwrap();
    crate::fleet_db::put_row(&mut db, "emails", &email, row).unwrap();
    drop(db);
    let moved = go(&["import", "--fleet-db", "--digest"]).unwrap();
    assert_eq!(moved["epoch"], first["epoch"]);
    assert_ne!(moved["digest"], first["digest"]);
    EVERYONE.with(|held| held.set(None));
}

/// `import --compare-sheet` reports what the sheet would do and how its password flags meet the
/// registry's sealed passwords, by count, key and column, and writes nothing: no address, no
/// cell value and no flag's text is in the report.
/// NEGATIVE CONTROL: with `differs_json` adding each finding's `at` coordinates' cell text
/// (`sheets[0].text(..)`), the report holds the sheet's provider word: `` assertion failed:
/// !text.contains("gmail") ``.
#[test]
fn compare_sheet_prints_no_values() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    replica_fixture(paths, 3, "active");
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let mut go = |list: &[&str]| run(paths, &words(list), Some(&Stand), &mut never, NOW);
    go(&["import", "--fleet-db", "--authoritative", "--apply"]).unwrap();
    let sheet = scratch.path().join("alab-emails.csv");
    std::fs::write(
        &sheet,
        "Email Address,Root Account,Password,Status,Provider,Activation Phone Number,Protection Flag,Notes\n\
         box01@proton.example,,1,Active,gmail,,,\n\
         box02@proton.example,,1,Active,proton,,,\n\
         new@example.invalid,,1,Active,gmail,,,a note\n",
    )
    .unwrap();
    let before = Registry::at(paths.clone()).view().unwrap().ops.len();
    let report = go(&["import", "--compare-sheet", sheet.to_str().unwrap()]).unwrap();
    assert_eq!(
        Registry::at(paths.clone()).view().unwrap().ops.len(),
        before
    );
    assert_eq!(report["kind"], json!("emails-csv"));
    assert_eq!(report["matches_registry"], json!(false));
    assert_eq!(
        report["password_flags"],
        json!({"set": 3, "sealed_in_registry": 2, "missing_in_registry": 1, "no_address": 0,
            "sealed_unflagged": 1})
    );
    let box01 = keys::address_pk("box01@proton.example").unwrap();
    assert!(
        report["differs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["pk"] == json!(crate::att::command::safe_key(&box01))
                && d["column"] == json!("provider")),
        "{report}"
    );
    let text = aterm_json::to_string(&report).unwrap();
    for value in ["gmail", "@", "a note", "Active", "synthetic-pass"] {
        assert!(!text.contains(value), "{value}: {text}");
    }
    assert!(
        go(&[
            "import",
            "--compare-sheet",
            sheet.to_str().unwrap(),
            "--apply"
        ])
        .is_err()
    );
}

/// A dry run on a Mac that does not hold the registry plans its replica's passwords against an empty
/// registry, each sealed to a token no key opens, rather than refusing: the report says "not held
/// here" and shows no value.
/// NEGATIVE CONTROL: with `Unheld::seal` returning the mask, the planned Secret cell fails
/// validation: `` called `Result::unwrap()` on an `Err` value: … authenticated opaque token ``.
#[test]
fn a_dry_run_without_the_registry_plans_a_replicas_passwords() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    replica_fixture(paths, 3, "active");
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let report = run(
        paths,
        &words(&["import", "--fleet-db", "--fill-only"]),
        Some(&Stand),
        &mut never,
        NOW,
    )
    .unwrap();
    assert_eq!(report["registry"], json!("not held here"), "{report}");
    let text = aterm_json::to_string(&report).unwrap();
    assert!(
        !text.contains("synthetic-pass") && !text.contains('@'),
        "{text}"
    );
}

/// The local review never shows a phone number: a number is a `Secret` cell's value.
/// NEGATIVE CONTROL: with `review_text` skipping `guard::phone_shaped`, the number shows: ``
/// left: "+1 (202) 555-0149" right: "(withheld: shaped like a secret)" ``.
#[test]
fn a_review_never_shows_a_phone_number() {
    assert_eq!(review_text("+1 (202) 555-0149", true), WITHHELD);
    assert_eq!(review_text("desk line", true), "desk line");
}

/// `import --mailbox-toml <file> --apply` on a real lab lands the harness's mailbox file and says
/// whether it is covered: every field of a synthetic file lands (`covered`, sealed, no value or
/// address in the report), and once the file holds a field with no column the report names that
/// field by its name alone and the file is not covered. `--mailbox-toml` goes with no
/// `--digest` or `--compare-sheet`.
/// NEGATIVE CONTROL: with `import` never inserting `mailbox_toml`, the report has no coverage:
/// `` assertion `left == right` failed left: Null right: Bool(true) ``.
#[test]
fn a_mailbox_file_imports_and_reports_its_coverage() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let scratch = aterm_tempfile::tempdir().unwrap();
    let file = scratch.path().join("ai-mailbox.toml");
    let body = "user = \"owner@proton.example\"\npassword = \"synthetic-pass-21\"\n\
                totp_secret = \"JBSWY3DPEHPK3PXP\"\n\
                [[mailboxes]]\nlogin = \"ann@example.invalid\"\npassword = \"synthetic-pass-22\"\n\
                receives = [\"ai1@example.invalid\", \"*@pm.example\"]\n";
    std::fs::write(&file, body).unwrap();
    let mut never = |_: &Path| -> Result<()> { panic!("no review") };
    let mut go = |list: &[&str]| run(paths, &words(list), Some(&Stand), &mut never, NOW);
    let path = file.to_str().unwrap();
    let applied = go(&["import", "--mailbox-toml", path, "--apply"]).unwrap();
    assert_eq!(applied["mailbox_toml"]["covered"], json!(true), "{applied}");
    assert_eq!(applied["mailbox_toml"]["uncovered"], json!(0), "{applied}");
    let text = aterm_json::to_string(&applied).unwrap();
    assert!(
        !text.contains("synthetic-pass") && !text.contains('@'),
        "{text}"
    );
    let view = Registry::at(paths.clone()).view().unwrap();
    let ann = keys::mailbox_pk(&keys::address_pk("ann@example.invalid").unwrap()).unwrap();
    assert_eq!(
        view.project(&MAILBOX).rows[&ann].get("receives_domains"),
        Some(&json!(["pm.example"]))
    );
    let again = go(&["import", "--mailbox-toml", path, "--apply"]).unwrap();
    assert_eq!(again["records"].as_u64(), Some(0), "{again}");
    std::fs::write(
        &file,
        format!("bearer_token = \"synthetic-token-23\"\n{body}"),
    )
    .unwrap();
    let applied = go(&["import", "--mailbox-toml", path, "--apply"]).unwrap();
    assert_eq!(
        applied["mailbox_toml"]["covered"],
        json!(false),
        "{applied}"
    );
    let cells = applied["mailbox_toml"]["cells"].as_array().unwrap();
    assert!(
        cells
            .iter()
            .any(|c| c["why"] == json!("unmapped") && c["column"] == json!("bearer_token")),
        "{applied}"
    );
    assert!(
        !aterm_json::to_string(&applied)
            .unwrap()
            .contains("synthetic-token")
    );
    for refused in [
        &["import", "--mailbox-toml", path, "--fleet-db", "--digest"][..],
        &["import", "--mailbox-toml", path, "--compare-sheet", path],
    ] {
        assert!(go(refused).is_err(), "{refused:?}");
    }
}

/// A sheet flag's path names a workbook's tab as `<file>.xlsx#<tab>`, or the flag's own tab for a
/// bare `<file>.xlsx`; anything else, a CSV named with a `#` included, is read as a CSV.
#[test]
fn a_sheet_path_names_a_workbook_tab() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let hashed = scratch.path().join("a#b.csv");
    std::fs::write(&hashed, "Email Address\n").unwrap();
    assert_eq!(workbook(&hashed, SheetKind::Emails), None);
    assert_eq!(
        workbook(
            Path::new("/x/AI Email Accounting.xlsx#Emails"),
            SheetKind::Phones
        ),
        Some((
            PathBuf::from("/x/AI Email Accounting.xlsx"),
            "Emails".to_owned()
        ))
    );
    assert_eq!(
        workbook(Path::new("/x/book.XLSX"), SheetKind::Computers),
        Some((PathBuf::from("/x/book.XLSX"), "Computers".to_owned()))
    );
    assert_eq!(workbook(Path::new("/x/sheet.csv"), SheetKind::Emails), None);
    assert_eq!(
        workbook(Path::new("/x/book.xlsx#"), SheetKind::Emails),
        None
    );
}

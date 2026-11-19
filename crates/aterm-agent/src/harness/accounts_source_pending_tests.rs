// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
#[cfg(unix)]
fn a_configured_key_excludes_default_pool_beside_named_roster() {
    use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
    for key in [false, true] {
        let bed = Bed::new("source-selection-key-pool");
        let _registry = TestSourceOverride::new(SourceSnapshot::Absent);
        let config = format!(
            "[fleet]\nmachine='m16'\n{}",
            if key {
                "accounts_source='unused.csv'\n"
            } else {
                ""
            }
        );
        fs::write(bed.aterm_toml(), config).unwrap();
        fs::write(bed.config.join(FLEET_ROSTER_FILE), "opt_in='named'\n[[account]]\nlabel='chosen'\nemail='chosen@example.test'\nmachines=['m16']\n").unwrap();
        bed.secret(super::super::super::ai_pool::AI_POOL_FILE, "invalid pool");
        let roster = bed.roster(Cli::Claude);
        let origin = fleet_origin(
            &accounts::Store::at(&bed.state),
            Some(&bed.home),
            Some(&bed.aterm_toml()),
            &roster,
        );
        let result =
            sync_accounts_for_init_with(&bed.home, &bed.state, &roster, NOW, || {}, &|_, _| {
                panic!("local input is captured")
            });
        if key {
            assert!(matches!(origin, FleetOrigin::Local { .. }));
            assert_eq!(
                result
                    .expect("init must read the same named source as display")
                    .rows,
                1
            );
            assert_eq!(
                Roster::load(&roster).unwrap().rows[0].email.as_deref(),
                Some("chosen@example.test")
            );
            assert!(!needs_init_accounts_sync(
                &bed.home,
                &bed.state,
                &roster,
                NOW + 1
            ));
        } else {
            assert!(matches!(origin, FleetOrigin::Vault(_)));
            assert!(
                result.is_err(),
                "the default pool remains selected without a configured key"
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn empty_pool_checks_the_captured_source_before_publishing() {
    use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
    for changed in [false, true] {
        let bed = Bed::new("empty-pool-source-capture");
        let original = "version=1\naccount=[]\n";
        bed.secret(super::super::super::ai_pool::AI_POOL_FILE, original);
        let _source = TestSourceOverride::new(SourceSnapshot::Absent);
        let roster = bed.roster(Cli::Claude);
        let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
        let store = env.accounts_store();
        let paths = [
            roster.clone(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            store.machine_file(),
            bed.config.join(FLEET_ROSTER_FILE),
        ];
        let images = || {
            paths
                .iter()
                .map(|path| fs::read(path).ok())
                .collect::<Vec<_>>()
        };
        let before = images();
        let source = super::super::super::ai_pool::AI_POOL_FILE;
        let called = std::cell::Cell::new(false);
        let result = sync_accounts_for_init_with(
            &bed.home,
            &bed.state,
            &roster,
            NOW,
            || {
                called.set(true);
                if changed {
                    bed.secret(source, &format!("{original}# changed after capture\n"));
                }
            },
            &|_, _| panic!("the captured local pool must not fetch"),
        );
        assert!(called.get());
        if changed {
            let error = result.unwrap_err();
            assert_eq!(error.retry_reason(), Some(Retry::Changed), "{error}");
            assert_eq!(
                images(),
                before,
                "a stale empty pool cannot publish authority"
            );
            assert_eq!(
                crate::vault_entry::read_text(&bed.home, source, 1 << 20).text(),
                Some(format!("{original}# changed after capture\n").as_str())
            );
        } else {
            assert!(
                result.unwrap().changed,
                "the unchanged empty pool publishes its projection"
            );
            assert!(bed.config.join(FLEET_ROSTER_FILE).is_file());
        }
    }
}

#[test]
fn automatic_configured_fetch_rechecks_admission_before_publication() {
    use crate::harness::alab_roster::{SourceSnapshot, TestSourceOverride};
    for invalidate_config in [false, true] {
        let bed = Bed::new("configured-fetch-admission");
        let roster = bed.roster(Cli::Claude);
        let store = bed.seed_records(Cli::Claude);
        aterm_uds::atomic_file::write(
            &bed.aterm_toml(),
            b"[fleet]\nmachine='m16'\naccounts_source='https://example.invalid/accounts.toml'\n",
            0o600,
        )
        .unwrap();
        let source = TestSourceOverride::new(SourceSnapshot::Absent);
        let paths = [
            roster.clone(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            bed.config.join(FLEET_ROSTER_FILE),
        ];
        let image = || {
            paths
                .iter()
                .map(|path| fs::read(path).ok())
                .collect::<Vec<_>>()
        };
        let before = image();
        let fetched = std::sync::atomic::AtomicBool::new(false);
        let fetch = |_: &Env, _: &str| {
            fetched.store(true, std::sync::atomic::Ordering::SeqCst);
            if invalidate_config {
                aterm_uds::atomic_file::write(
                    &bed.aterm_toml(),
                    b"[fleet]\nmachine='m16'\n[harness]\nenabled=false\n",
                    0o600,
                )
                .unwrap();
            } else {
                source.set(SourceSnapshot::Pending);
            }
            Ok(
                "[[account]]\nlabel='legacy'\nemail='legacy@example.test'\nmachines=['m16']\n"
                    .to_owned(),
            )
        };
        let error = sync_accounts_for_init_with(&bed.home, &bed.state, &roster, NOW, || {}, &fetch)
            .unwrap_err();
        assert!(fetched.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            error.to_string().contains("changed after capture"),
            "{error}"
        );
        assert_eq!(image(), before);
    }
}

#[test]
#[cfg(unix)]
fn captured_legacy_source_refuses_a_new_registry_decision() {
    use crate::harness::alab_roster::{Lab, SourceSnapshot, TestSourceOverride, test_machine};
    let bound = SourceSnapshot::Present(Arc::new(Lab {
        me: Some(test_machine("m16")),
        ..Lab::default()
    }));
    for next in [SourceSnapshot::Pending, bound, SourceSnapshot::Disabled] {
        let bed = Bed::new("changed-source-capture");
        let roster = bed.roster(Cli::Claude);
        let store = bed.seed_records(Cli::Claude);
        bed.secret(
            FLEET_ROSTER_FILE,
            "[[account]]\nlabel='legacy'\nemail='legacy@example.test'\nmachines=['m16']\n",
        );
        let source = TestSourceOverride::new(SourceSnapshot::Absent);
        let paths = [
            roster.clone(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            bed.config.join(FLEET_ROSTER_FILE),
        ];
        let image = || {
            paths
                .iter()
                .map(|path| fs::read(path).ok())
                .collect::<Vec<_>>()
        };
        let before = image();
        let result = sync_accounts_for_init_with(
            &bed.home,
            &bed.state,
            &roster,
            NOW,
            || source.set(next.clone()),
            &|_, _| panic!("captured input must not fetch another source"),
        );
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("changed after capture"),
            "{error}"
        );
        assert_eq!(
            image(),
            before,
            "new registry state cannot authorize a legacy capture"
        );
    }
}

#[test]
fn equivalent_registry_refresh_does_not_cancel_automatic_sync() {
    use crate::harness::alab_roster::{Lab, SourceSnapshot, TestSourceOverride, test_machine};
    let bed = Bed::new("equivalent-source-refresh");
    let roster = bed.roster(Cli::Claude);
    bed.seed_records(Cli::Claude);
    let lab = Lab {
        me: Some(test_machine("m16")),
        ..Lab::default()
    };
    let source = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(lab.clone())));
    let result = sync_accounts_for_init_with(
        &bed.home,
        &bed.state,
        &roster,
        NOW,
        || source.set(SourceSnapshot::Present(Arc::new(lab.clone()))),
        &|_, _| panic!("an equivalent native source must not read legacy input"),
    );
    result.expect("equivalent immutable data still authorizes the same sync");
    assert!(bed.rows(Cli::Claude).iter().any(|row| row.label == "kept"));
}

#[test]
#[cfg(unix)]
fn automatic_source_capture_cannot_outlive_a_changed_configuration() {
    for changed in [false, true] {
        let bed = Bed::new("source-capture-configuration");
        let roster = bed.roster(Cli::Claude);
        let store = bed.seed_records(Cli::Claude);
        bed.secret(
            FLEET_ROSTER_FILE,
            "[[account]]\nlabel='legacy'\nemail='legacy@example.test'\nmachines=['m16']\n",
        );
        let paths = [
            roster.clone(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
            store.machine_file(),
            bed.config.join(FLEET_ROSTER_FILE),
        ];
        // The files, and the fleet roster entry the capture read, as this Mac's store holds it.
        let retained = || {
            let mut held = paths
                .iter()
                .map(|path| fs::read(path).ok())
                .collect::<Vec<_>>();
            held.push(
                crate::vault_entry::read_text(&bed.home, FLEET_ROSTER_FILE, 1 << 20)
                    .text()
                    .map(|text| text.as_bytes().to_vec()),
            );
            held
        };
        let before = retained();
        assert!(
            before.last().is_some_and(Option::is_some),
            "PRECONDITION: the store holds the fleet roster entry"
        );
        let result = sync_accounts_for_init_with(
            &bed.home,
            &bed.state,
            &roster,
            NOW,
            || {
                if changed {
                    aterm_uds::atomic_file::write(
                        &bed.aterm_toml(),
                        b"[fleet]\nmachine = 'm16'\n[harness]\nenabled = false\n",
                        0o600,
                    )
                    .unwrap();
                }
            },
            &|_, _| panic!("captured source must not fetch another input"),
        );
        if changed {
            result.expect_err("a disabled automatic sync must discard its captured source");
            assert_eq!(
                retained(),
                before,
                "a refused capture must preserve account authority"
            );
        } else {
            result.expect("an unchanged admitted capture must still sync");
            assert!(
                bed.rows(Cli::Claude)
                    .iter()
                    .any(|row| row.label == "legacy")
            );
        }
    }
}

/// Each of `paths` with its bytes, `None` where it is absent.
fn images(paths: &[PathBuf]) -> Vec<Option<Vec<u8>>> {
    paths.iter().map(|path| fs::read(path).ok()).collect()
}

/// How a test's sync reads: never a fetch, no lock ladder.
fn quiet_how<'a>(fetch: &'a Fetch<'a>) -> SyncHow<'a> {
    SyncHow {
        fetch,
        ladder: &[],
        fleet_snapshots: &[],
        heal: None,
        allow_empty_pool: true,
    }
}

/// A SYNC WHILE THE LAB IS NOT READ YET GOES ON AND READS NO OTHER SOURCE: init's, the upkeep's and
/// `accounts sync`'s, with no source named and with `alab` named, each return Ok and leave every
/// row, record and fleet file as it stood, though the vault holds a fleet roster and a sheet that
/// name an account for this Mac and a pool that does not parse, so a read of any of them adds a
/// row or fails the sync. `accounts sync` says why in one status line and names no fleet file.
/// NEGATIVE CONTROL: with `chosen_captured`'s `Pending` arm the refusal it replaced, every entry
/// fails (`a pending lab goes on`).
#[test]
#[cfg(unix)]
fn a_pending_lab_syncs_with_every_row_kept_and_reads_no_source() {
    use crate::harness::alab_roster::{SOURCE, while_pending};
    let bed = Bed::new("pending-source");
    let roster = bed.roster(Cli::Claude);
    let store = bed.seed_records(Cli::Claude);
    let fleet = "[[account]]\nlabel='legacy'\nemail='legacy@example.test'\nmachines=['m16']\n";
    bed.secret(FLEET_ROSTER_FILE, fleet);
    bed.secret(
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\nsheet@example.test,m16,1\n",
    );
    bed.secret(super::super::super::ai_pool::AI_POOL_FILE, "invalid pool");
    let paths = [
        roster.clone(),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
        store.machine_file(),
        bed.config.join(FLEET_ROSTER_FILE),
    ];
    let before = images(&paths);
    let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
    let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("a pending lab fetched") };
    let how = quiet_how(&fetch);
    while_pending(|| {
        assert!(has_init_accounts_source(
            &bed.home, &bed.state, &roster, NOW
        ));
        assert!(
            !needs_init_accounts_sync(&bed.home, &bed.state, &roster, NOW),
            "nothing is due while the lab is unread"
        );
        assert!(matches!(
            fleet_origin(&store, Some(&bed.home), Some(&bed.aterm_toml()), &roster),
            FleetOrigin::AlabPending(_)
        ));
        let called = std::cell::Cell::new(false);
        let init = sync_accounts_for_init_with(
            &bed.home,
            &bed.state,
            &roster,
            NOW,
            || called.set(true),
            &fetch,
        )
        .expect("a pending lab goes on");
        assert!(called.get());
        assert_eq!((init.rows, init.changed), (1, false));
        for source in [None, Some(SOURCE)] {
            let report = sync_accounts_with(&env, source, None, &how)
                .map_err(SyncError::into_message)
                .expect("a pending lab goes on");
            assert!(report.publication.pending.is_some(), "{source:?}");
            assert!(!report.publication.changed, "{source:?}");
        }
        for json in [false, true] {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_accounts_sync(&env, None, None, None, json, &mut out, &mut err);
            assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
            let out = String::from_utf8(out).unwrap();
            if json {
                let doc: Value = aterm_json::from_str(&out).unwrap();
                assert!(
                    doc["lab_pending"]
                        .as_str()
                        .is_some_and(|why| why.contains("not read yet")),
                    "{out}"
                );
                assert_eq!(doc["fleet_roster"], Value::Null, "{out}");
            } else {
                assert_eq!(
                    out.lines()
                        .filter(|line| line.contains("not read yet"))
                        .count(),
                    1,
                    "{out}"
                );
                assert!(
                    out.starts_with(
                        "synced this Mac's own roster (1 accounts on this machine; no account \
                         changes): "
                    ),
                    "{out}"
                );
                assert!(!out.contains("fleet:"), "{out}");
            }
        }
    });
    assert_eq!(
        images(&paths),
        before,
        "every row, record and fleet file stands"
    );
    assert_eq!(
        crate::vault_entry::read_text(&bed.home, FLEET_ROSTER_FILE, SOURCE_TEXT_MAX_BYTES),
        crate::vault_entry::Read::Present(fleet.to_owned())
    );
}

/// A CLAUDE SYNC WHILE THE LAB IS NOT READ YET WRITES NOTHING, AND THE LAB'S OWN SYNC LISTS THE
/// DEFAULT LOGIN BY ITS RULES ONCE READ: with this Mac's default login naming an account no row
/// holds, and records retained under `main` (a row removed before), a pending sync succeeds, keeps
/// the roster's bytes, every record and every login directory, adds no row, and calls for no
/// further sync. Once the lab is read, an account it bars (not joinable: paused, retired) stays
/// out of the roster, and one that may join, preferred on another Mac, is listed under the lab's
/// label; the row the lab does not know stays as it was.
/// NEGATIVE CONTROL: with the removed `pending_default_row` back, the pending sync fails on the
/// records retained under `main` (or, with none, lists `main` for the barred account, which the
/// lab's sync then keeps: `the lab bars it`).
#[test]
fn a_pending_sync_writes_nothing_and_the_lab_lists_the_default_login_by_its_rules() {
    use crate::harness::alab_roster::{
        Lab, LabAccount, SourceSnapshot, TestSourceOverride, test_machine, while_pending,
    };
    for joinable in [false, true] {
        let bed = Bed::new("pending-default-login");
        let roster = bed.roster(Cli::Claude);
        let store = accounts::Store::at_for(&bed.state, Cli::Claude, &bed.home);
        fs::write(
            bed.home.join(".claude.json"),
            "{\"oauthAccount\":{\"emailAddress\":\"own@example.test\"}}\n",
        )
        .unwrap();
        let work = store.logins_dir().join("work");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("login"), b"work").unwrap();
        let standing = format!(
            "[[account]]\nlabel = \"work\"\nclass = \"dedicated\"\ndir = {}\n\
             email = \"work@example.test\"\n",
            crate::harness::host_path::toml(&work)
        );
        fs::write(&roster, &standing).unwrap();
        let (mut state, _) = store.load(NOW).unwrap();
        state.previous = Some(ACCOUNTS_OWN_LABEL.to_owned());
        store.save(&state).unwrap();
        let paths = [
            roster.clone(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        let before = images(&paths);
        let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source is fetched") };
        let how = quiet_how(&fetch);
        let sync = || sync_accounts_with(&env, None, None, &how).map_err(SyncError::into_message);
        let report = while_pending(sync).expect("a pending lab goes on");
        assert!(!report.publication.changed, "{joinable}");
        assert_eq!(images(&paths), before, "nothing is written");
        assert!(
            !store.logins_dir().join(ACCOUNTS_OWN_LABEL).exists(),
            "no login directory is made"
        );
        assert!(
            !while_pending(|| needs_init_accounts_sync(&bed.home, &bed.state, &roster, NOW)),
            "no sync is due while the lab is unread"
        );

        let _lab = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(Lab {
            me: Some(test_machine("m16")),
            accounts: vec![LabAccount {
                pk: "ac-0000000000000001".to_owned(),
                cli: Cli::Claude,
                vendor: accounts::Vendor::default(),
                label: "claude-oooooo".to_owned(),
                legacy_label: None,
                email: "own@example.test".to_owned(),
                identity: None,
                browser: None,
                joinable,
                preferred: [(test_machine("m3"), Some(1))].into_iter().collect(),
            }],
            ..Lab::default()
        })));
        sync().expect("the lab's sync goes on");
        let rows = bed.rows(Cli::Claude);
        let own: Vec<&str> = rows
            .iter()
            .filter(|row| row.matches_email("own@example.test"))
            .map(|row| row.label.as_str())
            .collect();
        if joinable {
            assert_eq!(
                own,
                ["claude-oooooo"],
                "listed under the lab's label: {rows:?}"
            );
        } else {
            assert!(own.is_empty(), "the lab bars it: {rows:?}");
        }
        assert!(
            rows.iter()
                .any(|row| row.label == "work" && row.dir.as_deref() == work.to_str()),
            "the row the lab does not know stays: {rows:?}"
        );
        assert_eq!(fs::read(work.join("login")).unwrap(), b"work");
    }
}

/// ANOTHER CLI'S SYNC WHILE THE LAB IS NOT READ YET GOES ON TOO: it keeps the roster as it stands,
/// reads neither that CLI's vault fleet nor the sheet nor a pool that does not parse, returns
/// [`CliSync::LabPending`], and its command exits 0 with one status line saying why.
/// NEGATIVE CONTROL: with the old refusal back in `ProviderSource::capture`, the sync is an error.
#[test]
#[cfg(unix)]
fn another_clis_sync_goes_on_with_its_roster_while_the_lab_is_unread() {
    use crate::harness::alab_roster::while_pending;
    for cli in OTHER_CLIS {
        let bed = Bed::new("pending-cli");
        keep_a_row_with_no_login(&bed, cli);
        bed.secret(
            &cli_fleet_file(cli),
            "[[account]]\nlabel='fleet'\nemail='fleet@example.test'\nmachines=['m16']\n",
        );
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &format!("email,computer,{}\nsheet@example.test,m16,1\n", cli.id()),
        );
        bed.secret(super::super::super::ai_pool::AI_POOL_FILE, "invalid pool");
        let before = Bed::tree(&bed.config);
        let synced = while_pending(|| bed.sync(cli, None));
        assert!(
            matches!(&synced, Ok(CliSync::LabPending { why, .. }) if why.contains("not read yet")),
            "{cli:?}: {synced:?}"
        );
        assert_eq!(Bed::tree(&bed.config), before, "{cli:?}: the roster stands");
        let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source is fetched") };
        let code = while_pending(|| {
            run_cli_accounts_sync_with(
                cli,
                (&env, None),
                (None, None, false),
                (&mut out, &mut err),
                &fetch,
            )
        });
        let out = String::from_utf8(out).unwrap();
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "{cli:?}: {}",
            String::from_utf8_lossy(&err)
        );
        assert_eq!(
            out.lines()
                .filter(|line| line.contains("not read yet"))
                .count(),
            1,
            "{cli:?}: {out}"
        );
        assert_eq!(Bed::tree(&bed.config), before, "{cli:?}: the roster stands");
    }
}

/// A LAB NOT READ YET ADMITS NO NATIVE RECEIPT, THOUGH ITS SYNC GOES ON: a capture while pending
/// chooses [`Chosen::Pending`], holds no receipt guard (`Retry::Unavailable`), and answers no lab
/// row for a name (`named_account`, which `accounts add` asks; NEGATIVE CONTROL: its deleted
/// pending refusal back fails the name). A lab turned off (`[alab] roster = false`) is still no
/// source and no pending one: a sync with no source named reads what it always read, and naming
/// `alab` is refused.
#[test]
fn a_pending_lab_admits_no_native_receipt_and_a_lab_turned_off_stays_off() {
    use crate::harness::alab_roster::{SOURCE, SourceSnapshot, TestSourceOverride, while_pending};
    let bed = Bed::new("pending-receipt");
    let config = bed.aterm_toml();
    let roster = bed.roster(Cli::Claude);
    while_pending(|| {
        let captured = AutomaticSource::capture(&config).unwrap();
        let refused = captured.native_guard().unwrap_err();
        assert_eq!(
            refused.retry_reason(),
            Some(Retry::Unavailable),
            "{refused}"
        );
        assert!(matches!(
            captured.chosen(Cli::Claude, &roster, None),
            Ok(Chosen::Pending(_))
        ));
        // No lab row is asked for by name meanwhile: a name is no lab account's, and refuses
        // nothing (the deleted refusal: "the native account source is unavailable").
        for name in ["work-1", "claude-abcdef"] {
            assert_eq!(
                captured.named_account(Cli::Claude, name),
                Ok(None),
                "{name}"
            );
        }
    });
    let _off = TestSourceOverride::new(SourceSnapshot::Disabled);
    let captured = AutomaticSource::capture(&config).unwrap();
    assert!(matches!(
        captured.chosen(Cli::Claude, &roster, None),
        Ok(Chosen::Unnamed)
    ));
    assert!(
        captured
            .chosen(Cli::Claude, &roster, Some(SOURCE))
            .is_err_and(|why| why.to_string().contains("turns the lab source off"))
    );
}

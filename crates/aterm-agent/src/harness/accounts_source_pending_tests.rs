// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
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
        let source = bed
            .secrets()
            .join(super::super::super::ai_pool::AI_POOL_FILE);
        let called = std::cell::Cell::new(false);
        let result = sync_accounts_for_init_with(
            &bed.home,
            &bed.state,
            &roster,
            NOW,
            || {
                called.set(true);
                if changed {
                    fs::write(&source, format!("{original}# changed after capture\n")).unwrap();
                }
            },
            &|_, _| panic!("the captured local pool must not fetch"),
        );
        assert!(called.get());
        if changed {
            let error = result.unwrap_err();
            assert!(error.contains("changed after init discovery"), "{error}");
            assert_eq!(
                images(),
                before,
                "a stale empty pool cannot publish authority"
            );
            assert_eq!(
                fs::read_to_string(source).unwrap(),
                format!("{original}# changed after capture\n")
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
        assert!(error.contains("changed after capture"), "{error}");
        assert_eq!(image(), before);
    }
}

#[test]
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
        assert!(error.contains("changed after capture"), "{error}");
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
            bed.secrets().join(FLEET_ROSTER_FILE),
        ];
        let retained = || {
            paths
                .iter()
                .map(|path| fs::read(path).ok())
                .collect::<Vec<_>>()
        };
        let before = retained();
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

#[test]
fn pending_registry_does_not_promote_an_automatic_file_into_an_explicit_source() {
    let bed = Bed::new("pending-source");
    let roster = bed.roster(Cli::Claude);
    let fleet = "[[account]]\nlabel='legacy'\nemail='legacy@example.test'\nmachines=['m16']\n";
    bed.secret(FLEET_ROSTER_FILE, fleet);
    let called = std::cell::Cell::new(false);
    crate::harness::alab_roster::while_pending(|| {
        assert!(has_init_accounts_source(
            &bed.home, &bed.state, &roster, NOW
        ));
        assert!(needs_init_accounts_sync(
            &bed.home, &bed.state, &roster, NOW
        ));
        assert!(matches!(
            fleet_origin(Some(&bed.home), Some(&bed.aterm_toml()), &roster),
            FleetOrigin::AlabPending(_)
        ));
        let expected = lab_chosen(Cli::Claude, &roster, &bed.aterm_toml()).unwrap_err();
        let result = sync_accounts_for_init_with(
            &bed.home,
            &bed.state,
            &roster,
            NOW,
            || called.set(true),
            &|_, _| panic!("a pending source fetched a fallback"),
        );
        assert_eq!(
            result.unwrap_err(),
            expected,
            "the selector's error must survive init"
        );
        assert!(!called.get(), "a pending source reached source capture");
        assert!(!roster.exists(), "a pending source created account rows");
        assert!(!bed.config.join(FLEET_ROSTER_FILE).exists());
        assert_eq!(
            fs::read_to_string(bed.secrets().join(FLEET_ROSTER_FILE)).unwrap(),
            fleet
        );
    });
}

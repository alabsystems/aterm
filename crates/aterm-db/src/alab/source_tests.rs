// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::Lab;
use aterm_secrets::{commons::roster::Role, fsx};
use aterm_spec::interp::State;
use aterm_uds::private_file::PrivateLock;
use std::time::Duration;

fn locked(dir: &Path, name: &str) -> bool {
    match PrivateLock::take(dir, name, Duration::ZERO, "source held") {
        Ok(_) => false,
        Err(why) if why.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(why) => panic!("cannot probe source lock: {why}"),
    }
}

fn check(scenario: i64, mutant: u8) -> State {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let scope = registry.open().unwrap();
    let config = paths.home.join("source.toml");
    let source = registry
        .with_admitted_source(&config, None, |source| Ok(source.clone()))
        .unwrap();
    let mut expected = source.revision;
    match scenario {
        1 => expected.inventory.0[0] ^= 1,
        2 => expected.scope[0] ^= 1,
        3 => expected.actor[0] ^= 1,
        4 => expected.control[0] ^= 1,
        5 | 6 => {
            let id = if scenario == 5 {
                scope.digest()
            } else {
                crate::catalog::Directory::new(paths, lab.bootstrap(0).trust())
                    .catalog_scope()
                    .digest()
            };
            fsx::write_atomic(
                &config,
                format!(
                    "[commons]\nexclude_tables = [\"{}\"]\n",
                    aterm_codec::hex::encode_lower(&id)
                )
                .as_bytes(),
                0o600,
            )
            .unwrap();
        }
        7 => fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap(),
        9 => Arc::make_mut(&mut expected.projection).me = Some("another-projection".into()),
        _ => {}
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            MUTANT.set(0);
            crate::generation::RELEASE_SOURCE_EARLY.set(false);
        }
    }
    let _reset = Reset;
    MUTANT.set(mutant);
    crate::generation::RELEASE_SOURCE_EARLY.set(mutant == 3);
    let table = crate::catalog::Directory::new(paths, lab.bootstrap(0).trust())
        .path(scope)
        .unwrap();
    let locks = [
        (paths.dir.clone(), ".lock"),
        (paths.dir.join("commons/control"), "control.lock"),
        (table, "table.lock"),
    ];
    let mut called = false;
    let mut held = true;
    let receipt = paths.home.join("local-receipt");
    let result = registry.with_admitted_source(&config, Some(&expected), |_| {
        called = true;
        fsx::write_atomic(&receipt, b"admitted", 0o600).unwrap();
        held = locks.iter().all(|(dir, name)| locked(dir, name));
        if scenario == 8 {
            Err(Error::new("local receipt refused"))
        } else {
            Ok(())
        }
    });
    let released = locks.iter().all(|(dir, name)| !locked(dir, name));
    assert_eq!(receipt.exists(), called);
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("called", i64::from(called)),
        ("accepted", i64::from(result.is_ok())),
        ("held", i64::from(held)),
        ("released", i64::from(released)),
    ])
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    if action == "Scenario" {
        let mut next = state.clone();
        next.insert("scenario", state["scenario"] + 1);
        next
    } else {
        assert_eq!(action, "Admit");
        check(state["scenario"], 0)
    }
}

#[test]
fn native_source_admission_refines_receipt_custody() {
    let model = aterm_models_db::native_source_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut admitted = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                admitted += usize::from(action.name == "Admit");
            }
        }
    }
    assert_eq!(admitted, 10);
    for (mutant, scenario, law) in [
        (1, 2, "RefuseChangedOrExcluded"),
        (2, 5, "RefuseChangedOrExcluded"),
        (3, 0, "HoldThroughReceipt"),
    ] {
        assert!(
            !model.check_invariant(law, &check(scenario, mutant)),
            "mutant {mutant}: {law}"
        );
    }
}

#[test]
fn native_source_refuses_a_changed_registry_before_saving_a_receipt() {
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    let config = lab.paths[0].home.join("source.toml");
    let old = registry.observe_source(&config).unwrap().unwrap();
    registry
        .with_admitted_source(&config, Some(&old.revision), |source| {
            assert_eq!(source.revision, old.revision);
            assert_eq!(source.view(), old.view());
            assert!(Arc::ptr_eq(source.view(), &source.revision.projection));
            Ok(())
        })
        .unwrap();
    registry
        .write(vec![vec![
            crate::typed::cell::change(
                &crate::alab::registry::MODEL,
                crate::typed::cell::CellKey {
                    pk: "test-model",
                    col: "row",
                    elem: None,
                },
                &crate::typed::cell::CellValue::automatic(1, aterm_json::json!("live")),
            )
            .unwrap(),
        ]])
        .unwrap();
    assert!(
        registry
            .with_admitted_source::<()>(&config, Some(&old.revision), |_| panic!(
                "changed inventory admitted"
            ))
            .is_err()
    );
    let new = registry.observe_source(&config).unwrap().unwrap();
    assert_eq!(
        old.view(),
        new.view(),
        "the edit changes no projected account"
    );
    assert_ne!(old.revision, new.revision);
}

#[test]
fn native_source_retains_mail_and_address_identity_from_one_view() {
    use crate::typed::grid::tests::put;
    use aterm_json::json;

    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    let scope = registry.open().unwrap();
    let config = lab.paths[0].home.join("source.toml");
    let login = keys::address_pk("before@example.test").unwrap();
    let mailbox = keys::mailbox_pk(&login).unwrap();
    registry
        .write(vec![vec![
            put(&ADDRESS, &login, "addr", json!("before@example.test")),
            put(&MAILBOX, &mailbox, "login", json!(login)),
        ]])
        .unwrap();
    let before = registry.observe_source(&config).unwrap().unwrap();
    assert_eq!(before.table(), scope.digest());
    for excluded in [before.revision.catalog, before.table()] {
        let policy = crate::catalog::Policy {
            excluded: BTreeSet::from([excluded]),
            ..Default::default()
        };
        assert!(!before.included_by(&policy));
    }
    assert!(before.included_by(&Default::default()));
    assert_eq!(before.mailboxes()[0].login, "before@example.test");
    assert_eq!(
        mail::login_mailbox(before.addresses(), "before@example.test"),
        Some(mailbox.clone())
    );
    registry
        .write(vec![vec![put(
            &ADDRESS,
            &login,
            "addr",
            json!("after@example.test"),
        )]])
        .unwrap();
    let after = registry.observe_source(&config).unwrap().unwrap();
    assert_eq!(before.mailboxes()[0].login, "before@example.test");
    assert_eq!(after.mailboxes()[0].login, "after@example.test");
    assert_eq!(
        mail::login_mailbox(after.addresses(), "after@example.test"),
        Some(mailbox)
    );
    assert_ne!(before.revision(), after.revision());
    assert!(!format!("{after:?}").contains("after@example.test"));
    registry
        .with_admitted_source(&config, Some(after.revision()), |current| {
            assert_eq!(current.mailboxes(), after.mailboxes());
            assert_eq!(current.addresses(), after.addresses());
            Ok(())
        })
        .unwrap();
    assert!(
        registry
            .with_admitted_source::<()>(&config, Some(before.revision()), |_| {
                panic!("mailbox rename admitted an older source")
            })
            .is_err()
    );
}

#[test]
fn native_source_selection_does_not_initialize_missing_custody() {
    let lab = Lab::with_foundation_enrolled(&[Role::Writer, Role::Writer], 1);
    let paths = &lab.paths[1];
    let before = crate::tests::files(&paths.dir);
    let result = Registry::at(paths.clone()).with_admitted_source::<()>(
        &paths.home.join("config.toml"),
        None,
        |_| panic!("unestablished source admitted"),
    );
    assert!(result.is_err());
    assert!(
        Registry::at(paths.clone())
            .observe_source(&paths.home.join("config.toml"))
            .is_err()
    );
    assert_eq!(crate::tests::files(&paths.dir), before);
}

#[test]
fn native_source_observer_needs_no_writer_and_changes_no_storage() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let scope = registry.open().unwrap();
    let config = paths.home.join("config.toml");
    let table = crate::catalog::Directory::new(paths, lab.bootstrap(0).trust())
        .path(scope)
        .unwrap();
    let control = paths.dir.join("commons/control");
    let before = crate::tests::files(&paths.dir);
    let control_writer = PrivateLock::take(
        &control,
        "control.lock",
        Duration::ZERO,
        "hold control writer",
    )
    .unwrap();
    let table_writer =
        PrivateLock::take(&table, "table.lock", Duration::ZERO, "hold table writer").unwrap();
    let source = registry.observe_source(&config).unwrap().unwrap();
    assert!(Arc::ptr_eq(source.view(), &source.revision.projection));
    drop(table_writer);
    drop(control_writer);
    assert_eq!(crate::tests::files(&paths.dir), before);
    registry
        .with_admitted_source(&config, Some(&source.revision), |_| Ok(()))
        .unwrap();
}

#[test]
fn native_source_observer_distinguishes_absence_exclusion_and_busy_state() {
    let empty = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(empty.path());
    assert!(
        Registry::at(paths.clone())
            .observe_source(&paths.home.join("config.toml"))
            .unwrap()
            .is_none()
    );
    assert!(!paths.dir.exists());

    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let config = paths.home.join("config.toml");
    drop(lab.bootstrap(0));
    assert!(registry.observe_source(&config).unwrap().is_none());
    let scope = registry.open().unwrap();
    assert!(registry.observe_source(&config).unwrap().is_some());
    let writer = aterm_secrets::client::test_hold_state_lock(paths).unwrap();
    assert!(registry.observe_source(&config).unwrap_err().is_busy());
    drop(writer);
    fsx::write_atomic(
        &config,
        format!(
            "[commons]\nexclude_tables = [\"{}\"]\n",
            aterm_codec::hex::encode_lower(&scope.digest())
        )
        .as_bytes(),
        0o600,
    )
    .unwrap();
    assert!(
        registry
            .observe_source(&config)
            .unwrap_err()
            .to_string()
            .contains("excluded")
    );
    fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    assert!(
        registry
            .observe_source(&config)
            .unwrap_err()
            .to_string()
            .contains("excluded")
    );
}

#[test]
fn native_source_selects_a_rolled_table_inside_current_admission() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    registry.publish_schema().unwrap();
    let config = paths.home.join("config.toml");
    let old = registry.observe_source(&config).unwrap().unwrap();
    let rolled = registry
        .roll(true, aterm_time::unix_millis().unwrap())
        .unwrap();
    assert!(rolled.sealed, "{rolled:?}");
    assert!(
        registry
            .with_admitted_source::<()>(&config, Some(&old.revision), |_| panic!(
                "old table revision admitted"
            ))
            .is_err()
    );
    let new = registry.observe_source(&config).unwrap().unwrap();
    assert_ne!(old.revision.scope, new.revision.scope);
    assert_eq!(old.view(), new.view());
    registry
        .with_admitted_source(&config, Some(&new.revision), |_| Ok(()))
        .unwrap();
}

#[test]
fn native_source_observation_does_not_grant_a_reader_receipt_authority() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let config = paths.home.join("config.toml");
    let before = registry.observe_source(&config).unwrap().unwrap();
    lab.rotate(&[Role::Reader]);
    let reader = registry.observe_source(&config).unwrap().unwrap();
    assert_eq!(before.view(), reader.view());
    assert_ne!(before.revision(), reader.revision());
    assert!(
        registry
            .with_admitted_source::<()>(&config, Some(reader.revision()), |_| panic!(
                "an observer token granted a reader writer authority"
            ))
            .is_err()
    );
}

#[test]
fn native_source_observer_retains_a_registry_in_an_authenticated_recovery_catalog() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let config = paths.home.join("config.toml");
    let before = registry.observe_source(&config).unwrap().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let catalog = directory.catalog_scope();
    let source = directory.observe_table(&session, catalog).unwrap();
    let wires: Vec<_> = source
        .inventory(&session)
        .unwrap()
        .map(|id| source.wire(&session, id).unwrap().unwrap().to_vec())
        .collect();
    drop(source);
    std::fs::rename(
        directory.path(catalog).unwrap(),
        paths.home.join("preserved-native-catalog"),
    )
    .unwrap();
    let mut recovery = crate::table::store::Store::join_recovery(
        &session,
        &directory.recovery().path(catalog).unwrap(),
        catalog,
    )
    .unwrap();
    for wire in wires {
        recovery.ingest(&session, &wire).unwrap();
    }
    drop(recovery);
    drop(session);
    drop(bootstrap);
    let recovered = registry.observe_source(&config).unwrap().unwrap();
    assert_eq!(before.view(), recovered.view());
    assert_eq!(before.revision(), recovered.revision());
}

#[test]
fn native_source_observer_keeps_lost_vault_configuration_distinct_from_absence() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    std::fs::rename(
        &paths.standing_files()[0],
        paths.home.join("preserved-vault-source.toml"),
    )
    .unwrap();
    assert!(
        registry
            .observe_source(&paths.home.join("config.toml"))
            .is_err(),
        "an established registry cannot become an absent source by losing its vault configuration"
    );
}

#[test]
fn native_source_observer_keeps_a_lost_catalog_distinct_from_absence() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let catalog = directory.catalog_scope();
    let claims = crate::table::store::claim_dir(&session).unwrap();
    std::fs::rename(
        directory.path(catalog).unwrap(),
        paths.home.join("preserved-native-catalog"),
    )
    .unwrap();
    std::fs::rename(
        claims.join(format!(
            "{}.claim",
            aterm_codec::hex::encode_lower(&catalog.digest())
        )),
        paths.home.join("preserved-native-catalog.claim"),
    )
    .unwrap();
    drop(session);
    drop(bootstrap);
    assert!(
        registry
            .observe_source(&paths.home.join("config.toml"))
            .is_err(),
        "retained registry storage is evidence that the catalog is missing, not absent"
    );
}

#[test]
fn native_source_observer_refuses_a_control_change_during_projection() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    Registry::at(paths.clone()).open().unwrap();
    let observed =
        application::observe_existing_session(paths, crate::admission::deadline(), |_| {
            lab.rotate(&[Role::Reader]);
            Ok(())
        });
    assert!(
        observed.is_err(),
        "the observed control prefix changed during the read"
    );
}

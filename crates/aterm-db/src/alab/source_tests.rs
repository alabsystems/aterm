// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::Lab;
use aterm_secrets::{client, commons::roster::Role, fsx};
use aterm_uds::private_file::PrivateLock;
use std::time::Duration;

#[test]
fn native_projection_refuses_each_changed_source_field_and_excluded_scope() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let scope = registry.open().unwrap();
    let config = paths.home.join("source.toml");
    let vault = client::config(paths).unwrap().unwrap();
    let source = registry.observe_projection(&config).unwrap();
    for scenario in 1..=6 {
        let mut expected = source.revision.clone();
        match scenario {
            1 => expected.source.revision.inventory.0[0] ^= 1,
            2 => expected.source.revision.scope[0] ^= 1,
            3 => expected.source.revision.actor[0] ^= 1,
            4 => expected.source.revision.control[0] ^= 1,
            5 => expected.source.revision.catalog[0] ^= 1,
            6 => {
                Arc::make_mut(&mut expected.source.revision.projection).me =
                    Some("another-projection".into())
            }
            _ => unreachable!(),
        }
        assert!(
            registry
                .with_admitted_projection::<()>(
                    &config,
                    paths,
                    &vault,
                    Some(&expected),
                    |_, _| panic!("changed field {scenario} admitted")
                )
                .is_err()
        );
    }
    for id in [scope.digest(), source.source().revision.catalog] {
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
        assert!(
            registry
                .observe_projection(&config)
                .unwrap_err()
                .to_string()
                .contains("excluded")
        );
        assert!(
            registry
                .with_admitted_projection::<()>(
                    &config,
                    paths,
                    &vault,
                    Some(source.revision()),
                    |_, _| panic!("excluded source admitted")
                )
                .unwrap_err()
                .to_string()
                .contains("excluded")
        );
    }
}

#[test]
fn installed_projection_is_absent_only_on_a_new_installation_and_refuses_a_busy_vault() {
    let empty = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(empty.path());
    assert!(
        Registry::at(paths.clone())
            .observe_installed_projection(&paths.home.join("config.toml"))
            .unwrap()
            .is_none()
    );
    assert!(!paths.dir.exists());

    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let config = paths.home.join("config.toml");
    drop(lab.bootstrap(0));
    // Inside an established vault a missing registry is unavailable, never absence.
    assert!(registry.observe_installed_projection(&config).is_err());
    registry.open().unwrap();
    assert!(
        registry
            .observe_installed_projection(&config)
            .unwrap()
            .is_some()
    );
    let writer = client::test_hold_state_lock(paths).unwrap();
    assert!(
        registry
            .observe_installed_projection(&config)
            .unwrap_err()
            .is_busy()
    );
    drop(writer);
    fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    assert!(
        registry
            .observe_installed_projection(&config)
            .unwrap_err()
            .to_string()
            .contains("excluded")
    );
}

#[test]
fn native_projection_refuses_a_changed_registry_before_saving_a_receipt() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let vault = client::config(paths).unwrap().unwrap();
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let config = lab.paths[0].home.join("source.toml");
    let old = registry.observe_projection(&config).unwrap();
    registry
        .with_admitted_projection(
            &config,
            paths,
            &vault,
            Some(old.revision()),
            |current, _| {
                let source = current.source();
                assert_eq!(current.revision(), old.revision());
                assert_eq!(source.view(), old.source().view());
                assert!(Arc::ptr_eq(source.view(), &source.revision.projection));
                Ok(())
            },
        )
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
            .with_admitted_projection::<()>(
                &config,
                paths,
                &vault,
                Some(old.revision()),
                |_, _| panic!("changed inventory admitted")
            )
            .is_err()
    );
    let new = registry.observe_projection(&config).unwrap();
    assert_eq!(
        old.source().view(),
        new.source().view(),
        "the edit changes no projected account"
    );
    assert_ne!(old.revision, new.revision);
}

#[test]
fn native_projection_retains_mail_and_address_identity_from_one_view() {
    use crate::typed::grid::tests::put;
    use aterm_json::json;

    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let vault = client::config(paths).unwrap().unwrap();
    let registry = Registry::at(paths.clone());
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
    let before_projection = registry.observe_projection(&config).unwrap();
    let before = before_projection.source();
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
    let after_projection = registry.observe_projection(&config).unwrap();
    let after = after_projection.source();
    assert_eq!(before.mailboxes()[0].login, "before@example.test");
    assert_eq!(after.mailboxes()[0].login, "after@example.test");
    assert_eq!(
        mail::login_mailbox(after.addresses(), "after@example.test"),
        Some(mailbox)
    );
    assert_ne!(before.revision(), after.revision());
    assert!(!format!("{after:?}").contains("after@example.test"));
    registry
        .with_admitted_projection(
            &config,
            paths,
            &vault,
            Some(after_projection.revision()),
            |projection, _| {
                let current = projection.source();
                assert_eq!(current.mailboxes(), after.mailboxes());
                assert_eq!(current.addresses(), after.addresses());
                Ok(())
            },
        )
        .unwrap();
    assert!(
        registry
            .with_admitted_projection::<()>(
                &config,
                paths,
                &vault,
                Some(before_projection.revision()),
                |_, _| { panic!("mailbox rename admitted an older source") }
            )
            .is_err()
    );
}

#[test]
fn native_projection_selection_does_not_initialize_missing_custody() {
    let lab = Lab::with_foundation_enrolled(&[Role::Writer, Role::Writer], 1);
    let paths = &lab.paths[1];
    let vault = client::config(paths).unwrap().unwrap();
    let before = crate::tests::files(&paths.dir);
    let result = Registry::at(paths.clone()).with_admitted_projection::<()>(
        &paths.home.join("config.toml"),
        paths,
        &vault,
        None,
        |_, _| panic!("unestablished source admitted"),
    );
    assert!(result.is_err());
    assert!(
        Registry::at(paths.clone())
            .observe_projection(&paths.home.join("config.toml"))
            .is_err()
    );
    assert!(
        Registry::at(paths.clone())
            .observe_local_source(&paths.home.join("config.toml"))
            .is_err()
    );
    assert_eq!(crate::tests::files(&paths.dir), before);
}

#[test]
fn native_projection_selects_a_rolled_table_inside_current_admission() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    registry.seed().unwrap();
    registry.publish_schema().unwrap();
    let config = paths.home.join("config.toml");
    let vault = client::config(paths).unwrap().unwrap();
    let old = registry.observe_projection(&config).unwrap();
    let rolled = registry
        .roll(true, aterm_time::unix_millis().unwrap())
        .unwrap();
    assert!(rolled.sealed, "{rolled:?}");
    assert!(
        registry
            .with_admitted_projection::<()>(
                &config,
                paths,
                &vault,
                Some(old.revision()),
                |_, _| panic!("old table revision admitted")
            )
            .is_err()
    );
    let new = registry.observe_projection(&config).unwrap();
    assert_ne!(old.table(), new.table());
    assert_eq!(old.source().view(), new.source().view());
    registry
        .with_admitted_projection(&config, paths, &vault, Some(new.revision()), |_, _| Ok(()))
        .unwrap();
}

#[test]
fn native_projection_retains_a_registry_in_an_authenticated_recovery_catalog() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let config = paths.home.join("config.toml");
    let vault = client::config(paths).unwrap().unwrap();
    let before = registry.observe_projection(&config).unwrap();
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
    let claim = crate::table::store::claim_dir(&session)
        .unwrap()
        .join(format!(
            "{}.claim",
            aterm_codec::hex::encode_lower(&catalog.digest())
        ));
    std::fs::rename(claim, paths.home.join("preserved-native-catalog.claim")).unwrap();
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
    let recovered = registry.observe_projection(&config).unwrap();
    assert_eq!(before.source(), recovered.source());
    assert_ne!(before.revision(), recovered.revision());
    assert!(
        registry
            .with_admitted_projection::<()>(
                &config,
                paths,
                &vault,
                Some(before.revision()),
                |_, _| panic!("old physical catalog admitted")
            )
            .is_err()
    );
    registry
        .with_admitted_projection(
            &config,
            paths,
            &vault,
            Some(recovered.revision()),
            |_, _| Ok(()),
        )
        .unwrap();
}

#[test]
fn native_projection_keeps_lost_vault_configuration_distinct_from_absence() {
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
            .observe_projection(&paths.home.join("config.toml"))
            .is_err(),
        "an established registry cannot become an absent source by losing its vault configuration"
    );
    assert!(
        registry
            .observe_local_source(&paths.home.join("config.toml"))
            .is_err()
    );
}

#[test]
fn native_projection_keeps_a_lost_catalog_distinct_from_absence() {
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
            .observe_projection(&paths.home.join("config.toml"))
            .is_err(),
        "retained registry storage is evidence that the catalog is missing, not absent"
    );
    assert!(
        registry
            .observe_local_source(&paths.home.join("config.toml"))
            .is_err()
    );
}

#[test]
fn native_projection_refuses_control_drift_and_holds_control_during_persistence() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let config = paths.home.join("config.toml");
    let vault = client::config(paths).unwrap().unwrap();
    let before = registry.observe_projection(&config).unwrap();
    lab.rotate(&[Role::Reader]);
    let after = registry.observe_projection(&config).unwrap();
    assert_eq!(before.source().view(), after.source().view());
    assert_ne!(before.revision(), after.revision());
    assert!(
        registry
            .with_admitted_projection::<()>(
                &config,
                paths,
                &vault,
                Some(before.revision()),
                |_, _| panic!("stale control admitted")
            )
            .is_err()
    );
    let control = paths.dir.join("commons/control");
    registry
        .with_admitted_projection(&config, paths, &vault, Some(after.revision()), |_, _| {
            assert_eq!(
                PrivateLock::take(
                    &control,
                    "control.lock",
                    Duration::ZERO,
                    "contending writer"
                )
                .err()
                .map(|error| error.kind()),
                Some(std::io::ErrorKind::WouldBlock)
            );
            Ok(())
        })
        .unwrap();
    drop(PrivateLock::take(&control, "control.lock", Duration::ZERO, "released").unwrap());
}

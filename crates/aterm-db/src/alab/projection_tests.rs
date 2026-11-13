// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::{store::Store, tests::Lab};
use aterm_secrets::{client, commons::roster::Role, fsx};
use aterm_spec::interp::State;
use aterm_uds::private_file::PrivateLock;
use std::time::Duration;

#[path = "local_projection_tests.rs"]
mod local;

fn locked(dir: &Path, name: &str) -> bool {
    match PrivateLock::take(dir, name, Duration::ZERO, "projection custody") {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(error) => panic!("cannot probe projection custody: {error}"),
    }
}

fn check(scenario: i64, mutant: u8) -> State {
    if scenario >= 11 {
        return local::check(scenario, mutant);
    }
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let scope = registry.open().unwrap();
    let config = paths.home.join("source.toml");
    if scenario == 1 {
        lab.rotate(&[Role::Reader]);
    }
    let bootstrap = lab.bootstrap(0);
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let table = directory.path(scope).unwrap();
    let catalog = directory.path(directory.catalog_scope()).unwrap();
    let control = paths.dir.join("commons/control");
    let mut expected = registry.observe_projection(&config).unwrap().revision;
    let mut vault_config = client::config(paths).unwrap().unwrap();
    let wrong = Paths::for_home(&paths.home.join("foreign-vault"));
    let entry_vault = if scenario == 6 { &wrong } else { paths };
    let mut table_owner = None;
    match scenario {
        2 => expected.source.revision.inventory.0[0] ^= 1,
        3 => fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap(),
        4 => std::fs::rename(catalog.join("table.lock"), catalog.join("saved-lock")).unwrap(),
        5 => {
            table_owner = Some(
                PrivateLock::take(&table, "table.lock", Duration::ZERO, "fixture writer").unwrap(),
            );
        }
        7 => vault_config.name.push_str("-different"),
        10 => {
            let claims = paths.dir.join("commons/table-claims");
            let claim = claims.join(format!(
                "{}.claim",
                aterm_codec::hex::encode_lower(&scope.digest())
            ));
            let mut bytes = std::fs::read(&claim).unwrap();
            bytes[4] = b'0';
            fsx::write_atomic(&claim, &bytes, 0o600).unwrap();
        }
        _ => {}
    }
    let before = crate::tests::files(&paths.dir);
    PROJECTION_MUTANT.set(mutant);
    crate::documents::application::DROP_PROJECTION_CUSTODY.set(mutant == 2);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            PROJECTION_MUTANT.set(0);
            crate::documents::application::DROP_PROJECTION_CUSTODY.set(false);
        }
    }
    let _reset = Reset;
    let receipt = paths.home.join("projection-receipt");
    let mut called = false;
    let mut held = true;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        registry.with_admitted_projection(
            &config,
            entry_vault,
            &vault_config,
            Some(&expected),
            |source, reader| {
                called = true;
                assert_eq!(source.table(), scope.digest());
                assert!(matches!(
                    reader.observe("unselected-candidate").unwrap(),
                    client::EntryRead::Absent(_)
                ));
                held = locked(&paths.dir, ".lock")
                    && locked(&control, "control.lock")
                    && locked(&catalog, "table.lock")
                    && locked(&table, "table.lock");
                fsx::write_atomic(&receipt, b"admitted", 0o600).unwrap();
                if scenario == 9 {
                    panic!("projection callback unwind");
                }
                if scenario == 8 {
                    Err(Error::new("projection callback refused"))
                } else {
                    Ok(())
                }
            },
        )
    }));
    let accepted = matches!(result, Ok(Ok(())));
    assert_eq!(receipt.exists(), called);
    assert_eq!(
        crate::tests::files(&paths.dir),
        before,
        "projection changed source storage"
    );
    drop(table_owner.take());
    for (dir, name) in [
        (&paths.dir, ".lock"),
        (&control, "control.lock"),
        (&catalog, "table.lock"),
        (&table, "table.lock"),
    ] {
        if dir.join(name).exists() {
            assert!(!locked(dir, name), "projection leaked {name}");
        }
    }
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("called", i64::from(called)),
        ("accepted", i64::from(accepted)),
        ("held", i64::from(held)),
    ])
}

pub(crate) fn transition(state: &State, action: &str) -> State {
    match action {
        "Case" => {
            let mut next = state.clone();
            next.insert("scenario", state["scenario"] + 1);
            next
        }
        "Read" => check(state["scenario"], 0),
        _ => panic!("unknown projection action {action}"),
    }
}

#[test]
fn native_projection_refines_current_subscriber_and_data_custody() {
    let model = aterm_models_db::native_projection_model();
    aterm_spec::interp::prove_and_catch(&model);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Read");
            }
        }
    }
    assert_eq!(cases, 39);
    for (mutant, scenario, law) in [
        (1, 2, "AdmittedInputs"),
        (2, 0, "DataCustody"),
        (3, 1, "ReaderAndWriterAvailable"),
        (1, 26, "AdmittedInputs"),
        (1, 27, "AdmittedInputs"),
        (1, 28, "AdmittedInputs"),
        (1, 29, "AdmittedInputs"),
        (1, 30, "AdmittedInputs"),
        (1, 31, "AdmittedInputs"),
        (2, 11, "DataCustody"),
        (3, 17, "ReaderAndWriterAvailable"),
        (4, 15, "ReaderAndWriterAvailable"),
        (4, 16, "ReaderAndWriterAvailable"),
        (5, 15, "ReaderAndWriterAvailable"),
        (5, 16, "ReaderAndWriterAvailable"),
        (4, 36, "AdmittedInputs"),
        (4, 37, "AdmittedInputs"),
        (5, 34, "AdmittedInputs"),
        (5, 35, "AdmittedInputs"),
    ] {
        let result = check(scenario, mutant);
        assert!(
            !model.check_invariant(law, &result),
            "{law}: mutant={mutant} {result:?}"
        );
    }
}

#[test]
fn retained_subscriber_keeps_read_only_session_authority() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    Registry::at(paths.clone()).open().unwrap();
    let before = crate::tests::files(&paths.dir);
    crate::generation::with_retained_session(
        paths,
        paths,
        crate::admission::deadline(),
        |session, look, validate| {
            assert!(look.guards_source());
            assert!(!session.has_control_writer());
            assert!(session.writer().is_err());
            let directory = crate::catalog::Directory::new(paths, session.trust());
            let mut catalog = directory.observe_store(session, directory.catalog_scope())?;
            assert!(
                catalog
                    .commit_catalog(session, b"forbidden projection write")
                    .is_err()
            );
            (validate.current)()?;
            (validate.custody)()
        },
    )
    .unwrap();
    assert_eq!(crate::tests::files(&paths.dir), before);
}

/// A retained session's checks (`ReadValidation::current`) each judge the vault afresh through the
/// retained look's glance, as each did before a look kept what it derived
/// (`client::VaultLook::forget_derived`): a check after another stamps the Commons observation's
/// files and looks at `state.toml` again (`client::test_look_derivations`). Each judges through
/// the glance, each file's `lstat` as the look first saw it, so a file changed during the look
/// without the state lock can be given the look's first answer (`client::VaultLook`); the next
/// session's open judges it afresh.
/// NEGATIVE CONTROL (2026-11-11): with `with_retained_session` letting its looks keep what they
/// derived across its checks (no `forget_derived`), the next check stamps nothing (every count 0,
/// `a retained session's next check stamps the vault afresh`).
#[test]
fn a_retained_sessions_checks_each_judge_the_vault_afresh() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    Registry::at(paths.clone()).open().unwrap();
    crate::generation::with_retained_session(
        paths,
        paths,
        crate::admission::deadline(),
        |_, _, validate| {
            (validate.current)()?;
            let before = client::test_look_derivations();
            (validate.current)()?;
            let derived = client::test_look_derivations().since(before);
            assert!(
                derived.derived.observation_stamps >= 1 && derived.derived.state_looks >= 1,
                "a retained session's next check stamps the vault afresh: {derived:?}"
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn retained_mutable_table_handle_blocks_projection_even_after_its_session_drops() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let scope = registry.open().unwrap();
    let config = paths.home.join("source.toml");
    let vault = client::config(paths).unwrap().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let table = crate::catalog::Directory::new(paths, bootstrap.trust())
        .path(scope)
        .unwrap();
    let retained = Store::open(&session, &table, scope).unwrap();
    drop(session);
    let blocked = registry
        .with_admitted_projection::<()>(&config, paths, &vault, None, |_, _| {
            panic!("a live data writer was ignored")
        })
        .unwrap_err();
    assert!(blocked.is_busy(), "{blocked}");
    drop(retained);
    registry
        .with_admitted_projection(&config, paths, &vault, None, |_, _| Ok(()))
        .unwrap();
}

#[test]
fn native_projection_refuses_a_hidden_successor_declaration() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let registry = Registry::at(paths.clone());
    let scope = registry.open().unwrap();
    let config = paths.home.join("source.toml");
    let expected = registry.observe_projection(&config).unwrap();
    assert_eq!(expected.table(), scope.digest());
    let vault = client::config(paths).unwrap().unwrap();
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(paths, bootstrap.trust());
    let mut catalog = directory
        .open_store(&session, directory.catalog_scope())
        .unwrap();
    let next = crate::att::REGISTRY_2
        .descriptor(bootstrap.trust())
        .unwrap();
    let sequence = catalog
        .catalog(&session)
        .unwrap()
        .iter()
        .filter(|record| record.header().actor == session.actor())
        .map(|record| record.header().sequence)
        .max()
        .unwrap_or(0)
        + 2;
    let missing = [91; 32];
    let wire = crate::table::tests::seal(
        &session,
        directory.catalog_scope(),
        crate::crypto::Causal {
            sequence,
            previous: Some(missing),
            parents: &[missing],
        },
        &next.encode().unwrap(),
    )
    .unwrap();
    catalog.ingest(&session, &wire).unwrap();
    drop((catalog, session));
    assert!(registry.observe_projection(&config).is_err());
    let before = crate::tests::files(&paths.dir);
    let mut called = false;
    let result = registry.with_admitted_projection(
        &config,
        paths,
        &vault,
        Some(expected.revision()),
        |_, _| {
            called = true;
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!called, "a hidden successor reached receipt persistence");
    assert_eq!(crate::tests::files(&paths.dir), before);
}

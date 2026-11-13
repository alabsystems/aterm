// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::{Session, tests::Lab};
use aterm_secrets::{client, commons::roster::Role, fsx};
use aterm_spec::interp::State;
use aterm_uds::private_file::{PrivateLock, create_private_dir};
use std::time::Duration;

fn locked(dir: &Path, name: &str) -> bool {
    match PrivateLock::take(dir, name, Duration::ZERO, "local source test") {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(error) => panic!("cannot inspect source custody: {error}"),
    }
}

fn park(paths: &Paths, path: &Path, scope: crate::table::Scope) {
    std::fs::rename(path, paths.home.join("saved-catalog")).unwrap();
    let claim = paths.dir.join("commons/table-claims").join(format!(
        "{}.claim",
        aterm_codec::hex::encode_lower(&scope.digest())
    ));
    std::fs::rename(claim, paths.home.join("saved-catalog-claim")).unwrap();
}

fn recovery_root(paths: &Paths, leaf: &str) -> std::path::PathBuf {
    let parent = paths.dir.join("commons/recovery");
    create_private_dir(&parent).unwrap();
    let root = parent.join(leaf);
    create_private_dir(&root).unwrap();
    root
}

fn check(scenario: i64, mutant: u8) -> State {
    if scenario == 42 {
        crate::table::store::with_volume_for_test([7; 16], || run(scenario, mutant))
    } else {
        run(scenario, mutant)
    }
}

fn run(scenario: i64, mutant: u8) -> State {
    let roles: &[Role] = if scenario == 9 || scenario == 34 {
        &[Role::Writer, Role::Writer]
    } else {
        &[Role::Writer]
    };
    let lab = Lab::new(roles);
    let member = usize::from(scenario == 34);
    let paths = &lab.paths[member];
    let registry = Registry::at(paths.clone());
    let config = paths.home.join("local-source.toml");
    let bootstrap = lab.bootstrap(member);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(paths, session.trust());
    let catalog_scope = directory.catalog_scope();
    let catalog_path = directory.path(catalog_scope).unwrap();
    let generations = crate::att::REGISTRY.chain();
    let first = generations[0].descriptor(session.trust()).unwrap();
    let last = generations
        .last()
        .unwrap()
        .descriptor(session.trust())
        .unwrap();
    // An explicitly enrolled, authenticated empty catalog is positive evidence.
    create_private_dir(&paths.dir.join("commons/tables")).unwrap();
    drop(directory.enroll(&session, catalog_scope).unwrap());
    drop(session);
    if matches!(scenario, 3 | 4 | 28) {
        registry.open().unwrap();
    }
    if matches!(scenario, 1 | 4) {
        lab.rotate(&[Role::Reader]);
    }
    if scenario == 34 {
        let writer = lab.bootstrap(0);
        let writer_session = Session::open(&writer).unwrap();
        let writer_dir = crate::catalog::Directory::new(&lab.paths[0], writer_session.trust());
        let other = Descriptor::application(writer_session.trust(), "notes", "pages", 1).unwrap();
        writer_dir.publish(&writer_session, &other).unwrap();
        let writer_store = writer_dir
            .observe_store(&writer_session, catalog_scope)
            .unwrap();
        let wires: Vec<_> = writer_store
            .inventory(&writer_session)
            .unwrap()
            .map(|id| {
                writer_store
                    .wire(&writer_session, id)
                    .unwrap()
                    .unwrap()
                    .to_vec()
            })
            .collect();
        let reader_session = Session::open(&bootstrap).unwrap();
        let mut reader_store = directory
            .open_store(&reader_session, catalog_scope)
            .unwrap();
        for wire in wires {
            reader_store.ingest(&reader_session, &wire).unwrap();
        }
        drop((reader_store, reader_session, writer_store, writer_session));
        lab.rotate(&[Role::Writer, Role::Reader]);
    }
    let expected = registry.observe_local_source(&config).unwrap().revision();
    let bootstrap = lab.bootstrap(member);
    let session = Session::open(&bootstrap).unwrap();
    let mut owner = None;
    match scenario {
        2 => {
            recovery_root(paths, "tables");
            drop(
                directory
                    .recovery()
                    .enroll(&session, catalog_scope)
                    .unwrap(),
            );
            park(paths, &catalog_path, catalog_scope);
        }
        5 => park(paths, &catalog_path, catalog_scope),
        6 => fsx::write_atomic(
            &aterm_journal::committed_files(&catalog_path, "table").anchor,
            b"damaged",
            0o600,
        )
        .unwrap(),
        7 | 31 => {
            let role = if scenario == 7 {
                "registry.900"
            } else {
                "registry.next.preview"
            };
            let future = Descriptor::application(session.trust(), "alab", role, 1).unwrap();
            directory
                .open_store(&session, catalog_scope)
                .unwrap()
                .commit_catalog(&session, &future.encode().unwrap())
                .unwrap();
        }
        8 | 9 => {
            directory
                .open_store(&session, catalog_scope)
                .unwrap()
                .commit_catalog(&session, b"unsupported future catalog encoding")
                .unwrap();
        }
        10 | 11 | 27 => {
            let target = if scenario == 11 {
                recovery_root(paths, "tables");
                directory.recovery()
            } else {
                directory.clone()
            };
            let mut store = target.enroll(&session, first.scope).unwrap();
            if scenario == 10 {
                store
                    .commit_catalog(&session, b"undeclared registry data")
                    .unwrap();
            }
            drop(store);
        }
        12 => {
            let claims = paths.dir.join("commons/table-claims");
            create_private_dir(&claims).unwrap();
            fsx::write_atomic(
                &claims.join(format!(
                    "{}.claim",
                    aterm_codec::hex::encode_lower(&first.scope.digest())
                )),
                b"pending",
                0o600,
            )
            .unwrap();
        }
        13 | 14 | 28 => {
            let excluded = if scenario == 13 {
                first.scope
            } else {
                last.scope
            };
            fsx::write_atomic(
                &config,
                format!(
                    "[commons]\nexclude_tables = [\"{}\"]\n",
                    aterm_codec::hex::encode_lower(&excluded.digest())
                )
                .as_bytes(),
                0o600,
            )
            .unwrap();
        }
        15 => fsx::write_atomic(&config, b"[commons]\nenabled = false\n", 0o600).unwrap(),
        16 | 17 | 19 => {
            let root = if scenario == 17 {
                recovery_root(paths, "tables")
            } else {
                paths.dir.join("commons/tables")
            };
            create_private_dir(&root).unwrap();
            let name = if scenario == 19 {
                aterm_codec::hex::encode_lower(&[170; 32]).to_uppercase()
            } else {
                aterm_codec::hex::encode_lower(&[170; 32])
            };
            create_private_dir(&root.join(name)).unwrap();
        }
        18 => {
            let root = recovery_root(paths, "table-claims");
            create_private_dir(&root).unwrap();
            fsx::write_atomic(
                &root.join(format!(
                    "{}.claim",
                    aterm_codec::hex::encode_lower(&[170; 32])
                )),
                b"opaque",
                0o600,
            )
            .unwrap();
        }
        20 => create_private_dir(&directory.path(first.scope).unwrap()).unwrap(),
        21 | 24 => {
            let other = Descriptor::application(session.trust(), "notes", "pages", 1).unwrap();
            directory.publish(&session, &other).unwrap();
        }
        26 => {
            owner = Some(
                PrivateLock::take(&catalog_path, "table.lock", Duration::ZERO, "fixture owner")
                    .unwrap(),
            );
        }
        29 => std::fs::rename(
            catalog_path.join("table.lock"),
            catalog_path.join("saved-lock"),
        )
        .unwrap(),
        30 => {
            park(paths, &catalog_path, catalog_scope);
            drop(directory.enroll(&session, catalog_scope).unwrap());
        }
        35 => {
            let ring = &crate::catalog::ring::CLAIMS;
            let now = 123 * ring.period_s;
            directory.retire(&session, ring, now).unwrap();
            drop(
                directory
                    .enroll_ring(&session, ring, ring.epoch_of(now), now)
                    .unwrap(),
            );
            let claims = paths.dir.join("commons/table-claims");
            fsx::write_atomic(&claims.join("release-pass"), b"123", 0o600).unwrap();
            fsx::write_atomic(&claims.join("release-seen"), b"", 0o600).unwrap();
            crate::clock::State::default().write(&claims).unwrap();
        }
        36 => {
            let other = Descriptor::application(session.trust(), "notes", "pages", 1).unwrap();
            directory.publish(&session, &other).unwrap();
            let claims = paths.dir.join("commons/table-claims");
            let volume = claims.join(format!(
                "{}.claim.volume",
                aterm_codec::hex::encode_lower(&other.scope.digest())
            ));
            // Other applications' proof bytes are opaque to registry admission;
            // their bounded private envelope and matched claim are inventoried.
            fsx::write_atomic(&volume, b"other application's volume evidence", 0o600).unwrap();
        }
        37 => {
            let claims = paths.dir.join("commons/table-claims");
            let volume = claims.join(format!(
                "{}.claim.volume",
                aterm_codec::hex::encode_lower(&first.scope.digest())
            ));
            fsx::write_atomic(&volume, b"orphan volume evidence", 0o600).unwrap();
        }
        38 => {
            create_private_dir(&paths.dir.join("commons/table-claims/release-pass")).unwrap();
        }
        39 => {
            fsx::write_atomic(
                &paths.dir.join("commons/table-claims/release-pass"),
                b"123",
                0o600,
            )
            .unwrap();
        }
        40 => {
            fsx::write_atomic(
                &directory.path(first.scope).unwrap(),
                b"not a directory",
                0o600,
            )
            .unwrap();
        }
        41 => {
            let other = Descriptor::application(session.trust(), "notes", "pages", 1).unwrap();
            directory.publish(&session, &other).unwrap();
            let claim = paths.dir.join("commons/table-claims").join(format!(
                "{}.claim",
                aterm_codec::hex::encode_lower(&other.scope.digest())
            ));
            let mut bytes = std::fs::read(&claim).unwrap();
            bytes[4] = b'0';
            fsx::write_atomic(&claim, &bytes, 0o600).unwrap();
        }
        42 => {
            let ring = &crate::catalog::ring::CLAIMS;
            let now = 123 * ring.period_s;
            let epoch = ring.epoch_of(now);
            let old = ring.scope(session.trust(), epoch).unwrap();
            drop(directory.enroll_ring(&session, ring, epoch, now).unwrap());
            let volume = paths.dir.join("commons/table-claims").join(format!(
                "{}.claim.volume",
                aterm_codec::hex::encode_lower(&old.digest())
            ));
            assert!(
                volume.exists(),
                "persistent-volume fixture did not write its witness"
            );
            directory
                .retire(&session, ring, now + 4 * ring.period_s)
                .unwrap();
            assert!(!volume.exists(), "retirement detached its volume witness");
        }
        _ => {}
    }
    drop(session);
    if scenario == 25 {
        registry.open().unwrap();
    }
    if scenario == 9 {
        lab.rotate(&[Role::Reader, Role::Writer]);
    }
    let mut vault = client::config(paths).unwrap().unwrap();
    if scenario == 33 {
        vault.name.push_str("-wrong");
    }
    // These cases intentionally capture a new, valid local source after setup.
    let expected = if matches!(scenario, 2 | 20 | 21 | 35 | 36 | 41 | 42) {
        Some(registry.observe_local_source(&config).unwrap().revision())
    } else {
        Some(expected)
    };
    // Negative controls isolate admission checks from the independent equality gate.
    let expected = if matches!(scenario, 7 | 8 | 9 | 16 | 17 | 18 | 19 | 31) {
        None
    } else {
        expected.as_ref()
    };
    let before = crate::tests::files(&paths.dir);
    if scenario == 32 {
        let orphan = paths
            .dir
            .join("commons/tables")
            .join(aterm_codec::hex::encode_lower(&[170; 32]));
        BEFORE_ENTRY.set(Some(Box::new(move || create_private_dir(&orphan).unwrap())));
    }
    crate::catalog::ABSENCE_MUTANT.set(mutant);
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::catalog::ABSENCE_MUTANT.set(0);
            BEFORE_ENTRY.take();
        }
    }
    let _reset = Reset;
    let mut called = false;
    let mut held = true;
    let mut absent = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        registry.with_admitted_local_source(&config, paths, &vault, expected, |read, reader| {
            called = true;
            absent = matches!(read, LocalSourceRead::Absent(_));
            assert!(matches!(
                reader.observe("candidate-not-enrolled").unwrap(),
                client::EntryRead::Absent(_)
            ));
            let live_catalog = if scenario == 2 {
                directory.recovery().path(catalog_scope).unwrap()
            } else {
                catalog_path.clone()
            };
            held = locked(&paths.dir, ".lock")
                && locked(&paths.dir.join("commons/control"), "control.lock")
                && locked(&live_catalog, "table.lock");
            if scenario == 23 {
                panic!("local source callback unwind");
            }
            if scenario == 22 {
                Err(Error::new("local callback refuses"))
            } else {
                Ok(())
            }
        })
    }));
    let accepted = matches!(result, Ok(Ok(())));
    if scenario != 32 {
        assert_eq!(
            crate::tests::files(&paths.dir),
            before,
            "read mutated its source"
        );
    }
    drop(owner);
    if catalog_path.join("table.lock").exists() {
        assert!(!locked(&catalog_path, "table.lock"), "catalog lock leaked");
    }
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("called", i64::from(called)),
        ("accepted", i64::from(accepted)),
        ("absent", i64::from(absent)),
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
        _ => panic!("unknown local source action {action}"),
    }
}

#[test]
fn local_source_refines_catalog_absence_and_retained_admission() {
    let model = aterm_models_db::local_source_model();
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
    assert_eq!(cases, 43);
    for (mutant, scenario, law) in [
        (1, 24, "AdmittedInputs"),
        (1, 25, "AdmittedInputs"),
        (1, 30, "AdmittedInputs"),
        (1, 39, "AdmittedInputs"),
        (2, 8, "AdmittedInputs"),
        (2, 9, "AdmittedInputs"),
        (3, 16, "AdmittedInputs"),
        (3, 17, "AdmittedInputs"),
        (3, 18, "AdmittedInputs"),
        (3, 32, "AdmittedInputs"),
        (4, 0, "CatalogCustody"),
        (5, 7, "AdmittedInputs"),
        (5, 31, "AdmittedInputs"),
        (6, 1, "AvailableSources"),
    ] {
        let next = check(scenario, mutant);
        assert!(
            !model.check_invariant(law, &next),
            "{law}: mutant={mutant} {next:?}"
        );
    }
}

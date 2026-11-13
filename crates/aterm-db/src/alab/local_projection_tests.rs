// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn wires(session: &Session<'_>, path: &Path, scope: Scope) -> Vec<Vec<u8>> {
    let store = Store::observe(session, path, scope).unwrap();
    store
        .inventory(session)
        .unwrap()
        .map(|id| store.wire(session, id).unwrap().unwrap().to_vec())
        .collect()
}

fn enroll(session: &Session<'_>, path: &Path, scope: Scope, recovery: bool, wires: &[Vec<u8>]) {
    let mut store = if recovery {
        Store::join_recovery(session, path, scope)
    } else {
        Store::join(session, path, scope)
    }
    .unwrap();
    for wire in wires {
        store.ingest(session, wire).unwrap();
    }
}

// Only generated fixture custody is moved; preserve both bytes and enrollment
// so a replacement exercises a genuinely different physical identity.
fn park(paths: &Paths, path: &Path, scope: Scope, recovery: bool, label: &str) {
    std::fs::rename(path, paths.home.join(format!("saved-{label}-data"))).unwrap();
    let root = if recovery {
        "commons/recovery/table-claims"
    } else {
        "commons/table-claims"
    };
    let claim = paths.dir.join(root).join(format!(
        "{}.claim",
        aterm_codec::hex::encode_lower(&scope.digest())
    ));
    std::fs::rename(claim, paths.home.join(format!("saved-{label}-claim"))).unwrap();
}

pub(super) fn check(scenario: i64, mutant: u8) -> State {
    let roles: &[Role] = if scenario == 17 {
        &[Role::Writer, Role::Writer]
    } else {
        &[Role::Writer]
    };
    let lab = Lab::new(roles);
    let member = usize::from(scenario == 17);
    let paths = &lab.paths[member];
    let registry = Registry::at(paths.clone());
    if scenario == 15 {
        let bootstrap = lab.bootstrap(member);
        let session = Session::open(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(paths, session.trust());
        aterm_uds::private_file::create_private_dir(&paths.dir.join("commons/tables")).unwrap();
        directory
            .enroll(&session, directory.catalog_scope())
            .unwrap()
            .commit_catalog(&session, b"inert predecessor of registry declaration")
            .unwrap();
    }
    let scope = registry.open().unwrap();
    // Two causal records make the split-history fixtures depend on both regions.
    let records = || {
        (1..=2)
            .map(|value| {
                vec![crate::database::Change {
                    collection: "projection-fixture".into(),
                    key: "row".into(),
                    value: Some(aterm_json::json!({"value": value})),
                }]
            })
            .collect()
    };
    if scenario == 17 {
        let writer = Registry::at(lab.paths[0].clone());
        writer.open().unwrap();
        writer.write(records()).unwrap();
    } else {
        registry.write(records()).unwrap();
    }
    let config = paths.home.join("projection.toml");
    let bootstrap = lab.bootstrap(member);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(paths, session.trust());
    let catalog_scope = directory.catalog_scope();
    let primary = [
        directory.path(catalog_scope).unwrap(),
        directory.path(scope).unwrap(),
    ];
    let recovery = [
        directory.recovery().path(catalog_scope).unwrap(),
        directory.recovery().path(scope).unwrap(),
    ];
    // The split catalog instead puts the registry declaration last, so neither
    // region alone contains both its declaration and its complete ancestry.
    if scenario != 15 {
        directory
            .open_store(&session, catalog_scope)
            .unwrap()
            .commit_catalog(&session, b"inert local projection fixture")
            .unwrap();
    }
    if scenario == 17 {
        let author = lab.bootstrap(0);
        let author_session = Session::open(&author).unwrap();
        let author_directory =
            crate::catalog::Directory::new(&lab.paths[0], author_session.trust());
        let received = wires(
            &author_session,
            &author_directory.path(scope).unwrap(),
            scope,
        );
        let mut store = Store::open(&session, &primary[1], scope).unwrap();
        for wire in received {
            store.ingest(&session, &wire).unwrap();
        }
    }
    let all = [
        wires(&session, &primary[0], catalog_scope),
        wires(&session, &primary[1], scope),
    ];
    let scopes = [catalog_scope, scope];
    for index in 0..2 {
        if scenario != 26 + index as i64 {
            enroll(&session, &recovery[index], scopes[index], true, &all[index]);
        }
    }
    match scenario {
        12..=14 => {
            for index in 0..2 {
                if scenario == 14 || scenario == 12 + index as i64 {
                    park(
                        paths,
                        &primary[index],
                        scopes[index],
                        false,
                        &format!("primary-{index}"),
                    );
                }
            }
            if scenario == 14 {
                std::fs::rename(
                    paths.dir.join("commons/tables"),
                    paths.home.join("saved-primary-root"),
                )
                .unwrap();
            }
        }
        15 | 16 => {
            let index = (scenario - 15) as usize;
            // Inventory is keyed by digest; order the signed headers by sequence.
            let mut ordered = all[index].clone();
            ordered.sort_by_key(|wire| {
                session
                    .authenticate(scopes[index], wire)
                    .unwrap()
                    .header()
                    .sequence
            });
            assert!(ordered.len() > 1);
            park(
                paths,
                &primary[index],
                scopes[index],
                false,
                "split-primary",
            );
            park(
                paths,
                &recovery[index],
                scopes[index],
                true,
                "split-recovery",
            );
            let mid = ordered.len() - 1;
            enroll(
                &session,
                &primary[index],
                scopes[index],
                false,
                &ordered[mid..],
            );
            enroll(
                &session,
                &recovery[index],
                scopes[index],
                true,
                &ordered[..mid],
            );
        }
        _ => {}
    }
    drop(session);
    drop(bootstrap);
    if scenario == 17 {
        lab.rotate(&[Role::Writer, Role::Reader]);
    }
    let expected = registry
        .observe_projection(&config)
        .unwrap_or_else(|error| panic!("scenario {scenario}: {error}"));
    let vault = client::config(paths).unwrap().unwrap();
    let mut owner = None;
    if (18..=21).contains(&scenario) {
        let ordinal = (scenario - 18) as usize;
        let path = if ordinal % 2 == 0 {
            &primary[ordinal / 2]
        } else {
            &recovery[ordinal / 2]
        };
        owner = Some(
            PrivateLock::take(path, "table.lock", Duration::ZERO, "busy contributor").unwrap(),
        );
    }
    match scenario {
        38 => lab.rotate(&[Role::Reader]),
        22 | 23 => {
            let index = (scenario - 22) as usize;
            std::fs::rename(
                recovery[index].join("table.lock"),
                recovery[index].join("saved-lock"),
            )
            .unwrap();
        }
        24 | 25 => {
            let index = (scenario - 24) as usize;
            park(
                paths,
                &primary[index],
                scopes[index],
                false,
                "absent-primary",
            );
            park(
                paths,
                &recovery[index],
                scopes[index],
                true,
                "absent-recovery",
            );
        }
        26..=31 => {
            let bootstrap = lab.bootstrap(0);
            let session = Session::open(&bootstrap).unwrap();
            let index = ((scenario - 26) % 2) as usize;
            let is_recovery = !(28..30).contains(&scenario);
            let path = if is_recovery {
                &recovery[index]
            } else {
                &primary[index]
            };
            if scenario >= 28 {
                park(paths, path, scopes[index], is_recovery, "replaced");
            }
            enroll(&session, path, scopes[index], is_recovery, &all[index]);
            drop(session);
            let changed = registry.observe_projection(&config).unwrap();
            assert_eq!(
                changed.source(),
                expected.source(),
                "only physical enrollment changed"
            );
            assert_ne!(changed.revision(), expected.revision());
        }
        34..=37 => {
            let bootstrap = lab.bootstrap(0);
            let session = Session::open(&bootstrap).unwrap();
            let index = ((scenario - 34) % 2) as usize;
            let path = if scenario < 36 {
                &recovery[index]
            } else {
                &primary[index]
            };
            let mut store = if scenario < 36 {
                Store::open_recovery(&session, path, scopes[index])
            } else {
                Store::open(&session, path, scopes[index])
            }
            .unwrap();
            let missing = [119; 32];
            let sequence = all[index]
                .iter()
                .map(|wire| {
                    session
                        .authenticate(scopes[index], wire)
                        .unwrap()
                        .header()
                        .sequence
                })
                .max()
                .unwrap_or(0)
                + 2;
            let wire = crate::table::tests::seal(
                &session,
                scopes[index],
                crate::crypto::Causal {
                    sequence,
                    previous: Some(missing),
                    parents: &[missing],
                },
                b"authenticated unavailable predecessor",
            )
            .unwrap();
            store.ingest(&session, &wire).unwrap();
        }
        _ => {}
    }
    let before = crate::tests::files(&paths.dir);
    PROJECTION_MUTANT.set(if mutant <= 3 { mutant } else { 0 });
    crate::documents::application::DROP_PROJECTION_CUSTODY.set(mutant == 2);
    crate::table::store::OMIT_REGION.set(match mutant {
        4 => 1,
        5 => 2,
        _ => 0,
    });
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            PROJECTION_MUTANT.set(0);
            crate::documents::application::DROP_PROJECTION_CUSTODY.set(false);
            crate::table::store::OMIT_REGION.set(0);
        }
    }
    let _reset = Reset;
    let mut called = false;
    let mut held = true;
    let receipt = paths.home.join("local-projection-receipt");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        registry.with_admitted_projection(
            &config,
            paths,
            &vault,
            Some(expected.revision()),
            |read, reader| {
                called = true;
                assert_eq!(read.source(), expected.source());
                assert!(matches!(
                    reader.observe("fixture-candidate").unwrap(),
                    client::EntryRead::Absent(_)
                ));
                held = locked(&paths.dir, ".lock")
                    && locked(&paths.dir.join("commons/control"), "control.lock");
                for path in primary
                    .iter()
                    .chain(recovery.iter())
                    .filter(|path| path.exists())
                {
                    held &= locked(path, "table.lock");
                }
                fsx::write_atomic(&receipt, b"admitted", 0o600).unwrap();
                if scenario == 33 {
                    panic!("local projection callback unwind");
                }
                if scenario == 32 {
                    Err(Error::new("local projection callback refused"))
                } else {
                    Ok(())
                }
            },
        )
    }));
    let accepted = matches!(result, Ok(Ok(())));
    drop(owner.take());
    for path in primary.iter().chain(recovery.iter()) {
        if path.join("table.lock").exists() {
            assert!(!locked(path, "table.lock"));
        }
    }
    assert!(!locked(&paths.dir, ".lock"));
    assert!(!locked(&paths.dir.join("commons/control"), "control.lock"));
    assert_eq!(called, receipt.exists());
    assert_eq!(
        before,
        crate::tests::files(&paths.dir),
        "admission changed native storage"
    );
    State::from([
        ("scenario", scenario),
        ("done", 1),
        ("called", i64::from(called)),
        ("accepted", i64::from(accepted)),
        ("held", i64::from(held)),
    ])
}

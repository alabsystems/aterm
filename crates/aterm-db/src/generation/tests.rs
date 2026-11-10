// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    catalog::Descriptor,
    table::{Scope, tests::Lab},
    tests::files,
};
use aterm_secrets::commons::roster::Role;
use std::collections::BTreeMap;

fn data(lab: &Lab) -> (Scope, Digest) {
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory
        .publish(
            &session,
            &Descriptor::application(bootstrap.trust(), "recovery-test", "records", 1).unwrap(),
        )
        .unwrap();
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let id = store
        .commit_catalog(&session, b"retained before device-number change")
        .unwrap()
        .id;
    (scope, id)
}

fn changed_device(lab: &Lab) {
    changed_device_at(&lab.paths[0]);
}

fn changed_device_at(paths: &Paths) {
    // The observed directory stays put. Only its retained old device number is
    // different, exactly the legacy-without-volume-witness failure on m114.
    let path = paths.dir.join("commons/table-claims");
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|p| p.to_str()) != Some("claim") {
            continue;
        }
        let mut bytes = std::fs::read(&path).unwrap();
        let at = 5 + 104 + 48 + 16;
        let old = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        bytes[at..at + 8].copy_from_slice(&(old + 1).to_le_bytes());
        fsx::write_atomic(&path, &bytes, 0o600).unwrap();
    }
}

fn reserved(lab: &Lab) -> Selection {
    let bootstrap = lab.bootstrap(0);
    let config = lab.paths[0].home.join("config.toml");
    private_file::create_private_dir(&root(&lab.paths[0])).unwrap();
    reserve(&lab.paths[0], &config, &bootstrap).unwrap()
}

fn move_admitted_candidate(lab: &mut Lab) -> Paths {
    let next = candidate(&lab.paths[0]);
    move_admitted_to(lab, 1, next)
}

fn move_admitted_to(lab: &mut Lab, member: usize, next: Paths) -> Paths {
    // Lab's second identity was admitted through its ordinary signed genesis
    // and vault join, with fresh age and device keys. Relocate its empty local
    // custody into the reserved home; no old actor or table claim is copied.
    for entry in std::fs::read_dir(&lab.paths[member].dir).unwrap() {
        let entry = entry.unwrap();
        std::fs::rename(entry.path(), next.dir.join(entry.file_name())).unwrap();
    }
    lab.paths[member] = next.clone();
    next
}

#[test]
fn a_displaced_application_triggers_succession_with_a_healthy_catalog() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (scope, id) = data(&lab);
    let original = lab.paths[0].clone();
    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        assert!(!recovery_required(&session, false).unwrap());
    }
    let claim = original.dir.join("commons/table-claims").join(format!(
        "{}.claim",
        aterm_codec::hex::encode_lower(&scope.digest())
    ));
    let mut bytes = std::fs::read(&claim).unwrap();
    let at = 5 + 104 + 48 + 16;
    let old = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    bytes[at..at + 8].copy_from_slice(&(old + 1).to_le_bytes());
    fsx::write_atomic(&claim, &bytes, 0o600).unwrap();
    let before_claims = files(&original.dir.join("commons/table-claims"));
    let before_tables = files(&original.dir.join("commons/tables"));

    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = Directory::new(&original, bootstrap.trust());
    let catalog = custody::inspect(&session, directory.catalog_scope().digest()).unwrap();
    assert!(catalog.directory.unwrap().binding_error.is_none());
    directory.snapshot_existing(&session).unwrap();
    let application = custody::inspect(&session, scope.digest()).unwrap();
    assert!(application.directory.unwrap().binding_error.is_some());
    assert!(application.prefix.unwrap().is_ok());
    assert!(
        recovery_required(&session, false).unwrap(),
        "a healthy catalog must not hide lost application custody"
    );
    assert_eq!(
        files(&original.dir.join("commons/table-claims")),
        before_claims
    );
    assert_eq!(files(&original.dir.join("commons/tables")), before_tables);
    drop(session);

    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    assert_eq!(selected(&original).unwrap(), next);
    let fresh = lab.bootstrap(1);
    let session = Session::observe(&fresh).unwrap();
    let directory = Directory::new(&next, fresh.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert_eq!(
        store
            .inventory(&session)
            .unwrap()
            .copied()
            .collect::<Vec<_>>(),
        vec![id]
    );
    assert_eq!(
        store.catalog(&session).unwrap()[0].payload(),
        b"retained before device-number change"
    );
    assert_eq!(
        files(&original.dir.join("commons/table-claims")),
        before_claims
    );
    assert_eq!(files(&original.dir.join("commons/tables")), before_tables);
}

#[test]
fn a_displaced_application_with_a_refused_prefix_cannot_start_succession() {
    let lab = Lab::new(&[Role::Writer]);
    let (scope, _) = data(&lab);
    let original = &lab.paths[0];
    let claim = original.dir.join("commons/table-claims").join(format!(
        "{}.claim",
        aterm_codec::hex::encode_lower(&scope.digest())
    ));
    let mut bytes = std::fs::read(&claim).unwrap();
    let at = 5 + 104 + 48 + 16;
    let old = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    bytes[at..at + 8].copy_from_slice(&(old + 1).to_le_bytes());
    *bytes.last_mut().unwrap() ^= 1;
    fsx::write_atomic(&claim, &bytes, 0o600).unwrap();
    let before = files(&original.dir);

    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    assert!(custody::directory_binding_lost(&session, scope.digest()).unwrap());
    assert!(
        custody::inspect(&session, scope.digest())
            .unwrap()
            .prefix
            .unwrap()
            .is_err()
    );
    assert!(recovery_required(&session, false).is_err());
    assert!(!root(original).exists());
    assert_eq!(files(&original.dir), before);
}

#[test]
fn completed_successor_keeps_transport_until_its_preserved_source_changes() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    assert!(refresh_required(&lab.paths[0]).unwrap());
    assert!(refresh_required(&next).unwrap());
    import(&mut selection).unwrap();
    assert!(is_candidate(&next));
    assert!(!refresh_required(&next).unwrap());
    assert!(refresh_required(&lab.paths[0]).unwrap());

    let claims = lab.paths[0].dir.join("commons/table-claims");
    let claim = std::fs::read_dir(claims)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let before = std::fs::read(&claim).unwrap();
    let mut changed = before.clone();
    *changed.last_mut().unwrap() ^= 1;
    fsx::write_atomic(&claim, &changed, 0o600).unwrap();
    assert!(refresh_required(&next).unwrap());
    fsx::write_atomic(&claim, &before, 0o600).unwrap();
    assert!(!refresh_required(&next).unwrap());
}

#[test]
fn a_busy_ancestor_releases_every_partial_source_read_before_waiting() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    let original = lab.paths[0].clone();
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let writers =
        [&original, &next].map(|paths| private_file::open_private(&paths.dir, ".lock").unwrap().0);
    #[expect(
        clippy::disallowed_methods,
        reason = "hold the ancestor vault's exclusive test lease"
    )]
    writers[0].try_lock().unwrap();
    assert!(try_local_source_reads(&next).unwrap().is_none());
    assert!(local_source_reads_until(&next, Instant::now()).is_err());
    #[expect(
        clippy::disallowed_methods,
        reason = "a failed attempt must release the descendant before waiting"
    )]
    writers[1]
        .try_lock()
        .expect("no descendant guard may survive the failed ancestor take");
    writers[1].unlock().unwrap();
    writers[0].unlock().unwrap();

    let reads = try_local_source_reads(&next).unwrap().unwrap();
    for writer in &writers {
        #[expect(
            clippy::disallowed_methods,
            reason = "a successful attempt holds every source in the lineage"
        )]
        let attempt = writer.try_lock();
        assert!(matches!(attempt, Err(std::fs::TryLockError::WouldBlock)));
    }
    drop(reads);
    for writer in &writers {
        #[expect(
            clippy::disallowed_methods,
            reason = "dropping the batch releases every source"
        )]
        writer.try_lock().unwrap();
        writer.unlock().unwrap();
    }
}

#[test]
fn successor_replay_holds_vault_sources_only_during_the_local_open() {
    use std::{cell::Cell, rc::Rc};

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (scope, expected) = data(&lab);
    let original = lab.paths[0].clone();
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let path = Directory::new(&next, bootstrap.trust())
        .path(scope)
        .unwrap();
    let before = files(&path);
    let writers = Rc::new(
        [&original, &next].map(|paths| private_file::open_private(&paths.dir, ".lock").unwrap().0),
    );
    let acquired = Rc::new([Cell::new(false), Cell::new(false)]);
    let calls = Rc::new(Cell::new(0));
    let result = {
        let writers = writers.clone();
        let acquired = acquired.clone();
        let calls = calls.clone();
        crate::table::store::with_replay_observer(
            move || {
                calls.set(calls.get() + 1);
                for (writer, acquired) in writers.iter().zip(acquired.iter()) {
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "the contender must not obtain an exclusive vault lock during replay"
                    )]
                    let attempt = writer.try_lock();
                    match attempt {
                        Ok(()) => acquired.set(true),
                        Err(std::fs::TryLockError::WouldBlock) => {}
                        Err(error) => panic!("vault contender failed: {error}"),
                    }
                }
            },
            || Store::observe(&session, &path, scope),
        )
    };
    for (writer, acquired) in writers.iter().zip(acquired.iter()) {
        if acquired.get() {
            writer.unlock().unwrap();
        }
    }
    assert_eq!(calls.get(), 1);
    let store =
        result.expect("a concurrent vault writer cannot interrupt an admitted local replay");
    assert!(acquired.iter().all(|held| !held.get()));
    assert_eq!(
        store
            .inventory(&session)
            .unwrap()
            .copied()
            .collect::<Vec<_>>(),
        vec![expected]
    );
    assert_eq!(files(&path), before);

    // Neither the session nor the returned store retains the read guard.
    for writer in writers.iter() {
        #[expect(
            clippy::disallowed_methods,
            reason = "the local open must have released its shared lock"
        )]
        writer
            .try_lock()
            .expect("vault writer can proceed after replay");
        writer.unlock().unwrap();
    }
    fsx::write_atomic(&selection.config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    assert!(store.inventory(&session).is_err());
    assert_eq!(files(&path), before);
}

#[test]
fn comparison_never_repairs_an_ancestor_while_selecting_a_completed_successor() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    let source = lab._home.path().join("comparison.csv");
    std::fs::write(&source, "email,google\nprivate@example.test,1\n").unwrap();
    let db = crate::import::commons::Imports::at(original.clone());
    db.apply(&source, [81; 16]).unwrap();
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let before = files(lab._home.path());
    let report = db.compare(&source).unwrap();
    assert_eq!(
        report["comparisons"][0]["exact_candidate_match"],
        json!(true)
    );
    assert_eq!(report["history_complete"], json!(true));
    assert_eq!(files(lab._home.path()), before);
    assert_eq!(selected(&original).unwrap(), next);

    let before = files(lab._home.path());
    assert!(
        crate::import::commons::Imports::at_until(original.clone(), Instant::now())
            .compare(&source)
            .is_err()
    );
    assert_eq!(files(lab._home.path()), before);
    let saved_complete = selection.complete;
    selection.complete = None;
    selection.save().unwrap();
    let before = files(lab._home.path());
    assert!(db.compare(&source).is_err());
    assert_eq!(files(lab._home.path()), before);
    selection.complete = saved_complete;
    selection.save().unwrap();
    let config = std::fs::read(&selection.config).ok();
    fsx::write_atomic(&selection.config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    let before = files(lab._home.path());
    assert!(db.compare(&source).is_err());
    assert_eq!(files(lab._home.path()), before);
    match config {
        Some(bytes) => fsx::write_atomic(&selection.config, &bytes, 0o600).unwrap(),
        None => std::fs::remove_file(&selection.config).unwrap(),
    }
    assert_eq!(
        db.compare(&source).unwrap()["history_complete"],
        json!(true)
    );

    let mut violations = Vec::new();
    for name in ["commons.lock", "commons.claim"] {
        let path = original.dir.join(name);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let before = files(lab._home.path());
        let result = db.compare(&source);
        let changed = files(lab._home.path()) != before;
        if result.is_ok() || changed {
            violations.push((name, result.is_ok(), changed));
        }
        // Restore only this fixture's saved bytes after recording the observation.
        fsx::write_atomic(&path, &bytes, 0o600).unwrap();
        assert_eq!(
            db.compare(&source).unwrap()["history_complete"],
            json!(true)
        );
    }
    assert!(
        violations.is_empty(),
        "(ancestor file, accepted, changed): {violations:?}"
    );
}

#[test]
fn successor_replay_refuses_withdrawal_before_return_or_tail_finalization() {
    for read_only in [true, false] {
        let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
        let (scope, original_id) = data(&lab);
        let mut selection = reserved(&lab);
        let next = move_admitted_candidate(&mut lab);
        import(&mut selection).unwrap();
        let bootstrap = lab.bootstrap(1);
        let session = Session::open(&bootstrap).unwrap();
        let path = Directory::new(&next, bootstrap.trust())
            .path(scope)
            .unwrap();
        let claim_dir = crate::table::store::claim_dir(&session).unwrap();
        let claim_path = claim_dir.join(crate::table::store::claim_name(scope));
        let acknowledged = std::fs::read(&claim_path).unwrap();
        let mut store = Store::open(&session, &path, scope).unwrap();
        let second_id = store
            .commit_catalog(&session, b"valid crash tail from the successor")
            .unwrap()
            .id;
        drop(store);
        // The table has one valid durable append beyond its independent claim,
        // as after a crash before publishing the next custody witness.
        fsx::write_atomic(&claim_path, &acknowledged, 0o600).unwrap();
        let snapshots = std::rc::Rc::new(std::cell::RefCell::new(None));
        let before_replay = snapshots.clone();
        let data_path = path.clone();
        let claims_path = claim_dir.clone();
        let source_config = selection.config.clone();
        let refused = crate::table::store::with_replay_observer(
            move || {
                *before_replay.borrow_mut() = Some((files(&data_path), files(&claims_path)));
                fsx::write_atomic(&source_config, b"[commons]\nenabled = false\n", 0o600).unwrap();
            },
            || {
                if read_only {
                    Store::observe(&session, &path, scope)
                } else {
                    Store::open(&session, &path, scope)
                }
            },
        );
        assert!(refused.is_err(), "withdrawal during replay must refuse");
        let (before_data, before_claims) = snapshots
            .borrow_mut()
            .take()
            .expect("the withdrawal must occur after journal admission");
        for (before, after) in [
            (before_data, files(&path)),
            (before_claims, files(&claim_dir)),
        ] {
            assert_eq!(
                after.keys().collect::<Vec<_>>(),
                before.keys().collect::<Vec<_>>()
            );
            for (name, bytes) in before {
                assert!(
                    after.get(&name) == Some(&bytes),
                    "refused replay changed {} (read_only={read_only})",
                    name.display()
                );
            }
        }

        fsx::write_atomic(&selection.config, b"[commons]\nenabled = true\n", 0o600).unwrap();
        let recovered = Store::open(&session, &path, scope).unwrap();
        let ids = recovered
            .inventory(&session)
            .unwrap()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids, [original_id, second_id].into_iter().collect());
    }
}

#[test]
fn account_service_observation_never_repairs_a_selected_generations_ancestor() {
    use aterm_types::network_state::ServiceAvailability;

    for name in ["commons.lock", "commons.claim"] {
        let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
        let original = lab.paths[0].clone();
        let proposals = crate::accounts::proposals::Proposals::at(original.clone());
        proposals.initialize().unwrap();
        let mut selection = reserved(&lab);
        move_admitted_candidate(&mut lab);
        import(&mut selection).unwrap();
        assert_eq!(
            crate::service_state::accounts(&original, &selection.config, 1000).availability,
            ServiceAvailability::Cached
        );
        let path = original.dir.join(name);
        std::fs::rename(&path, path.with_extension("preserved")).unwrap();
        let before = files(lab._home.path());

        let observed = crate::service_state::accounts(&original, &selection.config, 2000);
        assert!(
            files(lab._home.path()) == before,
            "account observation repaired ancestor {name}"
        );
        assert_eq!(observed.availability, ServiceAvailability::Unknown);
        assert!(observed.refresh_failed);
        assert_eq!(observed.last_success_ms, None);
    }
}

/// A holder of a predecessor's custody stops a read when the wait for a holder ends, however long
/// the read has left to read, at the one point the read takes that custody, after the selected
/// home's. The wait is spent here, so a read that does wait reaches the lock-wait observer, which
/// lets the holder go: that read then finds the lock free and reads Cached, rather than sitting
/// out the hang detector.
#[test]
fn a_held_predecessor_is_busy_when_the_wait_ends_not_when_the_read_does() {
    use crate::service_state::{accounts_until, control_busy, tests::after_custody};
    use aterm_types::network_state::ServiceAvailability;
    use aterm_uds::private_file::PrivateLock;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    crate::accounts::proposals::Proposals::at(original.clone())
        .initialize()
        .unwrap();
    let mut selection = reserved(&lab);
    move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let hang_detector = || Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    for lock in ["commons.lock", "commons-device.lock"] {
        let holder = Rc::new(RefCell::new(None));
        after_custody({
            let (holder, dir) = (holder.clone(), original.dir.clone());
            move || {
                let held = PrivateLock::take(&dir, lock, Duration::ZERO, "held").unwrap();
                *holder.borrow_mut() = Some(held);
            }
        });
        let waited = Rc::new(Cell::new(false));
        let observer = {
            let (holder, waited) = (holder.clone(), waited.clone());
            move |_: &std::fs::File| {
                waited.set(true);
                drop(holder.borrow_mut().take());
            }
        };
        let read = aterm_uds::file_lock::with_lock_wait_observer(observer, || {
            accounts_until(
                &original,
                &selection.config,
                1000,
                &Instant::now,
                hang_detector(),
            )
        });
        assert!(!waited.get(), "{lock}: waited past a spent wait");
        assert_eq!(read, control_busy(1000), "{lock}");
        assert!(holder.borrow_mut().take().is_some(), "{lock}: never held");
    }
    // With nothing held, the same spent wait reads: only a holder uses it up, on any load.
    let read = accounts_until(
        &original,
        &selection.config,
        2000,
        &Instant::now,
        hang_detector(),
    );
    assert_eq!(read.availability, ServiceAvailability::Cached, "{read:?}");
}

#[test]
fn recovery_diagnostics_preserve_missing_custody() {
    type Read = fn(&Paths) -> Result<Value>;
    let readers: [(&str, Read); 2] = [
        ("status", |paths| {
            status(paths, admission_deadline()).map(|(report, _)| report)
        }),
        ("inspect", inspect),
    ];
    let mut violations = Vec::new();
    for (name, read) in readers {
        for predecessor in [false, true] {
            for file in ["commons.lock", "commons.claim"] {
                let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
                data(&lab);
                let mut selection = reserved(&lab);
                let next = move_admitted_candidate(&mut lab);
                import(&mut selection).unwrap();
                let paths = &lab.paths[0];
                read(paths).unwrap();
                let target = if predecessor { paths } else { &next };
                let path = target.dir.join(file);
                std::fs::rename(&path, path.with_extension("preserved")).unwrap();
                let before = files(lab._home.path());
                let _ = read(paths);
                if files(lab._home.path()) != before {
                    violations.push((name, predecessor, file));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "(diagnostic, predecessor, file): {violations:?}"
    );
}

#[test]
fn observed_lineage_never_repairs_custody_or_runs_recovery() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.bootstrap(0).request().unwrap().binding().clone();
    let replacement = lab.bootstrap(1).request().unwrap().binding().clone();
    data(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let before = files(lab._home.path());
    assert_eq!(
        lineage_until(&lab.paths[0], admission_deadline()).unwrap(),
        vec![original, replacement]
    );
    assert_eq!(files(lab._home.path()), before);
    assert!(lineage_until(&lab.paths[0], Instant::now()).is_err());
    assert_eq!(files(lab._home.path()), before);

    let lock = lab.paths[0].dir.join(".lock");
    let inode = std::fs::metadata(&lock).unwrap().ino();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let before = files(lab._home.path());
    assert!(lineage_until(&lab.paths[0], admission_deadline()).is_err());
    assert_eq!(std::fs::metadata(&lock).unwrap().mode() & 0o777, 0o644);
    assert_eq!(std::fs::metadata(&lock).unwrap().ino(), inode);
    assert_eq!(files(lab._home.path()), before);
    // No repair or selection rewrite was delegated to the supplied successor.
    assert_eq!(next, lab.paths[1]);
}

#[test]
fn a_running_successor_materializes_later_vault_values_without_a_manual_sync() {
    use crate::runtime::{Diagnostic, Runtime, Status};
    use aterm_secrets::text::Audience;

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    let admin = client::admin_open(&original).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "ongoing-input", b"inert-before")
        .unwrap();
    client::admin_publish(&original, &admin, &doc).unwrap();
    client::sync(&lab.paths[1], None).unwrap();
    data(&lab);
    changed_device(&lab);
    let claims_before = files(&original.dir.join("commons/table-claims"));
    let records_before = files(&original.dir.join("commons/tables"));
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    client::sync(&next, None).unwrap();
    import(&mut selection).unwrap();
    let expected = client::config(&next).unwrap().unwrap();
    assert_eq!(
        client::read_verified_entry(&next, &expected, "ongoing-input")
            .unwrap()
            .unwrap()
            .value()
            .as_slice(),
        b"inert-before"
    );

    // Start the selected transport after fixture admission. The ALab enrollment
    // path cannot admit these generated test keys. Keep observation sockets
    // short without replacing the recovery selection's persistent policy.
    let observer = aterm_tempfile::Builder::new()
        .prefix("successor-upkeep-")
        .tempdir_in("/tmp")
        .unwrap();
    let (ready, started) = std::sync::mpsc::channel();
    let runtime = Runtime::start_diagnostic(
        next.clone(),
        selection.config.clone(),
        Diagnostic {
            observer_config: observer.path().join("aterm.toml"),
            recover_writer: false,
        },
        move |status| {
            let _ = ready.send(status);
        },
        |_| {},
    )
    .unwrap();
    assert!(matches!(
        started.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap(),
        Status::Offline | Status::Active { .. }
    ));

    let admin = client::admin_open(&original).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "ongoing-input", b"inert-after")
        .unwrap();
    client::admin_publish(&original, &admin, &doc).unwrap();
    let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let updated = loop {
        let updated = client::read_verified_entry(&next, &expected, "ongoing-input")
            .unwrap()
            .is_some_and(|entry| entry.value().as_slice() == b"inert-after");
        if updated || Instant::now() >= until {
            break updated;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    runtime.stop();
    assert!(
        updated,
        "a running successor never applied the later vault value"
    );
    assert_eq!(
        files(&original.dir.join("commons/table-claims")),
        claims_before
    );
    assert_eq!(files(&original.dir.join("commons/tables")), records_before);
}

#[test]
fn a_changed_device_succeeds_with_fresh_admitted_writer_and_exact_old_operations() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (scope, id) = data(&lab);
    changed_device(&lab);
    let original = lab.bootstrap(0);
    let old_session = Session::observe(&original).unwrap();
    let old_directory = Directory::new(&lab.paths[0], original.trust());
    assert!(Store::observe(&old_session, &old_directory.path(scope).unwrap(), scope).is_err());
    let claims_before = files(&lab.paths[0].dir.join("commons/table-claims"));
    let records_before = files(&lab.paths[0].dir.join("commons/tables"));
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    let fresh = lab.bootstrap(1);
    let fresh_session = Session::open(&fresh).unwrap();
    assert_ne!(fresh_session.actor(), old_session.actor());
    assert!(
        fresh_session.writer().is_err(),
        "admission alone cannot skip salvage"
    );
    drop(fresh_session);
    assert!(crate::application_paths(&lab.paths[0]).is_err());

    let inspected = crate::inspect_writer_recovery(&lab.paths[0]).unwrap();
    assert_eq!(inspected["mutated"], aterm_json::json!(false));
    let tables = inspected["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 2);
    assert!(
        tables
            .iter()
            .all(|table| table["authenticated_records"].as_u64() == Some(1))
    );
    assert!(
        !inspected
            .to_string()
            .contains("retained before device-number change")
    );
    assert!(!next.dir.join("commons/tables").exists());
    assert_eq!(
        files(&lab.paths[0].dir.join("commons/table-claims")),
        claims_before
    );
    assert_eq!(
        files(&lab.paths[0].dir.join("commons/tables")),
        records_before
    );

    assert!(
        import_while(&mut selection, &|| {
            if next.dir.join("commons/table-claims").exists() {
                Err(Error::new("test cancellation after candidate enrollment"))
            } else {
                Ok(())
            }
        })
        .is_err()
    );
    assert!(
        selected(&lab.paths[0]).is_err(),
        "a cancelled import never publishes a writer"
    );

    import(&mut selection).unwrap();
    assert_eq!(crate::application_paths(&lab.paths[0]).unwrap(), next);
    assert_eq!(
        files(&lab.paths[0].dir.join("commons/table-claims")),
        claims_before
    );
    assert_eq!(
        files(&lab.paths[0].dir.join("commons/tables")),
        records_before
    );
    let session = Session::open(&fresh).unwrap();
    session.writer().unwrap();
    let directory = Directory::new(&next, fresh.trust());
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert!(store.receipt(&session, &id).unwrap().is_some());
    let retained = store.catalog(&session).unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].header().id, id);
    assert_eq!(
        retained[0].payload(),
        b"retained before device-number change"
    );
    store
        .commit_catalog(&session, b"new writer operation")
        .unwrap();
    assert_eq!(store.catalog(&session).unwrap().len(), 2);
    assert_eq!(directory.catalog_scope(), old_directory.catalog_scope());
    drop(store);
    drop(session);
    import(&mut selection).unwrap();
    let session = Session::observe(&fresh).unwrap();
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert_eq!(
        store.catalog(&session).unwrap().len(),
        2,
        "retry neither loses nor duplicates data"
    );
}

#[test]
fn credential_needs_keep_the_logical_mac_ledger_after_writer_succession() {
    use aterm_secrets::need::{self, Purpose, Want};
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    let original = lab.paths[0].clone();
    need::test_alab_policy(&original);
    let before = need::raise(
        &original,
        Want::Mailbox("before-succession".into()),
        Purpose::ByHand,
    )
    .unwrap();
    let old_actor = lab.bootstrap(0).request().unwrap().binding().actor();
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    need::test_alab_policy(&next);
    import(&mut selection).unwrap();
    assert_eq!(crate::application_paths(&original).unwrap(), next);
    let new_actor = lab.bootstrap(1).request().unwrap().binding().actor();
    assert_ne!(old_actor, new_actor);

    let pass = crate::credentials::pass(&original).unwrap();
    assert_eq!(
        pass.posted, 1,
        "the already raised local need must be posted"
    );
    let key = crate::credentials::request_key(&new_actor, &before.id);
    let observed = crate::credentials::observe(&original).unwrap();
    assert_eq!(observed.book.threads[&key].requester, new_actor);
    assert_eq!(observed.book.threads[&key].want, before.want);
    let before_read = files(lab._home.path());
    let selected_observed = crate::credentials::observe(&next).unwrap();
    assert_eq!(selected_observed.observer, observed.observer);
    assert_eq!(selected_observed.book.threads[&key].requester, new_actor);
    assert_eq!(selected_observed.book.threads[&key].want, before.want);
    assert_eq!(files(lab._home.path()), before_read);
    assert_ne!(key, crate::credentials::request_key(&old_actor, &before.id));
    assert!(need::peek(&original).unwrap()[0].posted);

    // Consumers and the CLI keep raising needs through this Mac's logical home.
    let words = ["mailbox:after-succession".to_owned()];
    crate::credentials::cli::run(&original, &words).unwrap();
    let needs = need::peek(&original).unwrap();
    assert_eq!(needs.len(), 2);
    let observed = crate::credentials::observe(&original).unwrap();
    for need in needs {
        assert!(need.posted);
        let key = crate::credentials::request_key(&new_actor, &need.id);
        let thread = &observed.book.threads[&key];
        assert_eq!(thread.requester, new_actor);
        assert_eq!(thread.want, need.want);
    }

    let mut violations = Vec::new();
    for name in ["commons.lock", "commons.claim"] {
        let path = original.dir.join(name);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let before_read = files(lab._home.path());
        let result = crate::credentials::observe(&next);
        let changed = files(lab._home.path()) != before_read;
        if result.is_ok() || changed {
            violations.push((name, result.is_ok(), changed));
        }
        fsx::write_atomic(&path, &bytes, 0o600).unwrap();
    }
    assert!(
        violations.is_empty(),
        "(ancestor file, accepted, changed): {violations:?}"
    );
}

#[test]
fn recovered_credentials_reach_the_logical_home_and_preserve_the_other_audiences() {
    for preposted in [false, true] {
        recovered_credential_into_logical_home(preposted);
    }
}

fn recovered_credential_into_logical_home(preposted: bool) {
    use aterm_secrets::{
        need::{self, Purpose, Want},
        text::Audience,
    };
    let mut lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let original = lab.paths[0].clone();
    let admin = client::admin_open(&original).unwrap();
    let mut doc = admin.doc.clone();
    let holder = Audience::Hosts(vec!["m3".into()]);
    doc.set_entry(&holder, "recovered_password", b"scratch-recovered-password")
        .unwrap();
    doc.set_entry(&holder, "holder-only", b"scratch-stays-with-holder")
        .unwrap();
    client::admin_publish(&original, &admin, &doc).unwrap();
    for paths in &lab.paths {
        client::sync(paths, None).unwrap();
    }
    let want = Want::Mailbox("recovered".into());
    let now = 1_000_000_000;
    if preposted {
        need::raise(&original, want.clone(), Purpose::ByHand).unwrap();
        assert_eq!(
            crate::credentials::pass_at(&original, now).unwrap().posted,
            1
        );
    }
    data(&lab);
    changed_device(&lab);
    let claims_before = files(&original.dir.join("commons/table-claims"));
    let records_before = files(&original.dir.join("commons/tables"));
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    need::test_alab_policy(&next);
    import(&mut selection).unwrap();
    assert_eq!(crate::application_paths(&original).unwrap(), next);

    need::raise(&original, want.clone(), Purpose::ByHand).unwrap();
    assert_eq!(
        crate::credentials::pass_at(&original, now).unwrap().posted,
        1
    );
    let custodian = crate::credentials::custodian_pass_at(&original, now)
        .unwrap()
        .unwrap();
    // An already posted original request remains preserved beside its new
    // successor. Both publications must resolve to the same logical recipient.
    assert_eq!(custodian.published.len(), if preposted { 2 } else { 1 });
    assert_eq!(
        crate::credentials::pass_at(&original, now).unwrap().decided,
        1
    );
    client::sync(&original, None).unwrap();
    assert_eq!(
        fsx::read_private(&original.home.join(".secrets/recovered_password"), 1024)
            .unwrap()
            .unwrap()
            .as_slice(),
        b"scratch-recovered-password"
    );
    assert!(!original.home.join(".secrets/holder-only").exists());
    assert!(!next.home.join(".secrets/recovered_password").exists());
    let published = client::admin_open(&original).unwrap();
    assert!(
        published
            .doc
            .entry(&Audience::Hosts(vec!["m1".into()]), "recovered_password")
            .is_some()
    );
    assert!(
        published
            .doc
            .entry(&Audience::Hosts(vec!["m2".into()]), "recovered_password")
            .is_none()
    );
    assert!(published.doc.entry(&holder, "holder-only").is_some());
    assert_eq!(
        need::satisfied(&original, &want, Purpose::ByHand)
            .unwrap()
            .unwrap()
            .state,
        need::State::Fulfilled
    );
    assert_eq!(
        crate::credentials::pass_at(&original, now)
            .unwrap()
            .answered,
        1
    );
    assert_eq!(
        files(&original.dir.join("commons/table-claims")),
        claims_before
    );
    assert_eq!(files(&original.dir.join("commons/tables")), records_before);
}

#[test]
fn recovered_account_preparation_reads_the_newly_granted_recipient() {
    use crate::accounts::{activations::Activations, proposals::Proposals};
    use crate::database::Change;
    use aterm_secrets::{commons::accounts::Action, text::Audience};

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    let admin = client::admin_open(&original).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "account-input", b"inert-account-input")
        .unwrap();
    client::admin_publish(&original, &admin, &doc).unwrap();
    client::sync(&lab.paths[1], None).unwrap();
    let expected = client::config(&original).unwrap().unwrap();
    Proposals::at(original.clone())
        .write(
            [101; 16],
            vec![Change {
                collection: "accounts".into(),
                key: "rotation".into(),
                value: Some(aterm_json::json!({
                    "provider":"anthropic", "method":"oauth", "label":"rotation",
                    "secret_ref":{"vault":expected.name,"entry":"account-input"}
                })),
            }],
        )
        .unwrap();
    let db = Activations::at(original.clone());
    let account = [81; 32];
    let admin = client::admin_open(&original).unwrap();
    let old_actor = lab.bootstrap(0).request().unwrap().binding().actor();
    let request = db
        .configuration(&admin, account, "rotation", old_actor, vec![])
        .unwrap();
    let (old, _) = db.issue(&admin, [102; 16], request).unwrap();
    assert!(
        db.prepare(account, &expected)
            .unwrap()
            .credential()
            .is_some()
    );

    changed_device(&lab);
    let claims_before = files(&original.dir.join("commons/table-claims"));
    let records_before = files(&original.dir.join("commons/tables"));
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    client::sync(&next, None).unwrap();
    import(&mut selection).unwrap();
    assert_eq!(crate::application_paths(&original).unwrap(), next);
    assert!(db.prepare(account, &expected).is_err());

    let admin = client::admin_open(&original).unwrap();
    let binding = lab.bootstrap(1).request().unwrap().binding().clone();
    let request = db
        .configuration(&admin, account, "rotation", binding.actor(), vec![old.id()])
        .unwrap();
    let (current, _) = db.issue(&admin, [103; 16], request).unwrap();
    let Action::Prepare(terms) = &current.request().action else {
        unreachable!()
    };
    assert_eq!(terms.device, binding);
    let next_config = client::config(&next).unwrap().unwrap();
    assert_ne!(next_config.host, expected.host);
    assert!(
        client::read_account_bound_entry(&original, &expected, &current)
            .unwrap()
            .is_none(),
        "the new grant does not transfer to the old recipient"
    );
    assert!(
        client::read_account_bound_entry(&next, &next_config, &current)
            .unwrap()
            .is_some()
    );
    assert!(
        Activations::at(next.clone())
            .prepare(account, &next_config)
            .unwrap()
            .credential()
            .is_some()
    );

    let prepared = db.prepare(account, &expected).unwrap();
    assert_eq!(prepared.decision().id(), current.id());
    assert_eq!(
        prepared.credential().unwrap().value().as_slice(),
        b"inert-account-input"
    );
    assert!(db.prepare(account, &next_config).is_err());
    let stop = db.stop_request(account, vec![current.id()]).unwrap();
    db.issue(&admin, [104; 16], stop).unwrap();
    assert!(db.prepare(account, &expected).is_err());
    assert_eq!(
        files(&original.dir.join("commons/table-claims")),
        claims_before
    );
    assert_eq!(files(&original.dir.join("commons/tables")), records_before);
}

#[test]
fn succession_preserves_activation_pins_without_inheriting_the_old_fixed_share() {
    use crate::resources::{Allocation, Proposal, Resources, Terms};
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    let bootstrap = lab.bootstrap(0);
    let old_actor = bootstrap.request().unwrap().binding().actor();
    let proposed = Proposal::prepare(
        bootstrap.trust(),
        Terms {
            resource: [51; 32],
            window: [52; 32],
            unit: "requests".into(),
            limit: 3,
            shares: BTreeMap::from([(old_actor, 3)]),
        },
    )
    .unwrap();
    let allocation =
        Allocation::verify(proposed.bytes(), proposed.fingerprint(), bootstrap.trust()).unwrap();
    let original = lab.paths[0].clone();
    let resources = Resources::at(original.clone(), allocation.clone());
    resources.initialize().unwrap();
    let spent = resources.reserve([1; 16], 2).unwrap().receipt;
    let pins = files(&original.dir.join("commons/resource-pins"));
    changed_device(&lab);
    let claims_before = files(&original.dir.join("commons/table-claims"));
    let records_before = files(&original.dir.join("commons/tables"));
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    assert_eq!(crate::application_paths(&original).unwrap(), next);

    // Application routing cannot silently rebind the old activation's custody.
    assert!(resources.initialize().is_err());
    assert!(resources.reserve([1; 16], 2).is_err());
    assert!(resources.reserve([2; 16], 1).is_err());
    assert_eq!(files(&original.dir.join("commons/resource-pins")), pins);

    // Even an explicit pin of the same allocation in the admitted successor
    // preserves the old debit and grants the new actor none of the old share.
    let successor = Resources::at(next.clone(), allocation);
    successor.initialize().unwrap();
    let before = successor.snapshot().unwrap();
    assert_eq!(before.used, BTreeMap::from([(old_actor, 2)]));
    assert!(successor.reserve([1; 16], 2).is_err());
    assert!(successor.reserve([2; 16], 1).is_err());
    let after = successor.snapshot().unwrap();
    assert_eq!(before.retained, after.retained);
    assert_eq!(before.used, after.used);
    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = Directory::new(&next, bootstrap.trust());
    let scope = successor.initialize().unwrap();
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert_eq!(
        store.receipt(&session, &spent.id).unwrap().unwrap().id,
        spent.id
    );
    assert_eq!(
        files(&original.dir.join("commons/table-claims")),
        claims_before
    );
    assert_eq!(files(&original.dir.join("commons/tables")), records_before);
    assert_eq!(files(&original.dir.join("commons/resource-pins")), pins);
}

#[test]
fn pending_crash_and_contention_reuse_one_candidate_without_old_actor_reuse() {
    let lab = Lab::new(&[Role::Writer]);
    data(&lab);
    changed_device(&lab);
    let first = reserved(&lab);
    let next = candidate(&lab.paths[0]);
    let fresh = client::machine_identity(&next).unwrap().recipient();
    let device = aterm_secrets::commons::bootstrap::Device::open(&next)
        .unwrap()
        .unwrap();
    let original = lab.bootstrap(0);
    let fresh_actor = device
        .request(original.trust().network, original.trust().authority)
        .unwrap()
        .binding()
        .actor();
    assert_ne!(fresh_actor, Session::observe(&original).unwrap().actor());
    // Crash after LINK/identity publication and before the selector publication.
    std::fs::remove_file(root(&lab.paths[0]).join(STATE)).unwrap();
    let reopened = reserved(&lab);
    assert_eq!(first.bytes(), reopened.bytes());
    assert_eq!(client::machine_identity(&next).unwrap().recipient(), fresh);
    let mut pending = reopened;
    assert!(import(&mut pending).is_err());
    assert!(selected(&lab.paths[0]).is_err());
    let lock = PrivateLock::take(&root(&lab.paths[0]), "lock", Duration::ZERO, "busy").unwrap();
    let before = files(&root(&lab.paths[0]));
    let error = advance(&lab.paths[0], &lab.paths[0].home.join("config.toml"))
        .expect_err("the original recovery still owns succession");
    assert!(
        error.is_busy(),
        "local succession contention must use the ordinary retry path: {error}"
    );
    assert_eq!(files(&root(&lab.paths[0])), before);
    assert!(PrivateLock::take(&root(&lab.paths[0]), "lock", Duration::ZERO, "busy").is_err());
    drop(lock);
    assert_eq!(
        Selection::read(&lab.paths[0]).unwrap().unwrap().bytes(),
        first.bytes()
    );
    assert!(
        !next.home.join(".aterm").exists(),
        "no original-home credential mutation"
    );
}

#[test]
fn malformed_succession_lock_is_not_temporary_contention() {
    let lab = Lab::new(&[Role::Writer]);
    data(&lab);
    changed_device(&lab);
    let selection = reserved(&lab);
    std::fs::create_dir(root(&lab.paths[0]).join("lock")).unwrap();
    let before = files(&root(&lab.paths[0]));
    let error = advance(&lab.paths[0], &lab.paths[0].home.join("config.toml"))
        .expect_err("a directory cannot stand in for the succession lock");
    assert!(
        !error.is_busy(),
        "damaged local custody must not be reported as a normal retry"
    );
    assert_eq!(files(&root(&lab.paths[0])), before);
    assert_eq!(
        Selection::read(&lab.paths[0]).unwrap().unwrap().bytes(),
        selection.bytes()
    );
}

#[test]
fn succession_cannot_change_an_empty_resource_windows_allocation() {
    use crate::resources::{Allocation, Proposal, Resources, Terms};
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    let bootstrap = lab.bootstrap(0);
    let proposed = Proposal::prepare(
        bootstrap.trust(),
        Terms {
            resource: [53; 32],
            window: [54; 32],
            unit: "requests".into(),
            limit: 3,
            shares: BTreeMap::from([(bootstrap.request().unwrap().binding().actor(), 3)]),
        },
    )
    .unwrap();
    let allocation =
        Allocation::verify(proposed.bytes(), proposed.fingerprint(), bootstrap.trust()).unwrap();
    let original = lab.paths[0].clone();
    Resources::at(original.clone(), allocation.clone())
        .initialize()
        .unwrap();
    let pins = files(&original.dir.join("commons/resource-pins"));
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let mut different = allocation.terms().clone();
    different.limit = 10;
    different.shares =
        BTreeMap::from([(lab.bootstrap(1).request().unwrap().binding().actor(), 10)]);
    let proposed = Proposal::prepare(allocation.trust(), different).unwrap();
    let replacement =
        Allocation::verify(proposed.bytes(), proposed.fingerprint(), allocation.trust()).unwrap();
    assert!(
        Resources::at(next.clone(), replacement)
            .initialize()
            .is_err(),
        "writer succession discarded the original window allocation"
    );
    let successor = Resources::at(next, allocation);
    successor.initialize().unwrap();
    assert!(successor.snapshot().unwrap().used.is_empty());
    assert!(successor.reserve([1; 16], 1).is_err());
    assert_eq!(files(&original.dir.join("commons/resource-pins")), pins);
}

#[test]
fn repeated_loss_preserves_each_generation_and_keeps_original_policy_authoritative() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let (scope, first) = data(&lab);
    changed_device(&lab);
    let mut first_selection = reserved(&lab);
    let middle = move_admitted_candidate(&mut lab);
    import(&mut first_selection).unwrap();
    let middle_bootstrap = lab.bootstrap(1);
    let middle_session = Session::open(&middle_bootstrap).unwrap();
    let middle_directory = Directory::new(&middle, middle_bootstrap.trust());
    let mut middle_store = Store::open(
        &middle_session,
        &middle_directory.path(scope).unwrap(),
        scope,
    )
    .unwrap();
    let second = middle_store
        .commit_catalog(&middle_session, b"between successive losses")
        .unwrap()
        .id;
    drop(middle_store);
    drop(middle_session);
    changed_device_at(&middle);
    let old_data = files(&middle.dir.join("commons/tables"));
    aterm_journal::create_private_dir_durable(&root(&middle)).unwrap();
    let mut second_selection =
        reserve(&middle, &first_selection.config, &middle_bootstrap).unwrap();
    let newest = move_admitted_to(&mut lab, 2, candidate(&middle));
    assert!(
        selected(&lab.paths[0]).is_err(),
        "a partial descendant must not become active"
    );
    import(&mut second_selection).unwrap();
    assert_eq!(selected(&lab.paths[0]).unwrap(), newest);
    assert!(guard(&middle, true).is_err());
    assert_eq!(files(&middle.dir.join("commons/tables")), old_data);
    let bootstrap = lab.bootstrap(2);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&newest, bootstrap.trust());
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    for id in [first, second] {
        assert!(store.receipt(&session, &id).unwrap().is_some());
    }
    store
        .commit_catalog(&session, b"latest generation writes")
        .unwrap();
    drop(store);
    drop(session);
    let original_claims = files(&lab.paths[0].dir.join("commons/table-claims"));
    changed_device_at(&lab.paths[0]);
    assert!(
        guard(&newest, true).is_err(),
        "a changed grandparent source fences direct writers too"
    );
    for (path, bytes) in original_claims {
        fsx::write_atomic(&path, &bytes, 0o600).unwrap();
    }
    guard(&newest, true).unwrap();
    fsx::write_atomic(
        &first_selection.config,
        b"[swarm]\nenabled = false\n",
        0o600,
    )
    .unwrap();
    assert!(guard(&newest, false).is_err());
    assert!(selected(&lab.paths[0]).is_err());
}

#[test]
fn a_corrupted_link_cannot_form_a_cycle_or_redirect_a_generation() {
    let lab = Lab::new(&[Role::Writer]);
    data(&lab);
    let selection = reserved(&lab);
    let next = candidate(&lab.paths[0]);
    let mut bad = selection.clone();
    bad.original = next.clone();
    fsx::write_atomic(&next.dir.join(LINK), &bad.bytes(), 0o600).unwrap();
    assert!(guard(&next, false).is_err());
    fsx::write_atomic(&next.dir.join(LINK), &selection.bytes(), 0o600).unwrap();
    guard(&next, false).unwrap();
    std::fs::remove_file(next.dir.join(LINK)).unwrap();
    assert!(
        guard(&next, false).is_err(),
        "lost origin metadata must never turn a successor into an independent writer"
    );
}

#[test]
fn missing_historical_key_keeps_import_pending_until_authenticated_handoff() {
    let mut lab = Lab::with_enrolled(&[Role::Writer, Role::Writer], 1);
    let (scope, id) = data(&lab);
    lab.admit(&[Role::Writer, Role::Writer]);
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    assert!(import(&mut selection).is_err());
    assert!(
        Selection::read(&lab.paths[0])
            .unwrap()
            .unwrap()
            .complete
            .is_none()
    );
    assert!(selected(&lab.paths[0]).is_err());
    let original = lab.bootstrap(0);
    let fresh = lab.bootstrap(1);
    let journal = aterm_secrets::commons::journal::Journal::open_existing(
        original.journal_dir(),
        original.trust(),
    )
    .unwrap();
    let access = original.access_in(&journal).unwrap().unwrap();
    let handoff = original
        .history_handoff(
            &journal,
            &access,
            original.trust().genesis,
            fresh.request().unwrap().binding(),
        )
        .unwrap();
    drop(journal);
    let journal =
        aterm_secrets::commons::journal::Journal::open_existing(fresh.journal_dir(), fresh.trust())
            .unwrap();
    let access = fresh.access_in(&journal).unwrap().unwrap();
    fresh
        .import_history(&journal, &access, handoff.bytes())
        .unwrap();
    drop(journal);
    import(&mut selection).unwrap();
    assert_eq!(selected(&lab.paths[0]).unwrap(), next);
    let session = Session::observe(&fresh).unwrap();
    let directory = Directory::new(&next, fresh.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert!(store.receipt(&session, &id).unwrap().is_some());
}

#[test]
fn a_later_missing_history_key_keeps_earlier_imported_receipts_and_resumes_exactly() {
    let mut lab = Lab::with_enrolled(&[Role::Writer, Role::Writer, Role::Writer], 2);
    let author = lab.bootstrap(1);
    let descriptor = Descriptor::application(author.trust(), "succession", "partial", 1).unwrap();
    let scope = descriptor.scope;
    let directory = Directory::new(&lab.paths[1], author.trust());
    let session = Session::open(&author).unwrap();
    directory.publish(&session, &descriptor).unwrap();
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let older = store
        .commit_catalog(&session, b"older retained data")
        .unwrap()
        .id;
    let old_wire = store.wire(&session, &older).unwrap().unwrap().to_vec();
    drop((store, session));

    lab.admit(&[Role::Writer, Role::Writer, Role::Writer]);
    let author = lab.bootstrap(1);
    let session = Session::open(&author).unwrap();
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let newer = store
        .commit_catalog(&session, b"newer retained data")
        .unwrap()
        .id;
    let new_wire = store.wire(&session, &newer).unwrap().unwrap().to_vec();
    drop((store, session));

    // Receipt order need not be causal order. The first preserved ciphertext
    // is readable to the later member; the next needs an older history key.
    let original = lab.bootstrap(0);
    let session = Session::open(&original).unwrap();
    let directory = Directory::new(&lab.paths[0], original.trust());
    directory.publish(&session, &descriptor).unwrap();
    let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
    store.ingest(&session, &new_wire).unwrap();
    store.ingest(&session, &old_wire).unwrap();
    drop((store, session));
    changed_device(&lab);
    let claims_before = files(&lab.paths[0].dir.join("commons/table-claims"));
    let records_before = files(&lab.paths[0].dir.join("commons/tables"));
    let mut selection = reserved(&lab);
    let next = move_admitted_to(&mut lab, 2, candidate(&selection.original));
    let fresh = lab.bootstrap(2);

    assert!(import(&mut selection).is_err());
    assert!(selected(&lab.paths[0]).is_err());
    let session = Session::observe(&fresh).unwrap();
    let directory = Directory::new(&next, fresh.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert!(store.receipt(&session, &newer).unwrap().is_some());
    assert!(store.receipt(&session, &older).unwrap().is_none());
    assert_eq!(store.inventory(&session).unwrap().count(), 1);
    drop((store, session));

    let original = lab.bootstrap(0);
    let journal = Journal::open_existing(original.journal_dir(), original.trust()).unwrap();
    let access = original.access_in(&journal).unwrap().unwrap();
    let handoff = original
        .history_handoff(
            &journal,
            &access,
            original.trust().genesis,
            fresh.request().unwrap().binding(),
        )
        .unwrap();
    drop(journal);
    let journal = Journal::open_existing(fresh.journal_dir(), fresh.trust()).unwrap();
    let access = fresh.access_in(&journal).unwrap().unwrap();
    fresh
        .import_history(&journal, &access, handoff.bytes())
        .unwrap();
    drop(journal);

    import(&mut selection).unwrap();
    assert_eq!(selected(&lab.paths[0]).unwrap(), next);
    import(&mut selection).unwrap();
    let session = Session::observe(&fresh).unwrap();
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    for id in [older, newer] {
        assert!(store.receipt(&session, &id).unwrap().is_some());
    }
    assert_eq!(store.inventory(&session).unwrap().count(), 2);
    assert_eq!(
        files(&lab.paths[0].dir.join("commons/table-claims")),
        claims_before
    );
    assert_eq!(
        files(&lab.paths[0].dir.join("commons/tables")),
        records_before
    );
}

#[test]
fn original_off_revocation_or_new_receipts_withdraw_the_successor() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    guard(&next, true).unwrap();
    let held = Observation::capture(&next, admission_deadline()).unwrap();
    held.check(true, false, admission_deadline()).unwrap();
    fsx::write_atomic(&selection.config, b"[commons]\nenabled = false\n", 0o600).unwrap();
    assert!(held.check(false, false, admission_deadline()).is_err());
    assert!(guard(&next, false).is_err());
    assert!(selected(&lab.paths[0]).is_err());
    assert!(import(&mut selection).is_err());
    fsx::write_atomic(&selection.config, b"", 0o600).unwrap();
    guard(&next, true).unwrap();
    held.check(true, false, admission_deadline()).unwrap();
    // A newly witnessed source receipt must force another import. Altering a
    // retained commitment also models damage; neither keeps authoring enabled.
    let original = lab.bootstrap(0);
    let catalog = Directory::new(&lab.paths[0], original.trust()).catalog_scope();
    let claim = lab.paths[0].dir.join("commons/table-claims").join(format!(
        "{}.claim",
        aterm_codec::hex::encode_lower(&catalog.digest())
    ));
    let before = std::fs::read(&claim).unwrap();
    let mut changed = before.clone();
    *changed.last_mut().unwrap() ^= 1;
    fsx::write_atomic(&claim, &changed, 0o600).unwrap();
    assert!(held.check(true, false, admission_deadline()).is_err());
    assert!(guard(&next, true).is_err());
    assert!(import(&mut selection).is_err());
    fsx::write_atomic(&claim, &before, 0o600).unwrap();
    guard(&next, true).unwrap();
    held.check(true, false, admission_deadline()).unwrap();
    // The ordinary vault revocation must still fence this local successor even
    // though that successor has an independently admitted identity.
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let original = lab.bootstrap(0);
    let survivor = lab.bootstrap(1);
    let mut journal = aterm_secrets::commons::journal::Journal::open_existing(
        original.journal_dir(),
        original.trust(),
    )
    .unwrap();
    let grant = aterm_secrets::commons::roster::Grant::recorded(
        "m2",
        survivor.request().unwrap().binding().clone(),
        Role::Writer,
    );
    let removal = admin
        .prepare_commons_rotation(&journal, vec![grant])
        .unwrap();
    journal.ingest(removal.bytes()).unwrap();
    drop(journal);
    assert!(held.check(false, false, admission_deadline()).is_err());
    assert!(guard(&next, false).is_err());
    assert!(selected(&lab.paths[0]).is_err());
}

#[test]
fn healthy_or_disabled_installations_never_allocate_a_generation() {
    let lab = Lab::new(&[Role::Writer]);
    data(&lab);
    let config = lab.paths[0].home.join("config.toml");
    assert_eq!(advance(&lab.paths[0], &config).unwrap(), lab.paths[0]);
    assert!(!root(&lab.paths[0]).exists());
    changed_device(&lab);
    assert_eq!(advance(&lab.paths[0], &config).unwrap(), lab.paths[0]);
    assert!(
        !root(&lab.paths[0]).exists(),
        "custom networks keep their recovery policy"
    );
    fsx::write_atomic(&config, b"[swarm]\nenabled = false\n", 0o600).unwrap();
    assert!(advance(&lab.paths[0], &config).is_err());
    assert!(!root(&lab.paths[0]).exists());
}

#[test]
fn cancellation_or_elapsed_budget_before_reservation_creates_no_custody() {
    let lab = Lab::new(&[Role::Writer]);
    let config = lab.paths[0].home.join("config.toml");
    assert!(advance_while(&lab.paths[0], &config, &|| true).is_err());
    let budget = Budget {
        until: Instant::now(),
        cancelled: &|| false,
    };
    assert!(advance_at(&lab.paths[0], &config, 0, &budget).is_err());
    assert!(!root(&lab.paths[0]).exists());
}

#[test]
fn selecting_a_successor_serializes_with_original_writers_then_fences_raw_retention() {
    let lab = Lab::new(&[Role::Writer]);
    let (scope, id) = data(&lab);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let path = directory.path(scope).unwrap();
    let mut store = Store::open(&session, &path, scope).unwrap();
    let wire = store.wire(&session, &id).unwrap().unwrap().to_vec();
    let config = lab.paths[0].home.join("config.toml");
    aterm_journal::create_private_dir_durable(&root(&lab.paths[0])).unwrap();
    assert!(
        reserve_until(
            &lab.paths[0],
            &config,
            &bootstrap,
            Instant::now() + Duration::from_millis(25)
        )
        .is_err()
    );
    assert!(
        !fenced(&lab.paths[0]).unwrap(),
        "an empty staging directory is not a reservation"
    );
    session.writer().unwrap();
    store
        .commit_catalog(
            &session,
            b"failed reservation leaves ordinary writing available",
        )
        .unwrap();
    let before = files(&path);
    drop(store);
    drop(session);
    reserved(&lab);
    let session = Session::open(&bootstrap).unwrap();
    let reader = Session::observe(&bootstrap).unwrap();
    let mut store = Store::observe(&reader, &path, scope).unwrap();
    assert!(session.writer().is_err());
    assert!(
        store
            .commit_catalog(&session, b"old handle must stay fenced")
            .is_err()
    );
    assert!(store.ingest(&session, &wire).is_err());
    assert!(Store::open(&session, &path, scope).is_err());
    assert_eq!(files(&path), before);
    assert_eq!(
        store.catalog(&reader).unwrap().len(),
        2,
        "read-only salvage remains available"
    );
}

#[test]
fn lost_selector_keeps_original_fenced_and_reuses_the_completed_candidate() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let actor = lab.bootstrap(1).request().unwrap().binding().actor();
    let original = lab.bootstrap(0);
    let retained = Session::open(&original).unwrap();
    std::fs::remove_file(root(&lab.paths[0]).join(STATE)).unwrap();
    assert!(fenced(&lab.paths[0]).unwrap());
    assert_eq!(
        status(&lab.paths[0], admission_deadline()).unwrap().0["state"],
        json!("pending")
    );
    assert!(retained.writer().is_err());
    assert!(allow_mutation(&lab.paths[0]).is_err());
    assert!(
        guard(&next, false).is_err(),
        "incomplete selection is never used"
    );
    drop(retained);
    let mut resumed = reserved(&lab);
    assert_eq!(lab.bootstrap(1).request().unwrap().binding().actor(), actor);
    import(&mut resumed).unwrap();
    assert_eq!(selected(&lab.paths[0]).unwrap(), next);
    assert_eq!(lab.bootstrap(1).request().unwrap().binding().actor(), actor);
}

#[test]
fn held_original_or_candidate_device_custody_obeys_one_deadline_and_can_resume() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    data(&lab);
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let retained = files(&lab.paths[0].dir.join("commons/tables"));
    for paths in [&lab.paths[0], &next] {
        let held =
            PrivateLock::take(&paths.dir, "commons-device.lock", Duration::ZERO, "held").unwrap();
        let mut retry = selection.clone();
        std::thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let worker = scope.spawn(move || {
                let result = import_until(
                    &mut retry,
                    &|| Ok(()),
                    Instant::now() + Duration::from_millis(30),
                );
                send.send(result.is_err()).unwrap();
            });
            let refused = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
            // Always release before joining, even if an unbounded-lock mutant
            // failed to answer. A broken deadline cannot strand this test.
            drop(held);
            worker.join().unwrap();
            assert!(refused.unwrap());
        });
        import(&mut selection).unwrap();
        assert_eq!(selected(&lab.paths[0]).unwrap(), next);
    }
    assert_eq!(files(&lab.paths[0].dir.join("commons/tables")), retained);
}

/// An unchanged imported activation head cannot make an old proposal observation current.
#[test]
fn a_shared_application_read_refuses_a_completed_successor_with_the_same_decision() {
    use crate::{
        accounts::{activations::Activations, proposals::Proposals},
        database::Change,
    };
    use aterm_json::json;
    for after_read in [false, true] {
        let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
        let original = lab.paths[0].clone();
        let value = json!({"provider":"anthropic","method":"oauth","label":"before"});
        Proposals::at(original.clone())
            .write(
                [81; 16],
                vec![Change {
                    collection: "accounts".into(),
                    key: "one".into(),
                    value: Some(value),
                }],
            )
            .unwrap();
        let admin = client::admin_open(&original).unwrap();
        let db = Activations::at(original.clone());
        let bootstrap = lab.bootstrap(0);
        let request = db
            .configuration(
                &admin,
                [82; 32],
                "one",
                bootstrap.request().unwrap().binding().actor(),
                vec![],
            )
            .unwrap();
        let decision = db.issue(&admin, [83; 16], request).unwrap().0;
        assert_eq!(
            db.current_configuration([82; 32]).unwrap().0.id(),
            decision.id()
        );
        let session = Session::observe(&bootstrap).unwrap();
        let descriptor =
            Descriptor::application(session.trust(), "accounts", "proposals", 1).unwrap();
        let directory = Directory::new(&original, bootstrap.trust());
        let scope = descriptor.scope;
        let mut advance = || {
            // A healthy predecessor remains readable: no claim or table is damaged.
            let mut selection = reserved(&lab);
            let next = move_admitted_candidate(&mut lab);
            import(&mut selection).unwrap();
            assert_eq!(selected(&original).unwrap(), next);
            Proposals::at(next.clone())
                .write(
                    [84; 16],
                    vec![Change {
                        collection: "accounts".into(),
                        key: "one".into(),
                        value: Some(json!({"provider":"openai","method":"oauth","label":"after"})),
                    }],
                )
                .unwrap();
            next
        };
        let mut next = None;
        if !after_read {
            next = Some(advance());
        }
        let result = crate::documents::application::observe_store_in_session(
            &session,
            descriptor,
            admission_deadline(),
            |session, store, _| {
                let snapshot = Proposals::snapshot_from_store(session, store)?;
                if after_read {
                    next = Some(advance());
                }
                Ok(snapshot)
            },
        );
        assert!(
            result.is_err(),
            "a completed successor must invalidate the old shared read"
        );
        let next = next.unwrap();
        let current = Activations::at(next.clone());
        assert_eq!(
            current.state([82; 32]).unwrap(),
            crate::accounts::activations::State::Ready(decision.id())
        );
        assert!(
            current.current_configuration([82; 32]).is_err(),
            "the imported decision cannot approve changed proposals"
        );
        let fresh = Proposals::at(next).snapshot().unwrap();
        assert_eq!(
            fresh.cells[&("accounts".into(), "one".into())].candidates[0].value["label"].as_str(),
            Some("after")
        );
        // The ordinary historical read is intentionally still allowed.
        let historical = directory.observe_table(&session, scope).unwrap();
        let old = Proposals::snapshot_from_store(&session, &historical).unwrap();
        assert_eq!(
            old.cells[&("accounts".into(), "one".into())].candidates[0].value["label"].as_str(),
            Some("before")
        );
    }
}

#[test]
fn shared_application_observation_keeps_deadline_custody_and_catalog_refusals_read_only() {
    for refusal in 0..7 {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let (scope, _) = data(&lab);
        let bootstrap = lab.bootstrap(0);
        let session = Session::observe(&bootstrap).unwrap();
        let mut descriptor =
            Descriptor::application(session.trust(), "recovery-test", "records", 1).unwrap();
        let read = |session: &Session<'_>, store: &Store, _| Ok(store.catalog(session)?.len());
        assert_eq!(
            crate::documents::application::observe_store_in_session(
                &session,
                descriptor.clone(),
                admission_deadline(),
                read
            )
            .unwrap(),
            1
        );
        let deadline = match refusal {
            0 => {
                descriptor =
                    Descriptor::application(session.trust(), "absent", "records", 1).unwrap();
                admission_deadline()
            }
            1 => {
                descriptor.version = 2;
                admission_deadline()
            }
            2 => Instant::now(),
            3 => {
                std::fs::remove_file(lab.paths[0].dir.join("commons.claim")).unwrap();
                admission_deadline()
            }
            4 => {
                lab.rotate(&[Role::Writer, Role::Writer]);
                admission_deadline()
            }
            5 => {
                let directory = Directory::new(&lab.paths[0], bootstrap.trust());
                std::fs::remove_dir_all(directory.path(scope).unwrap()).unwrap();
                admission_deadline()
            }
            6 => {
                reserved(&lab);
                admission_deadline()
            }
            _ => unreachable!(),
        };
        let before = files(lab._home.path());
        assert!(
            crate::documents::application::observe_store_in_session(
                &session, descriptor, deadline, read
            )
            .is_err(),
            "refusal {refusal}"
        );
        assert_eq!(
            files(lab._home.path()),
            before,
            "refusal {refusal} must not enroll or repair"
        );
    }
}

/// A home or config path that is not UTF-8 (a Unix path is bytes) reads back from the selection
/// record as the same path, where its null text alone would read as no record at all. A UTF-8
/// path's record is its text alone, unchanged. A hex spelling of a UTF-8 path, or of one holding
/// a NUL, is refused, so each path has one spelling.
#[cfg(unix)]
#[test]
fn a_selection_whose_paths_are_not_utf8_reads_back_as_the_same_paths() {
    use std::os::unix::ffi::OsStrExt as _;
    let text = Selection {
        original: Paths::for_home(Path::new("/tmp/aterm-home")),
        config: PathBuf::from("/tmp/aterm-config.toml"),
        actor: "a1".to_owned(),
        trust: "t1".to_owned(),
        complete: None,
    };
    let encoded = text.bytes();
    assert!(
        !String::from_utf8_lossy(&encoded).contains("_hex"),
        "{}",
        String::from_utf8_lossy(&encoded)
    );
    assert_eq!(Selection::parse(&encoded).unwrap().bytes(), encoded);
    let mut raw = text.clone();
    raw.original = Paths::for_home(Path::new(std::ffi::OsStr::from_bytes(
        b"/tmp/aterm-home-\xff",
    )));
    raw.config = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/aterm-config-\xfe.toml"));
    let encoded = raw.bytes();
    let back = Selection::parse(&encoded).expect("a non-UTF-8 path reads back");
    assert_eq!(back.original, raw.original);
    assert_eq!(back.config, raw.config);
    assert_eq!(back.bytes(), encoded);
    let mut record: Value = aterm_json::from_slice(&encoded).unwrap();
    for forged in [&b"/tmp/aterm-home"[..], b"/tmp/aterm-\0\xff", b""] {
        record.as_object_mut().unwrap().insert(
            "home_hex".to_owned(),
            Value::String(aterm_codec::hex::encode_lower(forged)),
        );
        assert!(
            Selection::parse(record.to_string().as_bytes()).is_err(),
            "{forged:?}"
        );
    }
}

/// After m1's writer succession its requests are delegated: the successor signs them and the
/// original Writer authorizes delivery to its own recipient (§21). The original actor stays a
/// current Writer but writes nothing more, so the window decision on m1's own request names m3,
/// the one Mac that did not answer, and never m1 itself.
/// NEGATIVE CONTROL: m3, which never swept it, is named.
#[test]
fn a_window_decision_after_succession_never_names_the_requesters_own_mac() {
    use aterm_secrets::need::{self, Purpose, Unanswered, Want};
    let mut lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let original = lab.paths[0].clone();
    data(&lab);
    changed_device(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    need::test_alab_policy(&next);
    import(&mut selection).unwrap();
    assert_eq!(crate::application_paths(&original).unwrap(), next);
    need::raise(&original, Want::Mailbox("absent".into()), Purpose::ByHand).unwrap();
    let now = aterm_time::unix_millis().unwrap();
    assert_eq!(
        crate::credentials::pass_at(&original, now).unwrap().posted,
        1
    );
    // A minute past the window, so the request's own receipt, later than `now`, is inside it.
    let window_end = now + need::WANT_WINDOW_SECS * 1000 + 60_000;
    let decided = crate::credentials::custodian_pass_at(&original, window_end)
        .unwrap()
        .unwrap();
    assert_eq!(decided.asked.len(), 1, "{decided:?}");
    let heard = crate::credentials::pass_at(&original, window_end).unwrap();
    let thread = &heard.threads[0];
    assert!(thread.origin.is_some(), "a delegated request");
    assert_eq!(
        thread.unanswered,
        Unanswered {
            hosts: vec!["m3".into()],
            others: 0
        }
    );
    let rows = need::list(&original).unwrap();
    let line = &rows[0].line;
    assert!(line.contains(", and m3 did not answer:"), "{line}");
}

/// A successor home's table read observes its predecessor once, after it has opened the
/// selected home's custody: the session keeps that observation ([`Session::observe_selected`]),
/// and its generation checks take none of the predecessor's custody. So a holder of the
/// predecessor's custody that comes after the observation keeps the read from nothing, though
/// every wait for a holder is already spent: only `read_by` is the read's gate. m14,
/// 2026-11-07: one registry read observed its predecessor twice per pass under one 2 s bound
/// and, on a loaded Mac, ran out of it.
/// NEGATIVE CONTROL: with `observe_selected` observing the predecessors again
/// (`Observation::observe_within(bootstrap.paths(), read_by, &Instant::now)`), the read meets the
/// held lock at once and is busy; with the session bounded by the spent wait instead of `read_by`,
/// it runs out of time.
#[test]
fn a_successor_homes_read_observes_its_predecessor_once() {
    use crate::documents::application::{observe_store_if_declared_within, seams};
    use std::{cell::RefCell, rc::Rc};

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    data(&lab);
    let mut selection = reserved(&lab);
    move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let read_by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    for lock in ["commons.lock", "commons-device.lock"] {
        let holder = Rc::new(RefCell::new(None));
        seams::after_selection({
            let (holder, dir) = (holder.clone(), original.dir.clone());
            move || {
                let held = PrivateLock::take(&dir, lock, Duration::ZERO, "held").unwrap();
                *holder.borrow_mut() = Some(held);
            }
        });
        let read = observe_store_if_declared_within(
            &original,
            |bootstrap| Descriptor::application(bootstrap.trust(), "recovery-test", "records", 1),
            read_by,
            &Instant::now,
            |session, store, _| Ok(store.catalog(session)?.len()),
        );
        assert!(holder.borrow_mut().take().is_some(), "{lock}: never held");
        assert_eq!(read.unwrap(), Some(1), "{lock}");
    }
}

/// The `recovery-test` table [`data`] declares, which holds one record.
fn records(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "recovery-test", "records", 1)
}

/// How many records `store` holds.
fn rows(session: &Session<'_>, store: &Store, _: Scope) -> Result<usize> {
    Ok(store.catalog(session)?.len())
}

/// A successor home on `lab`: the original's [`data`] and account proposals imported into its
/// candidate. Returns the data table's scope, the candidate home and the selection's config.
fn imported_successor(lab: &mut Lab) -> (Scope, Paths, std::path::PathBuf) {
    let (scope, _) = data(lab);
    crate::accounts::proposals::Proposals::at(lab.paths[0].clone())
        .initialize()
        .unwrap();
    let mut selection = reserved(lab);
    let next = move_admitted_candidate(lab);
    import(&mut selection).unwrap();
    (scope, next, selection.config.clone())
}

/// Each read-only reader of a successor home that names the home with no custody
/// ([`observed_generation_until`]) and opens a session observes the predecessors once, after it
/// opens the selected home's custody, as the label read does
/// ([`a_successor_homes_read_observes_its_predecessor_once`]): the account service read,
/// `observe_store_if_here`, `aterm db commons-tables` and `commons-inventory`, the account claims
/// read and the recovery pending read (`Session::observe_named_until`); and `commons-status`
/// reports `ready` from one verified selection and reads its home from it.
/// A holder of a predecessor's custody that comes as that one observation judges it neither stops
/// the read nor makes it wait: the account service read is given a spent wait, so observing again
/// would read busy, and the others wait for a holder, so observing again would wait on it, which
/// the lock-wait observer records and lets go. A holder that comes once the selected home's custody
/// is open and before the predecessors are observed is met, so each reader that keeps its session's
/// observation took it after that custody: the readers that wait, wait on it (the account service
/// read's case is [`a_held_predecessor_is_busy_when_the_wait_ends_not_when_the_read_does`]).
/// ATERM-KI-1107-1, 2026-11-08: these readers observed each predecessor again as their session
/// opened, and the account read a third time to judge a stale read.
/// NEGATIVE CONTROL (2026-11-08): on main before the change (b564cfbf88, this test's first moment
/// alone added), the account read's rows read busy (`Unknown`) and the commons-status,
/// commons-tables, commons-inventory and `observe_store_if_here` rows each waited on the holder;
/// the label read's rows passed.
/// NEGATIVE CONTROL (2026-11-09): with the account claims read and the recovery pending read
/// back on `generation::selected` and `Session::observe` / `observe_until` (90ed0753c0, their
/// two call sites alone reverted), their rows failed at the moment before they observe, never
/// held: they observed no predecessor after the selected home's custody opened. Their rows as
/// they observe passed: in this fixture their second observation met no holder the first had not.
#[test]
fn a_successor_homes_readers_observe_their_predecessor_once() {
    use crate::documents::application::{
        observe_store_if_declared_within, observe_store_if_here, seams::after_custody,
    };
    use crate::generation::seams::{Point, at};
    use aterm_types::network_state::ServiceAvailability;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    /// A reader: its name, whether it opens its session through `observed_session` (whose
    /// `after_custody` seam the second moment holds at), and the read, which says whether it
    /// reached its result and what it read.
    type Reader<'r> = (&'static str, bool, Box<dyn Fn() -> (bool, String) + 'r>);

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    let (scope, _, config) = imported_successor(&mut lab);
    let table = aterm_codec::hex::encode_lower(&scope.digest());
    let hang_detector = || Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let command = |name: &str, options: &[(&'static str, &str)]| {
        let mut options = options.iter().copied().collect();
        crate::catalog::command::run_at(name, &mut options, &original, &config)
    };
    let readers: [Reader<'_>; 8] = [
        (
            "label read",
            true,
            Box::new(|| {
                let read = observe_store_if_declared_within(
                    &original,
                    records,
                    hang_detector(),
                    &hang_detector,
                    rows,
                );
                (matches!(read, Ok(Some(1))), format!("{read:?}"))
            }),
        ),
        (
            "account service read",
            false,
            Box::new(|| {
                let read = crate::service_state::accounts_until(
                    &original,
                    &config,
                    1000,
                    &Instant::now,
                    hang_detector(),
                );
                (
                    read.availability == ServiceAvailability::Cached,
                    format!("{read:?}"),
                )
            }),
        ),
        (
            "commons-status",
            false,
            Box::new(|| {
                let read = crate::catalog::command::commons_status_until(
                    &original,
                    &config,
                    hang_detector(),
                );
                let ok = read.as_ref().is_ok_and(|status| {
                    status["writer_recovery"]["state"].as_str() == Some("ready")
                        && status["state"].as_str() != Some("writer-recovery-pending")
                });
                (ok, format!("{read:?}"))
            }),
        ),
        (
            "commons-tables",
            true,
            Box::new(|| {
                let read = command("commons-tables", &[]);
                let ok = read.as_ref().is_ok_and(|listing| {
                    listing["tables"].as_array().is_some_and(|tables| {
                        tables
                            .iter()
                            .any(|entry| entry["table"].as_str() == Some(table.as_str()))
                    })
                });
                (ok, format!("{read:?}"))
            }),
        ),
        (
            "commons-inventory",
            true,
            Box::new(|| {
                let read = command("commons-inventory", &[("--table", table.as_str())]);
                let ok = read.as_ref().is_ok_and(|inventory| {
                    inventory["kind"].as_str() == Some("commons-inventory")
                        && inventory["storage"].as_str() == Some("present")
                        && inventory["known"]["count"].as_u64() == Some(1)
                });
                (ok, format!("{read:?}"))
            }),
        ),
        (
            "observe_store_if_here",
            true,
            Box::new(|| {
                let read = observe_store_if_here(&original, records, Some(hang_detector()), rows);
                (matches!(read, Ok(Some(1))), format!("{read:?}"))
            }),
        ),
        (
            "account claims read",
            true,
            Box::new(|| {
                let read = crate::accounts::claims::read(&original);
                (read.is_ok(), format!("{read:?}"))
            }),
        ),
        (
            "recovery pending",
            true,
            Box::new(|| {
                let read = crate::accounts::recovery::Recovery::at(original.clone()).pending();
                (read.is_ok(), format!("{read:?}"))
            }),
        ),
    ];
    let mut outcomes = Vec::new();
    for lock in ["commons.lock", "commons-device.lock"] {
        for before in [false, true] {
            for (reader, observed_session, read) in &readers {
                if before && !observed_session {
                    continue;
                }
                let holder = Rc::new(RefCell::new(None));
                let held = Rc::new(Cell::new(false));
                let hold = {
                    let (holder, held, dir) = (holder.clone(), held.clone(), original.dir.clone());
                    move || {
                        let lock = PrivateLock::take(&dir, lock, Duration::ZERO, "held").unwrap();
                        *holder.borrow_mut() = Some(lock);
                        held.set(true);
                    }
                };
                if before {
                    after_custody(hold);
                } else {
                    at(Point::BeforeDenial, hold);
                }
                let waited = Rc::new(Cell::new(false));
                let observer = {
                    let (holder, waited) = (holder.clone(), waited.clone());
                    move |_: &std::fs::File| {
                        waited.set(true);
                        drop(holder.borrow_mut().take());
                    }
                };
                let (ok, detail) = aterm_uds::file_lock::with_lock_wait_observer(observer, read);
                drop(holder.borrow_mut().take());
                let moment = if before {
                    "before it observes"
                } else {
                    "as it observes"
                };
                outcomes.push((lock, moment, *reader, held.get(), waited.get(), ok, detail));
            }
        }
    }
    let failed: Vec<_> = outcomes
        .iter()
        .filter(|(_, moment, _, held, waited, ok, _)| {
            !*held || *waited != (*moment == "before it observes") || !*ok
        })
        .collect();
    assert!(
        failed.is_empty(),
        "(lock, holder's moment, reader, held, waited, read ok, read): {failed:#?}"
    );
}

/// A finding of the account service read's later stages stands beside a holder of a
/// predecessor's custody: the read judges whether the finding is only stale by reading its own
/// home again ([`Session::observation_afresh`]), which checks the kept observation of the
/// predecessors and takes no new one, so the holder, met by no wait, does not turn the finding
/// into busy. The finding here is a configuration the catalog stage cannot read.
/// NEGATIVE CONTROL (2026-11-08): with the probe observing the predecessors again first
/// (`Observation::observe_within(bootstrap.paths(), read_by, wait)`, as `Session::observe_within`
/// did), the read is `control_busy`.
#[test]
fn a_stale_account_read_keeps_its_finding_beside_a_predecessor_holder() {
    use crate::service_state::{accounts_until, control_busy, tests::after_observed};
    use std::{cell::RefCell, rc::Rc};

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    imported_successor(&mut lab);
    let unreadable = lab._home.path().join("unreadable-config.toml");
    fsx::write_atomic(&unreadable, b"[commons\n", 0o600).unwrap();
    let holder = Rc::new(RefCell::new(None));
    after_observed({
        let (holder, dir) = (holder.clone(), original.dir.clone());
        move || {
            let held = PrivateLock::take(&dir, "commons.lock", Duration::ZERO, "held").unwrap();
            *holder.borrow_mut() = Some(held);
        }
    });
    let read = accounts_until(
        &original,
        &unreadable,
        1000,
        &Instant::now,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    );
    assert!(holder.borrow_mut().take().is_some(), "never held");
    assert_ne!(
        read,
        control_busy(1000),
        "the holder turned the finding into busy"
    );
    assert!(read.refresh_failed, "{read:?}");
    assert_eq!(read.detail, "Shared catalog metadata could not be read");
}

/// A vault writer that holds the selected home's state lock as `observe_store_if_here` asks
/// whether this Mac holds Commons makes the read busy, which a reader observes again, not
/// "nothing declared here": the shared account read took that moment for a Mac that set up no
/// catalog. Once the writer lets go, the same read reads the table.
/// NEGATIVE CONTROL (2026-11-08): with `observe_store_if_here` asking `Bootstrap::cached` again,
/// the held read is `Ok(None)`.
#[test]
fn a_vault_writer_keeps_observe_store_if_here_busy_not_undeclared() {
    use crate::documents::application::observe_store_if_here;

    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    let (_, next, _) = imported_successor(&mut lab);
    let hang_detector = || Some(Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    let writer = client::test_hold_state_lock(&next).unwrap();
    let busy = observe_store_if_here(&original, records, hang_detector(), rows);
    drop(writer);
    assert!(busy.as_ref().is_err_and(Error::is_busy), "{busy:?}");
    assert_eq!(
        observe_store_if_here(&original, records, hang_detector(), rows).unwrap(),
        Some(1)
    );
}

/// A generation check whose deadline has passed says it ran out of time, not that the generation
/// is unavailable or changed: it learned nothing about the generation, and given time the same
/// reads pass. Every deadline gate goes through `within`; this asks the first gate of each entry
/// readers and writers call.
/// NEGATIVE CONTROL: with `within` refusing as `fail()` again, every assertion fails.
#[test]
fn a_spent_deadline_is_out_of_time_not_a_changed_generation() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    data(&lab);
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let spent = Instant::now();
    let out = out_of_time().to_string();
    let refused = |error: Error| assert_eq!(error.to_string(), out);
    refused(observed_paths_until(&original, spent).err().unwrap());
    refused(selected_until(&original, spent).err().unwrap());
    refused(guard_until(&next, false, spent).unwrap_err());
    refused(lineage_until(&original, spent).err().unwrap());
    refused(Observation::observe(&next, spent).err().unwrap());
    refused(Observation::capture(&next, spent).err().unwrap());
    let given_time = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let observation = observed_selection_within(&original, given_time, &|| given_time).unwrap();
    refused(observation.check(false, false, spent).unwrap_err());
    assert_eq!(observation.paths(), &next);
    assert_eq!(observed_paths_until(&original, given_time).unwrap(), next);
}

/// A fleet label's registry read ([`crate::alab::Registry::view_until`]) reads the registry's
/// generation as its first stage, under its own `read_by`: a spent read refuses there, as out of
/// time; given time, it reads the registry a successor home imported.
/// NEGATIVE CONTROL: with `Handle::at_until` taking the generation from `current`, which swallows
/// the spent bound, the read refuses only later, at its pass gate
/// (`application pass deadline expired`).
#[test]
fn a_successor_homes_registry_reads_under_its_own_bound() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    crate::alab::Registry::at(original.clone()).open().unwrap();
    let mut selection = reserved(&lab);
    move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let registry = crate::alab::Registry::at(original);
    let spent = registry.view_until(Instant::now()).err().unwrap();
    assert_eq!(spent.to_string(), out_of_time().to_string());
    let read_by = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    assert!(registry.view_until(read_by).unwrap().complete);
}

/// A vault writer that takes a predecessor's state lock while a generation check judges this
/// Mac's standing there (whether that vault denies this Mac, and whether the check's access is
/// still current) makes the check busy, which a read observes again
/// ([`crate::att::reopened`]), not [`fail`]'s changed generation: it judged nothing. The
/// predecessor's access is log-backed, as a Mac's is since F18, so both questions read its vault.
/// On m14 (2026-11-07, read from the source) every registry view runs dozens of these checks
/// beside the vault's own writers.
/// NEGATIVE CONTROL: with `check` asking `local_recipient_denied_in_roster` again, the
/// `BeforeDenial` case fails as a changed generation; with it asking `Access::is_current`, the
/// `BeforeCurrent` case does.
#[test]
fn a_vault_writer_during_a_check_is_busy_not_a_changed_generation() {
    use std::{cell::RefCell, rc::Rc};

    let mut lab = Lab::with_foundation(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    client::seed_vault_genesis(&original, &lab._home.path().join("genesis.v2")).unwrap();
    let records = client::load_records(&original).unwrap().encode();
    client::save_records(&lab.paths[1], &records).unwrap();
    data(&lab);
    let mut selection = reserved(&lab);
    move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let given_time = || Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let observation = observed_selection_within(&original, given_time(), &given_time).unwrap();
    assert!(
        observation.ancestors[0].access.is_log_backed(),
        "CONTROL: the predecessor's log backs its access"
    );
    let outcomes: Vec<_> = [seams::Point::BeforeDenial, seams::Point::BeforeCurrent]
        .into_iter()
        .map(|point| {
            let writer = Rc::new(RefCell::new(None));
            seams::at(point, {
                let (writer, original) = (writer.clone(), original.clone());
                move || {
                    let held = client::test_hold_state_lock(&original).unwrap();
                    *writer.borrow_mut() = Some(held);
                }
            });
            let checked = observation.check(false, false, given_time());
            let held = writer.borrow_mut().take().is_some();
            let busy = checked.as_ref().is_err_and(Error::is_busy);
            let after = observation.check(false, false, given_time()).is_ok();
            (point, held, busy, after, format!("{checked:?}"))
        })
        .collect();
    assert!(
        outcomes
            .iter()
            .all(|(_, held, busy, after, _)| *held && *busy && *after),
        "(point, writer held, check busy, next check passes, check): {outcomes:?}"
    );
}

/// A read of a successor home whose succession is still preserving old records refuses, as
/// observing it always did, rather than naming the candidate: so a peek never opens the
/// candidate's custody before it is complete or answers as if no table were there
/// (`registry: not imported`).
/// NEGATIVE CONTROL: with `observed_generation_until` following a selection that is not complete,
/// it names the candidate and the declaration peek answers `None`.
#[test]
fn a_pending_succession_refuses_a_read_rather_than_naming_its_candidate() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    data(&lab);
    let _selection = reserved(&lab);
    move_admitted_candidate(&mut lab);
    let given_time = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let pending = observed_generation_until(&original, given_time)
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        pending,
        "Commons writer succession is still preserving old records"
    );
    let declared = crate::documents::application::peek_declaration(
        &original,
        |bootstrap| Descriptor::application(bootstrap.trust(), "recovery-test", "records", 1),
        given_time,
    );
    assert_eq!(declared.err().unwrap().to_string(), pending);
}

/// A fleet label's registry generation ([`crate::att::current_within`]) is the read's to answer:
/// a vault writer that holds the selected home's state lock makes the lookup busy, which the read
/// observes again ([`crate::att::Handle::at_until`]), rather than reading the first generation in
/// place of the one this Mac's catalog names; once the writer is gone it reads the catalog's.
/// NEGATIVE CONTROL: with the catalog peek taking a busy vault for no Commons here
/// (`Bootstrap::cached`), the lookup answers the first generation while the writer holds.
#[test]
fn a_busy_vault_keeps_a_labels_registry_generation_busy_not_the_first() {
    let mut lab = Lab::new(&[Role::Writer, Role::Writer]);
    let original = lab.paths[0].clone();
    crate::alab::Registry::at(original.clone()).open().unwrap();
    let mut selection = reserved(&lab);
    let next = move_admitted_candidate(&mut lab);
    import(&mut selection).unwrap();
    let given_time = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let writer = client::test_hold_state_lock(&next).unwrap();
    let busy = crate::att::current_within(&original, &crate::att::REGISTRY, given_time)
        .err()
        .unwrap();
    drop(writer);
    assert!(busy.is_busy(), "{busy}");
    let current = crate::att::current_within(&original, &crate::att::REGISTRY, given_time).unwrap();
    assert_eq!(current.generation, crate::att::REGISTRY.generation);
}

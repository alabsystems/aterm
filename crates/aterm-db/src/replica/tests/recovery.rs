// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{catalog::Directory, table::tests::repair_commons};

#[test]
fn conflict_inventory_defers_until_all_primary_proofs_reach_recovery() {
    let lab = Lab::new(&[Role::Writer; BATCH + 3]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let scope = directory
        .open_application(&session, "conflict-preparation", "rows", 1)
        .unwrap();
    drop(session);

    let mut proofs = Vec::new();
    for actor in 1..=BATCH + 1 {
        let author = lab.bootstrap(actor);
        let session = Session::open(&author).unwrap();
        let own = Directory::new(&lab.paths[actor], author.trust());
        assert_eq!(
            own.open_application(&session, "conflict-preparation", "rows", 1)
                .unwrap(),
            scope
        );
        let mut store = own.open_store(&session, scope).unwrap();
        let receipt = store.commit_catalog(&session, b"proof source").unwrap();
        let first = store.wire(&session, &receipt.id).unwrap().unwrap();
        let mut second = first.to_vec();
        let end = second.len() - 64;
        second[end - 1] ^= 1;
        crate::table::tests::resign(&author, &mut second);
        proofs.push(
            crate::table::conflict::Conflict::from_pair(&session, scope, first, &second)
                .unwrap()
                .wire()
                .to_vec(),
        );
    }

    let session = Session::open(&bootstrap).unwrap();
    let mut primary = directory.open_store(&session, scope).unwrap();
    for proof in &proofs {
        primary.ingest_conflict(&session, proof).unwrap();
    }
    let expected = primary.proof_inventory(&session).unwrap();
    assert_eq!(expected.len(), BATCH + 1);
    directory.recovery().enroll(&session, scope).unwrap();
    let tables = directory
        .existing_subscriptions(&session, &crate::catalog::policy::Policy::default())
        .unwrap()
        .unwrap();
    drop((primary, session));

    let primary_before = crate::tests::files(&directory.path(scope).unwrap());
    let peer = lab
        .bootstrap(BATCH + 2)
        .request()
        .unwrap()
        .binding()
        .actor();
    let replica = Replica::commons(lab.paths[0].clone(), lab.paths[0].home.join("config")).unwrap();
    let mut round = replica.round(peer).unwrap();
    round.tables = tables;
    round.active_table.set(Some(scope.digest()));
    for (retained, incomplete) in [(BATCH, true), (BATCH + 1, false), (BATCH + 1, false)] {
        // The peer declines its own inventory, so this call ends after the
        // local readiness/proof response without another application batch.
        let mut stream = Cursor::new(vec![0]);
        let mut repair = Repair::default();
        let mut report = Report::default();
        assert!(
            !round
                .conflicts(
                    &mut stream,
                    peer,
                    Side::Responder,
                    scope.digest(),
                    &mut repair,
                    &mut report
                )
                .unwrap()
        );
        let bytes = stream.into_inner();
        let offered = receive_inventory::<81>(&mut Cursor::new(&bytes[1..]), MAX_PROOFS).unwrap();
        assert_eq!(offered, (!incomplete).then(|| expected.clone()));
        assert_eq!(
            repair.deferral.unwrap().retaining_primary_conflicts,
            incomplete
        );
        assert!(report.recovery);
        let session = Session::observe(&bootstrap).unwrap();
        let recovery = directory
            .recovery()
            .observe_store_if_exists(&session, scope)
            .unwrap()
            .unwrap();
        assert_eq!(recovery.proof_inventory(&session).unwrap().len(), retained);
        assert!(crate::tests::files(&directory.path(scope).unwrap()) == primary_before);
    }
}

#[test]
fn recovery_obeys_disabled_and_excluded_policy_without_replacing_either_catalog() {
    for disabled in [true, false] {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let source = lab.bootstrap(0);
        let source_directory = Directory::new(&lab.paths[0], source.trust());
        let session = Session::open(&source).unwrap();
        let scope = source_directory
            .open_application(&session, "recovery-policy", "rows", 1)
            .unwrap();
        let mut store = source_directory.open_store(&session, scope).unwrap();
        store
            .commit_catalog(&session, b"before-policy-change")
            .unwrap();
        drop((store, session));
        let receiver = lab.bootstrap(1);
        let directory = Directory::new(&lab.paths[1], receiver.trust());
        let session = Session::open(&receiver).unwrap();
        directory.snapshot(&session).unwrap();
        let original_catalog = directory.path(directory.catalog_scope()).unwrap();
        let saved_catalog = lab._home.path().join("saved-original-catalog");
        std::fs::rename(&original_catalog, &saved_catalog).unwrap();
        let claim_path = crate::table::store::claim_dir(&session)
            .unwrap()
            .join(crate::table::store::claim_name(directory.catalog_scope()));
        let original_claim = std::fs::read(&claim_path).unwrap();
        let original_data = std::fs::read(saved_catalog.join("table.astream")).unwrap();
        drop(session);
        let config = lab.paths[1].home.join("config.toml");
        let exclusion = format!(
            "[commons]\nexclude_tables=[\"{}\"]\n",
            aterm_codec::hex::encode_lower(&directory.catalog_scope().digest())
        );
        let denied = if disabled {
            "[commons]\nenabled=false\n"
        } else {
            &exclusion
        };
        aterm_secrets::fsx::write_atomic(&config, denied.as_bytes(), 0o600).unwrap();
        repair_commons(&lab, 8);
        let recovery_root = lab.paths[1].dir.join("commons/recovery");
        assert!(
            !recovery_root.exists(),
            "a denied repair cannot enroll recovery storage"
        );
        aterm_secrets::fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
        repair_commons(&lab, 8);
        let observe_count = || {
            let bootstrap = lab.bootstrap(1);
            let session = Session::observe(&bootstrap).unwrap();
            let store = directory.observe_table(&session, scope).unwrap();
            assert!(store.is_recovery());
            store.inventory(&session).unwrap().count()
        };
        assert_eq!(observe_count(), 1);
        let excluded = format!(
            "[commons]\nexclude_tables=[\"{}\"]\n",
            aterm_codec::hex::encode_lower(&scope.digest())
        );
        aterm_secrets::fsx::write_atomic(&config, excluded.as_bytes(), 0o600).unwrap();
        let session = Session::open(&source).unwrap();
        source_directory
            .open_store(&session, scope)
            .unwrap()
            .commit_catalog(&session, b"after-policy-change")
            .unwrap();
        drop(session);
        repair_commons(&lab, 8);
        assert_eq!(
            observe_count(),
            1,
            "an excluded table receives no new records"
        );
        aterm_secrets::fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
        repair_commons(&lab, 8);
        assert_eq!(observe_count(), 2);
        assert!(!original_catalog.exists());
        assert_eq!(std::fs::read(&claim_path).unwrap(), original_claim);
        assert_eq!(
            std::fs::read(saved_catalog.join("table.astream")).unwrap(),
            original_data
        );
        let replica = Replica::commons(lab.paths[1].clone(), config.clone()).unwrap();
        let peer = source.request().unwrap().binding().actor();
        let mut stale = replica.round(peer).unwrap();
        stale.commons = Some((directory.recovery(), config));
        let recovered_log = directory
            .recovery()
            .path(scope)
            .unwrap()
            .join("table.astream");
        let retained = std::fs::read(&recovered_log).unwrap();
        lab.rotate(&[Role::Writer]);
        assert!(
            stale
                .local(peer, |_| -> Result<()> {
                    panic!("a removed member cannot run a recovery batch")
                })
                .is_err()
        );
        assert_eq!(std::fs::read(recovered_log).unwrap(), retained);
    }
}

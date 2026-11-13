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

/// A primary store that keeps advancing through every read of retention's check defers the
/// round's conflict inventory, as the inventory's own read does, without saying retention has work
/// left.
///
/// NEGATIVE CONTROL: with `Round::conflicts` returning `retain_primary`'s refusal, this round is
/// lost for both peers.
#[test]
fn a_primary_that_keeps_advancing_through_retention_defers_the_conflict_inventory() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let scope = directory
        .open_application(&session, "retention-overtaken", "rows", 1)
        .unwrap();
    directory
        .open_store(&session, scope)
        .unwrap()
        .commit_catalog(&session, b"row")
        .unwrap();
    directory.recovery().enroll(&session, scope).unwrap();
    let tables = directory
        .existing_subscriptions(&session, &crate::catalog::policy::Policy::default())
        .unwrap()
        .unwrap();
    drop(session);
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let replica = Replica::commons(lab.paths[0].clone(), lab.paths[0].home.join("config")).unwrap();
    let mut round = replica.round(peer).unwrap();
    round.tables = tables;
    round.active_table.set(Some(scope.digest()));

    let overtake = {
        let (writer, directory) = (lab.bootstrap(0), directory.clone());
        move || {
            let session = Session::open(&writer).unwrap();
            directory
                .open_store(&session, scope)
                .unwrap()
                .commit_catalog(&session, b"overtaking-ingest")
                .unwrap();
        }
    };
    let mut stream = Cursor::new(vec![1, 0, 0, 0, 0]);
    let (mut repair, mut report) = (Repair::default(), Report::default());
    let went_on = crate::table::store::with_each_replay_observed(overtake, || {
        round.conflicts(
            &mut stream,
            peer,
            Side::Responder,
            scope.digest(),
            &mut repair,
            &mut report,
        )
    })
    .unwrap();
    assert!(!went_on);
    let why = repair.deferral.unwrap();
    assert_eq!(
        why.local_cause,
        Some(aterm_types::network_state::RepairCause::NotReady)
    );
    assert!(!why.retaining_primary_conflicts && !why.peer_unavailable);
    assert!(!report.recovery);
    assert_eq!(report.deferred, 1);
    assert_eq!(
        stream.into_inner()[5..],
        [0],
        "the peer hears the inventory is unavailable"
    );
}

/// E2: retention's check (`Round::retain_primary`) opens the primary only beside a recovery store,
/// whose existence it reads from the recovery claim alone (`Directory::enrollment`). Over recovery
/// {absent, pending, lacking the primary's evidence, holding it, damaged claim, damaged journal}
/// × primary {healthy, damaged, absent, busy}, each cell's verdict is the one the primary-first
/// order (PRIMARY_FIRST below) gives, but for one recorded change: a primary that refuses busy
/// (`FailureKind::Busy`, the hook's `Error::busy`; a snapshot that changed was already deferred by
/// retention's caller, `Round::table_read`) beside no recovery store is ready, where that order
/// refuses busy and so fails the round; the round's proof inventory read meets the busy primary and
/// defers, and the round goes on. Beside no recovery store the primary is not opened; beside one,
/// or a recovery claim that cannot be judged, it is opened as before, and once more where retention
/// copies what the recovery store lacks. An open is counted once it is past its claim
/// (`with_open_hook`): a damaged or absent primary's never is. Before each cell runs, the recovery
/// claim is checked to say what its stage names: Damaged's, while flipped, cannot be judged.
/// DamagedJournal keeps an enrolled claim while its journal cannot be read.
/// NEGATIVE CONTROLS: PRIMARY_FIRST, the check opening the primary before it asks for the recovery
/// store as it did, fails the healthy primary's open count beside no recovery store and the busy
/// primary's verdict and round there. READY_BESIDE_RECOVERY, the check ready beside any recovery
/// store without comparing, fails the cell whose recovery store lacks the evidence. ENROLLED_ONLY,
/// the check opening the primary only beside a recovery claim that is judged `Enrolled` (one that
/// cannot be judged read as no recovery store), fails the damaged recovery claim's healthy cell.
/// ENROLLED_READ_FIRST, an eager recovery observation beside an enrolled claim, fails the four
/// damaged-journal cells while leaving the preceding twenty cells unchanged.
#[test]
fn retain_primary_opens_the_primary_only_beside_a_recovery_store() {
    use crate::table::store::{Region, claim_dir_of, claim_name, with_open_hook};
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Recovery {
        Absent,
        Pending,
        Lacks,
        Holds,
        /// Holds, with its recovery claim's first byte flipped while each cell runs.
        Damaged,
        /// Holds under an intact enrolled claim, with its journal damaged while each cell runs.
        DamagedJournal,
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Primary {
        Healthy,
        Damaged,
        Absent,
        Busy,
    }
    #[derive(Debug, PartialEq, Eq)]
    struct Outcome {
        recovery: Recovery,
        primary: Primary,
        verdict: &'static str,
        primary_opens: usize,
        recovery_records: Option<usize>,
        round: Option<&'static str>,
    }
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let primaries = [
        (Primary::Healthy, "retention-healthy"),
        (Primary::Damaged, "retention-damaged"),
        (Primary::Absent, "retention-absent"),
        (Primary::Busy, "retention-busy"),
    ];
    let session = Session::open(&bootstrap).unwrap();
    let scopes: Vec<_> = primaries
        .iter()
        .map(|(_, application)| {
            let scope = directory
                .open_application(&session, application, "rows", 1)
                .unwrap();
            directory
                .open_store(&session, scope)
                .unwrap()
                .commit_catalog(&session, b"the primary's evidence")
                .unwrap();
            scope
        })
        .collect();
    let tables = directory
        .existing_subscriptions(&session, &crate::catalog::policy::Policy::default())
        .unwrap()
        .unwrap();
    let recovery_claims = Region::Recovery.claims(&session).unwrap();
    drop(session);
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let replica = Replica::commons(lab.paths[0].clone(), lab.paths[0].home.join("config")).unwrap();
    let mut round = replica.round(peer).unwrap();
    round.tables = tables;
    let claims = claim_dir_of(&bootstrap).unwrap();
    let aside = lab._home.path().join("aside");
    std::fs::create_dir(&aside).unwrap();
    // What the recovery claim says of `scope`'s recovery storage now.
    let enrollment = |scope: Scope| {
        let session = Session::observe(&bootstrap).unwrap();
        directory
            .recovery()
            .enrollment(&session, scope)
            .map_err(drop)
    };

    // Each stage holds the primary as it is: retention copies only what it can read.
    let advance = |stage: Recovery, scope: Scope| {
        let session = Session::open(&bootstrap).unwrap();
        let recovery = directory.recovery();
        match stage {
            // Damaged keeps the store Holds left; `as_recovery` damages its claim.
            Recovery::Absent | Recovery::Damaged | Recovery::DamagedJournal => {}
            Recovery::Pending => {
                // A first enrollment cut short after its pending claim: its data path was taken.
                let path = recovery.path(scope).unwrap();
                let tables = path.parent().unwrap();
                aterm_journal::create_private_dir_durable(tables.parent().unwrap()).unwrap();
                aterm_journal::create_private_dir_durable(tables).unwrap();
                std::fs::write(&path, b"occupied data path").unwrap();
                assert!(Store::join_recovery(&session, &path, scope).is_err());
                std::fs::remove_file(&path).unwrap();
            }
            Recovery::Lacks => drop(recovery.enroll(&session, scope).unwrap()),
            Recovery::Holds => {
                let source = directory
                    .observe_store_if_exists(&session, scope)
                    .unwrap()
                    .unwrap();
                let mut target = recovery.open_store(&session, scope).unwrap();
                assert!(
                    target
                        .retain_replica_batch(&session, &source, BATCH)
                        .unwrap()
                );
            }
        }
    };
    // `run` with the primary as `primary` names it, and the primary as it was after.
    let as_primary = |primary: Primary, scope: Scope, run: &mut dyn FnMut()| {
        let (path, name) = (directory.path(scope).unwrap(), claim_name(scope));
        match primary {
            Primary::Healthy | Primary::Busy => run(),
            Primary::Damaged => {
                let claim = std::fs::read(claims.join(&name)).unwrap();
                let mut damaged = claim.clone();
                damaged[0] ^= 1;
                aterm_uds::private_file::write_private(&claims, &name, &damaged).unwrap();
                run();
                aterm_uds::private_file::write_private(&claims, &name, &claim).unwrap();
            }
            Primary::Absent => {
                std::fs::rename(&path, aside.join("table")).unwrap();
                std::fs::rename(claims.join(&name), aside.join("claim")).unwrap();
                run();
                std::fs::rename(aside.join("table"), &path).unwrap();
                std::fs::rename(aside.join("claim"), claims.join(&name)).unwrap();
            }
        }
    };
    // `run` with the recovery storage as `stage` names it, and that storage as it was after, so the
    // cell's recovery store can still be read.
    let as_recovery = |stage: Recovery, scope: Scope, run: &mut dyn FnMut()| {
        if stage == Recovery::DamagedJournal {
            let journal = directory
                .recovery()
                .path(scope)
                .unwrap()
                .join("table.astream");
            let intact = std::fs::read(&journal).unwrap();
            std::fs::write(&journal, b"damaged").unwrap();
            assert_eq!(
                enrollment(scope),
                Ok(Enrollment::Enrolled),
                "enrollment judges the intact claim without replaying the damaged journal"
            );
            run();
            std::fs::write(&journal, intact).unwrap();
            return;
        }
        if stage != Recovery::Damaged {
            return run();
        }
        let name = claim_name(scope);
        let claim = std::fs::read(recovery_claims.join(&name)).unwrap();
        let mut damaged = claim.clone();
        damaged[0] ^= 1;
        aterm_uds::private_file::write_private(&recovery_claims, &name, &damaged).unwrap();
        assert_eq!(
            enrollment(scope),
            Err(()),
            "a damaged recovery claim cannot be judged"
        );
        run();
        aterm_uds::private_file::write_private(&recovery_claims, &name, &claim).unwrap();
    };

    let mut cells = Vec::new();
    for stage in [
        Recovery::Absent,
        Recovery::Pending,
        Recovery::Lacks,
        Recovery::Holds,
        Recovery::Damaged,
        Recovery::DamagedJournal,
    ] {
        for (&(primary, _), &scope) in primaries.iter().zip(&scopes) {
            advance(stage, scope);
            assert_eq!(
                enrollment(scope),
                Ok(match stage {
                    Recovery::Absent => Enrollment::Absent,
                    Recovery::Pending => Enrollment::Pending,
                    Recovery::Lacks
                    | Recovery::Holds
                    | Recovery::Damaged
                    | Recovery::DamagedJournal => Enrollment::Enrolled,
                }),
                "{stage:?} {primary:?}: the recovery claim says what the stage names"
            );
            let id = scope.digest();
            round.active_table.set(Some(id));
            let path = directory.path(scope).unwrap();
            let busy = primary == Primary::Busy;
            let refusal = move || {
                if busy { Err(Error::busy()) } else { Ok(()) }
            };
            let opens = std::rc::Rc::new(std::cell::Cell::new(0));
            let (mut verdict, mut went) = ("", None);
            as_recovery(stage, scope, &mut || {
                as_primary(primary, scope, &mut || {
                    let counted = opens.clone();
                    let hook = move || {
                        counted.set(counted.get() + 1);
                        refusal()
                    };
                    verdict = match with_open_hook(&path, hook, || round.retain_primary(peer, id)) {
                        Ok(true) => "ready",
                        Ok(false) => "retaining",
                        Err(error) if error.is_busy() => "busy",
                        Err(_) => "refused",
                    };
                    if busy {
                        // The peer declines its inventory, so the round ends after this side's.
                        let mut stream = Cursor::new(vec![0]);
                        let (mut repair, mut report) = (Repair::default(), Report::default());
                        let conflicts = with_open_hook(&path, refusal, || {
                            round.conflicts(
                                &mut stream,
                                peer,
                                Side::Responder,
                                id,
                                &mut repair,
                                &mut report,
                            )
                        });
                        went = Some(match conflicts {
                            Ok(false)
                                if report.deferred == 1
                                    && repair.deferral.as_ref().is_some_and(|deferral| {
                                        !deferral.retaining_primary_conflicts
                                        && deferral.local_cause
                                            == Some(
                                                aterm_types::network_state::RepairCause::NotReady,
                                            )
                                    }) =>
                            {
                                "deferred"
                            }
                            Ok(_) => "went on",
                            Err(_) => "failed",
                        });
                    }
                });
            });
            let session = Session::observe(&bootstrap).unwrap();
            let recovery_records = directory
                .recovery()
                .observe_store_if_exists(&session, scope)
                .unwrap()
                .map(|store| store.inventory(&session).unwrap().count());
            drop(session);
            cells.push(Outcome {
                recovery: stage,
                primary,
                verdict,
                primary_opens: opens.get(),
                recovery_records,
                round: went,
            });
        }
    }
    let cell = |recovery, primary, verdict, primary_opens, recovery_records, round| Outcome {
        recovery,
        primary,
        verdict,
        primary_opens,
        recovery_records,
        round,
    };
    assert_eq!(
        cells,
        [
            // No recovery store: nothing to retain, and the primary is not opened.
            cell(Recovery::Absent, Primary::Healthy, "ready", 0, None, None),
            cell(Recovery::Absent, Primary::Damaged, "ready", 0, None, None),
            cell(Recovery::Absent, Primary::Absent, "ready", 0, None, None),
            cell(
                Recovery::Absent,
                Primary::Busy,
                "ready",
                0,
                None,
                Some("deferred")
            ),
            cell(Recovery::Pending, Primary::Healthy, "ready", 0, None, None),
            cell(Recovery::Pending, Primary::Damaged, "ready", 0, None, None),
            cell(Recovery::Pending, Primary::Absent, "ready", 0, None, None),
            cell(
                Recovery::Pending,
                Primary::Busy,
                "ready",
                0,
                None,
                Some("deferred")
            ),
            // A recovery store: the primary first, as the check always read it. What it lacks of
            // a readable primary it retains, opening the primary again to copy it.
            cell(Recovery::Lacks, Primary::Healthy, "ready", 2, Some(1), None),
            cell(Recovery::Lacks, Primary::Damaged, "ready", 0, Some(0), None),
            cell(Recovery::Lacks, Primary::Absent, "ready", 0, Some(0), None),
            cell(
                Recovery::Lacks,
                Primary::Busy,
                "busy",
                1,
                Some(0),
                Some("failed")
            ),
            cell(Recovery::Holds, Primary::Healthy, "ready", 1, Some(1), None),
            cell(Recovery::Holds, Primary::Damaged, "ready", 0, Some(1), None),
            cell(Recovery::Holds, Primary::Absent, "ready", 0, Some(1), None),
            cell(
                Recovery::Holds,
                Primary::Busy,
                "busy",
                1,
                Some(1),
                Some("failed")
            ),
            // A recovery claim that cannot be judged: the primary first, as beside a recovery
            // store. A readable primary is opened, and the recovery observation then refuses the
            // claim; a damaged or absent primary leaves nothing to retain, and a busy one refuses
            // busy.
            cell(
                Recovery::Damaged,
                Primary::Healthy,
                "refused",
                1,
                Some(1),
                None
            ),
            cell(
                Recovery::Damaged,
                Primary::Damaged,
                "ready",
                0,
                Some(1),
                None
            ),
            cell(
                Recovery::Damaged,
                Primary::Absent,
                "ready",
                0,
                Some(1),
                None
            ),
            cell(
                Recovery::Damaged,
                Primary::Busy,
                "busy",
                1,
                Some(1),
                Some("failed")
            ),
            // An enrolled claim does not promise a readable journal. Its damage must not
            // displace an unavailable primary's result, just as an unjudged claim must not.
            cell(
                Recovery::DamagedJournal,
                Primary::Healthy,
                "refused",
                1,
                Some(1),
                None
            ),
            cell(
                Recovery::DamagedJournal,
                Primary::Damaged,
                "ready",
                0,
                Some(1),
                None
            ),
            cell(
                Recovery::DamagedJournal,
                Primary::Absent,
                "ready",
                0,
                Some(1),
                None
            ),
            cell(
                Recovery::DamagedJournal,
                Primary::Busy,
                "busy",
                1,
                Some(1),
                Some("failed")
            ),
        ]
    );
}

/// E3: a repair round's table step (`Round::table`) enrolls a primary application table whose
/// claim says it has no enrollment yet, which its storage admission reads from that claim alone
/// (`Directory::enrollment`): after the table step, an empty discovery reservation (`Absent`) and a
/// first enrollment cut short after its pending claim (`Pending`) are each enrolled and observable.
/// Each row's claim is checked to say what the row names before the round runs.
/// NEGATIVE CONTROLS: PENDING_UNJOINED, the storage admission joining an `Absent` table only,
/// fails the pending row; ABSENT_UNJOINED, the admission joining a `Pending` table only, fails the
/// reservation row.
#[test]
fn a_round_enrolls_a_primary_table_its_claim_says_is_not_enrolled_yet() {
    use crate::table::store::{claim_dir_of, claim_name};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let rows = [
        (Enrollment::Absent, "unenrolled-reservation"),
        (Enrollment::Pending, "unenrolled-pending"),
    ]
    .map(|(row, application)| {
        let scope = directory
            .open_application(&session, application, "rows", 1)
            .unwrap();
        (row, scope)
    });
    let tables = directory
        .existing_subscriptions(&session, &crate::catalog::policy::Policy::default())
        .unwrap()
        .unwrap();
    // Declaring a table enrolls its primary (`Directory::publish`): undo that, then leave the
    // storage as the row names it.
    let claims = claim_dir_of(&bootstrap).unwrap();
    for (row, scope) in rows {
        let path = directory.path(scope).unwrap();
        std::fs::remove_dir_all(&path).unwrap();
        std::fs::remove_file(claims.join(claim_name(scope))).unwrap();
        match row {
            // The slot `subscriptions()` reserves before `Store::join` creates its claim.
            Enrollment::Absent => aterm_journal::create_private_dir_durable(&path).unwrap(),
            // A first enrollment cut short after its pending claim: its data path was taken.
            Enrollment::Pending => {
                std::fs::write(&path, b"occupied data path").unwrap();
                assert!(Store::join(&session, &path, scope).is_err());
                std::fs::remove_file(&path).unwrap();
            }
            Enrollment::Enrolled => unreachable!("every row is unenrolled"),
        }
        assert_eq!(
            directory.enrollment(&session, scope).map_err(drop),
            Ok(row),
            "the claim says what the row names"
        );
    }
    drop(session);
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let replica = Replica::commons(lab.paths[0].clone(), lab.paths[0].home.join("config")).unwrap();
    let mut round = replica.round(peer).unwrap();
    round.tables = tables;

    let enrolled: Vec<_> = rows
        .iter()
        .map(|&(row, scope)| {
            // The peer declines its conflict inventory, so the table step ends after this side's.
            let mut stream = Cursor::new(vec![0]);
            let (mut repair, mut report) = (Repair::default(), Report::default());
            let went = round
                .table(
                    &mut stream,
                    peer,
                    Side::Responder,
                    scope.digest(),
                    &mut repair,
                    &mut report,
                )
                .map_err(|error| error.to_string());
            let session = Session::observe(&bootstrap).unwrap();
            (
                row,
                went,
                directory.enrollment(&session, scope).map_err(drop),
                directory
                    .observe_store_if_exists(&session, scope)
                    .map(|store| store.is_some())
                    .map_err(drop),
            )
        })
        .collect();
    assert_eq!(
        enrolled,
        [
            (
                Enrollment::Absent,
                Ok(()),
                Ok(Enrollment::Enrolled),
                Ok(true)
            ),
            (
                Enrollment::Pending,
                Ok(()),
                Ok(Enrollment::Enrolled),
                Ok(true)
            ),
        ],
        "each row's table step enrolls its primary"
    );
}

/// Repair rounds that let recovery enroll and take a new record: a pair's two tables, one a round.
/// The positive counts below prove this on the same states, so the denied runs of the same length
/// are not vacuous, and one round fails them.
const RECOVERY_ROUNDS: usize = 2;

/// One Lab refuses recovery twice, once with Commons disabled and once with the catalog excluded,
/// and then runs recovery, its table exclusion and a removed member's stale round.
/// GAP: the exclusion is denied after the disabled run, not on a pristine Lab, so a defect only on
/// a pristine Lab with the catalog excluded is not seen, and the tail runs after both denials.
#[test]
fn recovery_obeys_disabled_and_excluded_policy_without_replacing_either_catalog() {
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
    let recovery_root = lab.paths[1].dir.join("commons/recovery");
    for denied in ["[commons]\nenabled=false\n", &exclusion] {
        aterm_secrets::fsx::write_atomic(&config, denied.as_bytes(), 0o600).unwrap();
        repair_commons(&lab, RECOVERY_ROUNDS);
        assert!(
            !recovery_root.exists(),
            "a denied repair cannot enroll recovery storage"
        );
    }
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
    repair_commons(&lab, RECOVERY_ROUNDS);
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
    repair_commons(&lab, RECOVERY_ROUNDS);
    assert_eq!(
        observe_count(),
        1,
        "an excluded table receives no new records"
    );
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
    repair_commons(&lab, RECOVERY_ROUNDS);
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

/// A Commons table's fence stands only for the store `Round::store` opens, which is an existing
/// recovery store first (`Round::opens_fenced`). While the primary stands alone, the matching
/// inventory's final read opens no store beyond its admission's (whose table policy reads the
/// catalog). Once a recovery store is enrolled after the operation inventory read the primary,
/// the final read reads the recovery store `Round::store` now opens, whose inventory differs,
/// and withdraws the observation. A recovery store's own fence then stands with no probe, until
/// the recovery store receives a row.
/// NEGATIVE CONTROL: `opens_fenced` without its recovery probe keeps the first observation, and one
/// that declines every recovery store's fence opens a store for the last but one.
#[test]
fn a_commons_tables_fence_stands_only_for_the_store_round_store_opens() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    let scope = directory
        .open_application(&session, "fenced-recovery", "rows", 1)
        .unwrap();
    let mut primary = directory.open_store(&session, scope).unwrap();
    let row = primary.commit_catalog(&session, b"row").unwrap();
    let wire = primary.wire(&session, &row.id).unwrap().unwrap().to_vec();
    drop(primary);
    let tables = directory
        .existing_subscriptions(&session, &crate::catalog::policy::Policy::default())
        .unwrap()
        .unwrap();
    drop(session);
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let replica = Replica::commons(lab.paths[0].clone(), lab.paths[0].home.join("config")).unwrap();
    let mut round = replica.round(peer).unwrap();
    round.tables = tables;
    let id = scope.digest();
    round.active_table.set(Some(id));
    let inventory = round
        .inventory(peer, id, &mut None, &mut None)
        .unwrap()
        .unwrap();
    let before = inventory.observation;
    assert!(
        inventory
            .fence
            .as_ref()
            .is_some_and(|fence| !fence.is_recovery()),
        "the operation inventory read the primary store"
    );
    round
        .inventory_read
        .replace(Some((before, inventory.fence)));
    let report = Report {
        table: Some(id),
        local_known: Some(before.inventory),
        peer_known: Some(before.inventory),
        local_offered: Some(before.inventory),
        ..Report::default()
    };
    let opens = || crate::table::store::test_opens_and_checks().0;
    let opened = opens();
    round.local(peer, |_| Ok(())).unwrap();
    let admission = opens() - opened;
    let opened = opens();
    assert!(round.matching_inventory(peer, id, &report).is_some());
    assert_eq!(
        opens() - opened,
        admission,
        "the primary's fence stands while it stands alone"
    );

    let session = Session::open(&bootstrap).unwrap();
    directory.recovery().enroll(&session, scope).unwrap();
    drop(session);
    let opened = opens();
    assert!(
        round.matching_inventory(peer, id, &report).is_none(),
        "the recovery store Round::store now opens holds another inventory"
    );
    assert!(
        opens() - opened > admission,
        "the final read opens the store Round::store chooses"
    );

    let inventory = round
        .inventory(peer, id, &mut None, &mut None)
        .unwrap()
        .unwrap();
    let before = inventory.observation;
    assert!(
        inventory
            .fence
            .as_ref()
            .is_some_and(|fence| fence.is_recovery()),
        "the operation inventory read the recovery store"
    );
    round
        .inventory_read
        .replace(Some((before, inventory.fence)));
    let report = Report {
        table: Some(id),
        local_known: Some(before.inventory),
        peer_known: Some(before.inventory),
        local_offered: Some(before.inventory),
        ..Report::default()
    };
    let opened = opens();
    assert!(round.matching_inventory(peer, id, &report).is_some());
    assert_eq!(
        opens() - opened,
        admission,
        "a recovery store's fence stands with no probe"
    );

    let session = Session::open(&bootstrap).unwrap();
    directory
        .recovery()
        .open_store(&session, scope)
        .unwrap()
        .ingest(&session, &wire)
        .unwrap();
    drop(session);
    let opened = opens();
    assert!(
        round.matching_inventory(peer, id, &report).is_none(),
        "the row the recovery store received moved its claim and its inventory"
    );
    assert!(
        opens() - opened > admission,
        "a moved recovery store is read again"
    );
}

/// The ring clock [`selection`]'s fixtures write their ring states at and select by.
const SELECTION_NOW: u64 = 20_000 * crate::catalog::ring::CLAIMS.period_s + 60;

/// How a [`selection`] fixture holds its primary catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Primary {
    /// A declared table with storage, and the catalog that declares it.
    Healthy,
    /// No catalog and no table storage yet.
    Absent,
    /// A declared table's storage, and no catalog: neither its storage nor its claim.
    AbsentBesideStorage,
    /// A declared table with storage, and the catalog's claim with its storage gone.
    Damaged,
    /// [`Primary::Healthy`], the declared table excluded by local policy.
    Excluded,
    /// [`Primary::Damaged`], after a first selection enrolled the recovery catalog.
    RecoveryBesideDamaged,
    /// [`Primary::Healthy`], every open of its catalog in the selection refusing busy
    /// (`Error::busy`, through `with_open_hook`).
    Busy,
    /// [`Primary::RecoveryBesideDamaged`], whose recovery catalog's first open in the selection
    /// puts the primary catalog back and refuses with a changed snapshot
    /// (`Error::snapshot_changed`), so the selection's admission is read again (`Round::local`).
    HealedOnRetry,
}

/// One Mac's round selection (`Round::select_tables`) over `primary`, its ring states written as
/// a settled Mac's are: whether it selected, whether its report and its storage region say
/// recovery, its tables, by name, and whether a recovery catalog is enrolled after it.
fn selection(
    primary: Primary,
) -> (
    std::result::Result<(), String>,
    bool,
    bool,
    BTreeSet<&'static str>,
    bool,
) {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let config = lab.paths[0].home.join("config.toml");
    let policy = crate::catalog::policy::Policy::default();
    let session = Session::open(&bootstrap).unwrap();
    directory
        .ring_subscriptions(&session, &policy, SELECTION_NOW)
        .unwrap();
    let catalog = directory.catalog_scope();
    let app = (primary != Primary::Absent).then(|| {
        directory
            .open_application(&session, "selection", "rows", 1)
            .unwrap()
    });
    let catalog_path = directory.path(catalog).unwrap();
    let moved = lab._home.path().join("moved-catalog");
    match primary {
        Primary::Healthy | Primary::Absent | Primary::Busy => {}
        Primary::AbsentBesideStorage => {
            std::fs::remove_dir_all(&catalog_path).unwrap();
            std::fs::remove_file(
                crate::table::store::claim_dir(&session)
                    .unwrap()
                    .join(crate::table::store::claim_name(catalog)),
            )
            .unwrap();
        }
        Primary::Damaged | Primary::RecoveryBesideDamaged | Primary::HealedOnRetry => {
            std::fs::rename(&catalog_path, &moved).unwrap();
        }
        Primary::Excluded => {
            let excluded = format!(
                "[commons]\nexclude_tables=[\"{}\"]\n",
                aterm_codec::hex::encode_lower(&app.unwrap().digest())
            );
            aterm_secrets::fsx::write_atomic(&config, excluded.as_bytes(), 0o600).unwrap();
        }
    }
    drop(session);
    let mut replica = Replica::commons(lab.paths[0].clone(), config).unwrap();
    replica.ring_clock = Some(SELECTION_NOW);
    let recovery_path = directory.recovery().path(catalog).unwrap();
    let recovery_enrolled = || recovery_path.join("table.astream").exists();
    if matches!(
        primary,
        Primary::RecoveryBesideDamaged | Primary::HealedOnRetry
    ) {
        let mut round = replica.round(None).unwrap();
        if let Err(error) = round.select_tables(None, &mut Report::default()) {
            let first = format!("the first selection: {error}");
            return (Err(first), false, false, BTreeSet::new(), false);
        }
        assert!(
            recovery_enrolled(),
            "CONTROL: the recovery catalog is enrolled"
        );
    }
    let mut round = replica.round(None).unwrap();
    let mut report = Report::default();
    let opens = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut select = || {
        round
            .select_tables(None, &mut report)
            .map_err(|error| error.to_string())
    };
    let selected = match primary {
        Primary::Busy => {
            let opens = opens.clone();
            crate::table::store::with_open_hook(
                &catalog_path,
                move || {
                    opens.set(opens.get() + 1);
                    Err(Error::busy())
                },
                select,
            )
        }
        Primary::HealedOnRetry => {
            let (opens, moved, catalog_path) = (opens.clone(), moved.clone(), catalog_path.clone());
            crate::table::store::with_open_hook(
                &recovery_path,
                move || {
                    opens.set(opens.get() + 1);
                    if opens.get() > 1 {
                        return Ok(());
                    }
                    std::fs::rename(&moved, &catalog_path).unwrap();
                    Err(Error::snapshot_changed())
                },
                select,
            )
        }
        _ => select(),
    };
    if matches!(primary, Primary::Busy | Primary::HealedOnRetry) {
        assert!(opens.get() >= 1, "CONTROL: the hooked catalog was opened");
    }
    let name = |id: &Digest| {
        if *id == catalog.digest() {
            "catalog"
        } else if app.is_some_and(|app| app.digest() == *id) {
            "app"
        } else {
            "another"
        }
    };
    (
        selected,
        report.recovery,
        round
            .commons
            .as_ref()
            .is_some_and(|(directory, _)| directory.is_recovery()),
        round.tables.keys().map(name).collect(),
        recovery_enrolled(),
    )
}

/// One selection places a round in its storage region and names its tables: a healthy primary
/// selects its catalog and declared table there; with no catalog and no storage the write path
/// enrolls the catalog; a catalog absent beside table storage, or damaged, sends the round to
/// recovery, which enrolls its own catalog, and a recovery catalog already there is read; a table
/// local policy excludes is not selected. A primary catalog that refuses busy fails the round
/// where it stands: no recovery in the report or the region, no table, no recovery catalog
/// enrolled beside the healthy primary. A retry of the selection's admission decides the region
/// afresh: where the recovery catalog's read, the primary damaged, refuses with a changed snapshot
/// and the primary heals meanwhile, the second read places the round in the primary with the
/// primary's tables, nothing of the first read's verdict kept. Each fixture's ring states are
/// written, so a selection that finds its subscriptions settled takes them without the writer.
/// NEGATIVE CONTROLS: the selection's projection under `Policy::default()` in place of the
/// round's policy selects the excluded table: `` {"app", "catalog"} `` for `Excluded`; a
/// selection that returns the primary catalog's error in place of turning to recovery fails
/// `AbsentBesideStorage` ("catalog is absent beside retained table storage"), `Damaged`,
/// `RecoveryBesideDamaged` and `HealedOnRetry` (their first selection), each "No such file or
/// directory"; without `Round::select_tables`' busy arm, `Busy` goes to recovery and enrolls its
/// catalog: `` (Ok(()), true, true, {"catalog"}, true) ``; without its reset of the region flag at
/// each attempt, `HealedOnRetry` is placed in recovery with the primary's tables:
/// `` (Ok(()), true, true, {"app", "catalog"}, true) ``.
#[test]
fn one_selection_places_and_subscribes_the_round() {
    let rows = [
        Primary::Healthy,
        Primary::Absent,
        Primary::AbsentBesideStorage,
        Primary::Damaged,
        Primary::Excluded,
        Primary::RecoveryBesideDamaged,
        Primary::Busy,
        Primary::HealedOnRetry,
    ];
    let selected = aterm_spec::fs_lane::each(&rows, |&primary| selection(primary));
    let both = BTreeSet::from(["catalog", "app"]);
    let catalog = BTreeSet::from(["catalog"]);
    assert_eq!(
        rows.iter().copied().zip(selected).collect::<Vec<_>>(),
        [
            (
                Primary::Healthy,
                (Ok(()), false, false, both.clone(), false)
            ),
            (
                Primary::Absent,
                (Ok(()), false, false, catalog.clone(), false)
            ),
            (
                Primary::AbsentBesideStorage,
                (Ok(()), true, true, catalog.clone(), true)
            ),
            (
                Primary::Damaged,
                (Ok(()), true, true, catalog.clone(), true)
            ),
            (
                Primary::Excluded,
                (Ok(()), false, false, catalog.clone(), false)
            ),
            (
                Primary::RecoveryBesideDamaged,
                (Ok(()), true, true, catalog.clone(), true)
            ),
            (
                Primary::Busy,
                (
                    Err(Error::busy().to_string()),
                    false,
                    false,
                    BTreeSet::new(),
                    false
                )
            ),
            (
                Primary::HealedOnRetry,
                (Ok(()), false, false, both.clone(), true)
            ),
        ]
    );
}

/// A selection reads the primary catalog once (`Round::select_tables`): over a healthy primary
/// whose subscriptions and ring states are settled, the selection a mesh call makes
/// (`Replica::mesh_tables`) opens one store, the catalog, where its probe and its subscription
/// read opened it twice.
/// NEGATIVE CONTROL: `Directory::existing_subscriptions`, which opens the catalog again, in place
/// of `Directory::selected_subscriptions` opens 2.
#[test]
fn a_selection_reads_the_primary_catalog_once() {
    let lab = Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let session = Session::open(&bootstrap).unwrap();
    directory
        .ring_subscriptions(
            &session,
            &crate::catalog::policy::Policy::default(),
            SELECTION_NOW,
        )
        .unwrap();
    let scope = directory
        .open_application(&session, "selection", "rows", 1)
        .unwrap();
    drop(session);
    let mut replica =
        Replica::commons(lab.paths[0].clone(), lab.paths[0].home.join("config.toml")).unwrap();
    replica.ring_clock = Some(SELECTION_NOW);
    let (opens, _) = crate::table::store::test_opens_and_checks();
    let tables = replica.mesh_tables().unwrap();
    let (now, _) = crate::table::store::test_opens_and_checks();
    assert_eq!(
        tables,
        BTreeSet::from([directory.catalog_scope().digest(), scope.digest()]),
        "CONTROL: the selection names the catalog and its table"
    );
    assert_eq!(now - opens, 1, "the selection's store opens");
}

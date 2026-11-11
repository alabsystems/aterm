// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

type Hook = (u8, Box<dyn FnOnce()>);
thread_local! {
    static SESSION_BOUNDARY: RefCell<Option<Hook>> = RefCell::new(None);
}

pub(in crate::replica) fn session_boundary(at: u8) {
    let callback = SESSION_BOUNDARY.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(point, _)| *point == at) {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, callback)) = callback {
        callback();
    }
}

struct Change;
impl Change {
    fn install(at: u8, callback: impl FnOnce() + 'static) -> Self {
        SESSION_BOUNDARY.with(|slot| {
            assert!(
                slot.borrow_mut()
                    .replace((at, Box::new(callback)))
                    .is_none()
            );
        });
        Self
    }
}
impl Drop for Change {
    fn drop(&mut self) {
        SESSION_BOUNDARY.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[test]
fn a_policy_change_fires_once_at_only_its_selected_session_boundary() {
    for point in [0, 1] {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let invoked = calls.clone();
        let _change = Change::install(point, move || invoked.set(invoked.get() + 1));
        session_boundary(1 - point);
        assert_eq!(
            calls.get(),
            0,
            "the other boundary must not consume the change"
        );
        session_boundary(point);
        assert_eq!(
            calls.get(),
            1,
            "the selected boundary must apply the change"
        );
        session_boundary(point);
        assert_eq!(calls.get(), 1, "a boundary change must not run twice");
    }
}

#[test]
fn vault_read_spans_session_admission_and_ends_with_the_local_batch() {
    use std::{cell::Cell, rc::Rc};

    let p = Pair::new(false, 1);
    let replica =
        Replica::new(p.lab.paths[0].clone(), vec![(p.scope, p.paths[0].clone())]).unwrap();
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let writer = Rc::new(
        aterm_uds::private_file::open_private(&p.lab.paths[0].dir, ".lock")
            .unwrap()
            .0,
    );
    let acquired = Rc::new(Cell::new(false));
    let calls = Rc::new(Cell::new(0));
    let before = std::fs::read(p.paths[0].join("table.astream")).unwrap();
    let result = {
        let contender = writer.clone();
        let held = acquired.clone();
        let called = calls.clone();
        let _change = Change::install(0, move || {
            called.set(called.get() + 1);
            #[expect(
                clippy::disallowed_methods,
                reason = "the contender tests the gap before repair-session admission"
            )]
            let attempt = contender.try_lock();
            match attempt {
                Ok(()) => held.set(true),
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(error) => panic!("vault contender failed: {error}"),
            }
        });
        round.local(peer, |session| {
            let store = Store::observe(session, &p.paths[0], p.scope)?;
            Ok(store.inventory(session)?.count())
        })
    };
    if acquired.get() {
        writer.unlock().unwrap();
    }
    assert_eq!(calls.get(), 1);
    assert_eq!(
        result.expect("vault writers cannot enter before session admission"),
        1
    );
    assert!(!acquired.get());
    #[expect(
        clippy::disallowed_methods,
        reason = "the source read must end before this local batch returns to network I/O"
    )]
    writer
        .try_lock()
        .expect("vault writer can proceed after the batch");
    writer.unlock().unwrap();
    assert_eq!(
        std::fs::read(p.paths[0].join("table.astream")).unwrap(),
        before
    );
}

/// A local batch's admission (its session's open, and the peer and the observation it checks at
/// once after it: `Replica::observe`) hands one look at the vault
/// ([`aterm_secrets::client::VaultLook`]) to each of its custody, access, currency and roster
/// checks: none is made without one ([`crate::table::OWN_CHECKS`]), for a read or for a write. A
/// read's look is its batch's source read of the vault, so its admission opens no look of its own
/// (`aterm_secrets::client::test_looks_observed`), and its session keeps it: the batch's own
/// checks (a peer's currency and roster, its observation and its legacy one, an endpoint's
/// context, a store's context and inventory) are judged through it, none on its own call. A
/// write's look ends before its batch runs, whose own `Session::peer` asks its currency and its
/// roster each on its own call. Run on a pair whose access reads the journal alone and on one whose
/// access is log-backed (`compatibility::log_pair`), whose observation asks its currency too. That
/// the look serves them, observing the state lock once for all of them, is `aterm-secrets`'
/// `a_session_look_observes_once_and_judges_as_its_own_calls`.
/// NEGATIVE CONTROLS (2026-11-09): with `Replica::observe`'s peer check on its own call
/// (`Session::peer`), a read's admission asks two (`a read's admission`); with its write opened
/// by `Session::open_until`, a write's asks two (`a write's admission`); with
/// `Session::peer_member_in` reading the roster on its own call whatever its look, a read's asks
/// one; with the read's look dropped before its batch runs (`a read batch's own checks`), or
/// opened by `VaultLook::new` (`a read's own looks`), and with the session's currency (`Session::current`), its context
/// (`Session::current_observed`), its observation or its peer judged on its own call (`a read
/// batch's own checks`, the observation's over the log-backed pair), and with a write keeping its
/// look into its batch (`a write batch's own checks`), each fails.
#[test]
fn a_local_batch_is_admitted_through_one_look_at_the_vault() {
    for log_backed in [false, true] {
        let p = if log_backed {
            super::compatibility::log_pair()
        } else {
            Pair::new(false, 1)
        };
        let replica = p.replica(0);
        let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
        let round = replica.round(peer).unwrap();
        let own = || crate::table::OWN_CHECKS.get();
        let looks = aterm_secrets::client::test_looks_observed;
        let (before, looks_before) = (own(), looks());
        let (admitted, read_looks, batch) = round
            .local(peer, |session| {
                let (admitted, read_looks) = (own() - before, looks() - looks_before);
                assert_eq!(session.is_log_backed(), log_backed, "CONTROL: its access");
                session.peer(peer)?;
                session.observation()?;
                session.legacy_observation()?;
                session.control_context()?;
                let store = Store::observe(session, &p.paths[0], p.scope)?;
                assert_eq!(store.inventory(session)?.count(), 1, "CONTROL: its record");
                Ok((admitted, read_looks, own() - before - admitted))
            })
            .unwrap();
        assert_eq!(admitted, 0, "a read's admission (log-backed {log_backed})");
        assert_eq!(
            read_looks, 0,
            "a read's own looks (log-backed {log_backed})"
        );
        assert_eq!(
            batch, 0,
            "a read batch's own checks (log-backed {log_backed})"
        );
        let (before, looks_before) = (own(), looks());
        let (admitted, write_looks, batch) = round
            .local_write(peer, |session| {
                let (admitted, write_looks) = (own() - before, looks() - looks_before);
                session.peer(peer)?;
                Ok((admitted, write_looks, own() - before - admitted))
            })
            .unwrap();
        assert_eq!(admitted, 0, "a write's admission (log-backed {log_backed})");
        assert_eq!(
            write_looks, 1,
            "a write's own looks (log-backed {log_backed})"
        );
        assert_eq!(
            batch, 2,
            "a write batch's own checks (log-backed {log_backed})"
        );
    }
}

/// A store read batch's opening and closing fences (`Session::read_context`) ask the session's
/// currency afresh, on their own calls, though the session keeps its batch's look: a table-repair
/// read batch's store open over a journal pair's two records replays them in one read batch, whose
/// two fences each ask afresh ([`crate::table::FENCES_AFRESH`]), and whose other checks ask
/// nothing on their own calls ([`crate::table::OWN_CHECKS`]).
/// NEGATIVE CONTROL (2026-11-10): with `read_context` judging through the session's look, its
/// fences ask none afresh (`a read batch's fences`).
#[test]
fn a_store_read_batchs_fences_ask_afresh_under_a_kept_look() {
    let p = Pair::new(false, 2);
    let replica = p.replica(0);
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let fences = || crate::table::FENCES_AFRESH.get();
    let own = || crate::table::OWN_CHECKS.get();
    let (fenced, asked) = round
        .local(peer, |session| {
            assert!(!session.is_log_backed(), "CONTROL: its access");
            let (fences_before, own_before) = (fences(), own());
            let store = Store::observe(session, &p.paths[0], p.scope)?;
            let judged = (fences() - fences_before, own() - own_before);
            assert_eq!(store.inventory(session)?.count(), 2, "CONTROL: its records");
            Ok(judged)
        })
        .unwrap();
    assert_eq!(fenced, 2, "a read batch's fences");
    assert_eq!(asked, 0, "a read batch's own checks");
}

/// A table-repair read batch's record history and keys are judged through the look its session
/// keeps: over a log-backed pair (`compatibility::log_pair`), its probe of the history it needs
/// (`Session::history_needs`) makes no roster observation of its own
/// (`aterm_secrets::client::test_roster_observations`) and reads the log on no call of its own
/// (`aterm_secrets::client::test_logs_read`), and its store open, whose records' history and keys
/// it takes, makes none beyond its fences' own currency checks (`Session::read_context`,
/// [`crate::table::FENCES`]) and its closing check of each key it reused
/// ([`crate::table::KEYS_REVALIDATED`]), which ask afresh by design: a lone record, replayed with
/// no read batch and no fence, and two, replayed in one read batch whose keys it takes as it goes,
/// the second reusing the first's. A write batch's probe asks each on its own call.
/// NEGATIVE CONTROLS (2026-11-10): with `Session::history_needs` reading the log on its own call,
/// or asking `Bootstrap::holds_histories_in` with no look, its probe asks on its own (`a read
/// batch's history probe`); with `Session::decode_envelope` asking `Bootstrap::historical_in` with
/// no look, the store open asks more than its fences do (`a read batch's record history and keys`,
/// of one record and of two); with its key taken by `Historical::with_key_in` with no look, the
/// lone record's open (`... of one record`) or, for a read batch's keys, the two records' open
/// (`... of two records`) asks more.
#[test]
fn a_read_batchs_record_history_and_keys_are_judged_through_its_look() {
    let p = super::compatibility::log_pair();
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let asked = || {
        (
            aterm_secrets::client::test_roster_observations(),
            aterm_secrets::client::test_logs_read(),
        )
    };
    let since = |before: (usize, usize)| {
        let now = asked();
        (now.0 - before.0, now.1 - before.1)
    };
    let afresh = || {
        (
            crate::table::FENCES.get(),
            crate::table::KEYS_REVALIDATED.get(),
        )
    };
    // The probe's asks, and the store open's asks, fences and keys checked again, as one read
    // batch judges them.
    let read = |records: usize| {
        let replica = p.replica(0);
        let round = replica.round(peer).unwrap();
        round
            .local(peer, |session| {
                assert!(session.is_log_backed(), "CONTROL: its access");
                let before = asked();
                session.history_needs(None, 16)?;
                let probe = since(before);
                let (before, afresh_before) = (asked(), afresh());
                let store = Store::observe(session, &p.paths[0], p.scope)?;
                assert_eq!(
                    store.inventory(session)?.count(),
                    records,
                    "CONTROL: its records"
                );
                let opened = since(before);
                let now = afresh();
                Ok((
                    probe,
                    opened,
                    (now.0 - afresh_before.0, now.1 - afresh_before.1),
                ))
            })
            .unwrap()
    };
    let (probe, opened, fenced) = read(1);
    assert_eq!(probe, (0, 0), "a read batch's history probe");
    assert_eq!(
        fenced,
        (0, 0),
        "CONTROL: a lone record's replay has no fence"
    );
    assert_eq!(
        opened,
        (0, 0),
        "a read batch's record history and keys, of one record"
    );
    {
        let bootstrap = p.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        Store::open(&session, &p.paths[0], p.scope)
            .unwrap()
            .commit_catalog(&session, b"a second record")
            .unwrap();
    }
    let (probe, opened, fenced) = read(2);
    assert_eq!(
        probe,
        (0, 0),
        "a read batch's history probe, over two records"
    );
    assert!(
        fenced.0 > 0 && fenced.1 > 0,
        "CONTROL: two records' replay is fenced and reuses a key: {fenced:?}"
    );
    assert_eq!(
        opened,
        (fenced.0 + fenced.1, 0),
        "a read batch's record history and keys, of two records"
    );
    let replica = p.replica(0);
    let probe = replica
        .round(peer)
        .unwrap()
        .local_write(peer, |session| {
            let before = asked();
            session.history_needs(None, 16)?;
            Ok(since(before))
        })
        .unwrap();
    assert!(
        probe.0 > 0 && probe.1 > 0,
        "CONTROL: a write batch's probe asks on its own calls: {probe:?}"
    );
}

/// A table-repair read batch's store opens hold its vault through the look its session keeps,
/// which stands on the batch's own source read of the vault until the batch returns: a store open,
/// of a lone record and of two replayed in a read batch, takes no source read of its own
/// (`aterm_secrets::client::test_source_reads_held`), where a write batch's, whose look ended
/// before its batch ran, takes one. Where the lock file the batch's source read holds was put away
/// before its admission, the look holds nothing at the vault's path, and its store open takes a
/// source read of the lock that stands there: no writer takes that lock while the open replays.
/// NEGATIVE CONTROLS (2026-11-10): with `Session::local_source_reads` taking its own source read
/// whatever look the session keeps, a read batch's store open takes one (`a read batch's store
/// open's source reads`); with `Bootstrap::local_source_read_in` standing on a look whether or not
/// it holds the lock at the vault's path, a writer takes the lock that stands there while the open
/// replays (`a store open under a look on a lock put away`).
#[test]
fn a_read_batchs_store_opens_hold_its_vault_through_its_look() {
    use std::{cell::Cell, rc::Rc};
    let held = aterm_secrets::client::test_source_reads_held;
    for records in [1, 2] {
        let p = Pair::new(false, records);
        let replica = p.replica(0);
        let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
        let round = replica.round(peer).unwrap();
        let (read, count) = round
            .local(peer, |session| {
                let before = held();
                let store = Store::observe(session, &p.paths[0], p.scope)?;
                Ok((held() - before, store.inventory(session)?.count()))
            })
            .unwrap();
        assert_eq!(count, records, "CONTROL: its records");
        assert_eq!(
            read, 0,
            "a read batch's store open's source reads, of {records} records"
        );
        let written = round
            .local_write(peer, |session| {
                let before = held();
                Store::observe(session, &p.paths[0], p.scope)?;
                Ok(held() - before)
            })
            .unwrap();
        assert_eq!(
            written, 1,
            "CONTROL: a write batch's store open takes its own source read, of {records} records"
        );
    }

    let p = Pair::new(false, 1);
    let replica = p.replica(0);
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let dir = p.lab.paths[0].dir.clone();
    let contender: Rc<std::cell::OnceCell<std::fs::File>> = Rc::default();
    let _change = {
        let (dir, contender) = (dir.clone(), contender.clone());
        Change::install(0, move || {
            let lock = dir.join(".lock");
            let put_away = lock.with_extension("put-away");
            std::fs::rename(&lock, &put_away).unwrap();
            std::fs::copy(&put_away, &lock).unwrap();
            let file = aterm_uds::private_file::open_private(&dir, ".lock")
                .unwrap()
                .0;
            assert!(contender.set(file).is_ok());
        })
    };
    let acquired = Rc::new(Cell::new(false));
    let (read, count) = {
        let (contender, acquired) = (contender.clone(), acquired.clone());
        crate::table::store::with_replay_observer(
            move || {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the contender must not take the vault's lock while a store replays"
                )]
                let attempt = contender.get().expect("the lock put away").try_lock();
                match attempt {
                    Ok(()) => acquired.set(true),
                    Err(std::fs::TryLockError::WouldBlock) => {}
                    Err(error) => panic!("vault contender failed: {error}"),
                }
            },
            || {
                round.local(peer, |session| {
                    let before = held();
                    let store = Store::observe(session, &p.paths[0], p.scope)?;
                    Ok((held() - before, store.inventory(session)?.count()))
                })
            },
        )
        .unwrap()
    };
    if acquired.get() {
        contender.get().unwrap().unlock().unwrap();
    }
    assert_eq!(count, 1, "CONTROL: its record");
    assert!(
        !acquired.get(),
        "a store open under a look on a lock put away"
    );
    assert_eq!(
        read, 1,
        "a store open under a look on a lock put away takes its own source read"
    );
}

/// Another of this Mac's rounds ingesting into `p`'s first table: a writer's commit, which overtakes
/// an observer of that table it lands inside.
fn overtaking_ingest(p: &Pair) -> impl Fn() + 'static {
    let (bootstrap, path, scope) = (p.lab.bootstrap(0), p.paths[0].clone(), p.scope);
    move || {
        let writer = Session::open(&bootstrap).unwrap();
        Store::open(&writer, &path, scope)
            .unwrap()
            .commit_catalog(&writer, b"overtaking-ingest")
            .unwrap();
    }
}

/// A read that another of this Mac's rounds overtakes with an ingest is read afresh, not refused:
/// its observer sees the witness advance, and the next read holds the new operation.
///
/// NEGATIVE CONTROL: with `LOCAL_READS` at 1, the first read's refusal comes back and the round
/// is lost, as it was for both its peers.
#[test]
fn a_round_reads_afresh_when_another_rounds_ingest_overtakes_its_read() {
    let p = Pair::new(false, 1);
    let replica = p.replica(0);
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let overtake = overtaking_ingest(&p);

    let mut reads = 0;
    let known = round
        .local(peer, |session| {
            reads += 1;
            let store = Store::observe(session, &p.paths[0], p.scope)?;
            if reads == 1 {
                overtake();
            }
            Ok(store.inventory(session)?.count())
        })
        .unwrap();
    assert_eq!((reads, known), (2, 2));

    // A table that never stops advancing ends the reads.
    let mut reads = 0;
    let kept_advancing = round.local(peer, |session| {
        reads += 1;
        let store = Store::observe(session, &p.paths[0], p.scope)?;
        overtake();
        Ok(store.inventory(session)?.count())
    });
    assert_eq!(reads, LOCAL_READS);
    assert!(kept_advancing.unwrap_err().is_snapshot_changed());

    // Any other refusal is the read's answer at once.
    let mut reads = 0;
    let busy = round.local(peer, |_| -> Result<()> {
        reads += 1;
        Err(Error::busy())
    });
    assert_eq!(reads, 1);
    assert!(busy.unwrap_err().is_busy());
    let (mut refusal, mut cause) = (None, None);
    assert!(Round::table_read::<()>(Err(Error::busy()), &mut refusal, &mut cause).is_err());
    assert_eq!((refusal, cause), (None, None));
}

/// A conflict proof of `p`'s table by its second writer, which its first holds none of.
fn conflict_proof(p: &Pair) -> Vec<u8> {
    let author = p.lab.bootstrap(1);
    let session = Session::open(&author).unwrap();
    let mut store = Store::open(&session, &p.paths[1], p.scope).unwrap();
    let receipt = store.commit_catalog(&session, b"proof source").unwrap();
    let first = store.wire(&session, &receipt.id).unwrap().unwrap();
    let mut second = first.to_vec();
    let end = second.len() - 64;
    second[end - 1] ^= 1;
    crate::table::tests::resign(&author, &mut second);
    crate::table::conflict::Conflict::from_pair(&session, p.scope, first, &second)
        .unwrap()
        .wire()
        .to_vec()
}

/// A peer's bytes for a [`Stream`]: its inventory, available and holding nothing.
const EMPTY_INVENTORY: [u8; 5] = [1, 0, 0, 0, 0];

/// `Round::inventory` and `Round::conflicts` overtaken inside their store's open: the store's last
/// check is what refuses, and the round reads afresh. A table that keeps advancing through every
/// read defers, with the cause a store this side cannot read now has, and tells the peer so.
///
/// NEGATIVE CONTROL: without the `is_snapshot_changed` arm in `Round::inventory` or
/// `Round::conflicts`, the first overtaken open is the read's refusal and the round defers on it,
/// as the islands test's round did; without their `table_read`, a table that keeps advancing
/// costs both peers the round.
#[test]
fn a_rounds_table_reads_take_a_fresh_read_when_overtaken_and_defer_when_it_keeps_advancing() {
    use crate::table::store::{with_each_replay_observed, with_replay_observer};
    use aterm_types::network_state::RepairCause;

    let p = Pair::new(false, 1);
    let proof = conflict_proof(&p);
    let replica = p.replica(0);
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let id = p.scope.digest();

    let (mut refusal, mut cause) = (None, None);
    let inventory = with_replay_observer(overtaking_ingest(&p), || {
        round.inventory(peer, id, &mut refusal, &mut cause)
    })
    .unwrap();
    assert_eq!((refusal, cause), (None, None));
    assert_eq!(
        inventory
            .expect("an overtaken read is read afresh")
            .known
            .len(),
        2,
        "the fresh read holds the overtaking operation"
    );

    let overtake = {
        let (bootstrap, path, scope) = (p.lab.bootstrap(0), p.paths[0].clone(), p.scope);
        let proof = proof.clone();
        move || {
            let writer = Session::open(&bootstrap).unwrap();
            Store::open(&writer, &path, scope)
                .unwrap()
                .ingest_conflict(&writer, &proof)
                .unwrap();
        }
    };
    let mut incoming = EMPTY_INVENTORY.to_vec();
    incoming.extend(0u32.to_le_bytes());
    incoming.extend(Sha256::digest(&proof));
    incoming.push(1);
    let mut stream = Stream {
        input: Cursor::new(incoming),
        output: Vec::new(),
    };
    let (mut repair, mut report) = (Repair::default(), Report::default());
    let went_on = with_replay_observer(overtake, || {
        round.conflicts(
            &mut stream,
            peer,
            Side::Responder,
            id,
            &mut repair,
            &mut report,
        )
    })
    .unwrap();
    assert!(went_on);
    assert_eq!(repair.deferral, None);
    assert_eq!(
        (report.deferred, report.conflicts_sent),
        (0, 1),
        "the fresh read holds the overtaking proof, which the peer lacked"
    );

    let (mut refusal, mut cause) = (None, None);
    let inventory = with_each_replay_observed(overtaking_ingest(&p), || {
        round.inventory(peer, id, &mut refusal, &mut cause)
    })
    .unwrap();
    assert!(inventory.is_none());
    assert!(refusal.unwrap().contains("reopen and verify"));
    assert_eq!(cause, Some(RepairCause::NotReady));

    let mut stream = Stream {
        input: Cursor::new(EMPTY_INVENTORY.to_vec()),
        output: Vec::new(),
    };
    let (mut repair, mut report) = (Repair::default(), Report::default());
    let went_on = with_each_replay_observed(overtaking_ingest(&p), || {
        round.conflicts(
            &mut stream,
            peer,
            Side::Responder,
            id,
            &mut repair,
            &mut report,
        )
    })
    .unwrap();
    assert!(!went_on);
    let why = repair.deferral.unwrap();
    assert_eq!(why.inventory, "conflicts");
    assert_eq!(why.local_cause, Some(RepairCause::NotReady));
    assert!(!why.retaining_primary_conflicts && !why.peer_unavailable);
    assert_eq!(report.deferred, 1);
    assert_eq!(
        stream.output,
        [0],
        "the peer hears the inventory is unavailable"
    );
}

/// A table that keeps advancing through every read of an outgoing batch fails the round, with
/// nothing on the wire, rather than sending the empty batch `receive_batch` would accept: a peer's
/// round that hears an inventory holding what it lacks and then an empty batch reads as having
/// left nothing to fetch, which the second half shows.
///
/// NEGATIVE CONTROL: with `send_batch` sending an empty batch on this refusal, the first half
/// fails. With `drained_table` no longer believing an empty batch, the second half fails, and the
/// sender could defer.
#[test]
fn a_batch_its_sender_cannot_read_fails_the_round_since_an_empty_one_says_nothing_is_left() {
    use crate::table::store::with_each_replay_observed;

    let p = Pair::new(false, 1);
    let source = p.lab.bootstrap(0);
    let session = Session::observe(&source).unwrap();
    let sender = session.actor();
    let held: Vec<Digest> = Store::observe(&session, &p.paths[0], p.scope)
        .unwrap()
        .inventory(&session)
        .unwrap()
        .copied()
        .collect();
    drop(session);
    let id = p.scope.digest();
    let receiver_actor = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let replica = p.replica(0);
    let round = replica.round(receiver_actor).unwrap();

    let mut stream = Stream::default();
    let mut report = Report::default();
    let refused = with_each_replay_observed(overtaking_ingest(&p), || {
        round.send_batch(
            &mut stream,
            receiver_actor,
            |session| {
                round.prepare_batch(session, id, &held, |session, store, key| {
                    Ok(store.wire(session, key)?.unwrap().to_vec())
                })
            },
            BatchKind::Operation,
            &mut report,
        )
    });
    assert!(refused.unwrap_err().is_snapshot_changed());
    assert!(stream.output.is_empty(), "no batch said nothing is left");

    let mut incoming = EMPTY_INVENTORY.to_vec();
    incoming.extend(0u32.to_le_bytes());
    incoming.push(1);
    incoming.extend(u32::try_from(held.len()).unwrap().to_le_bytes());
    incoming.extend(held.concat());
    incoming.extend(0u32.to_le_bytes());
    let mut stream = Stream {
        input: Cursor::new(incoming),
        output: Vec::new(),
    };
    let replica = p.replica(1);
    let round = replica.round(sender).unwrap();
    let mut report = Report {
        table: Some(id),
        ..Report::default()
    };
    round
        .table(
            &mut stream,
            sender,
            Side::Responder,
            id,
            &mut Repair::default(),
            &mut report,
        )
        .unwrap();
    assert_eq!(p.count(1), 0, "the peer still lacks what the sender holds");
    assert_eq!(
        crate::runtime::RepairRound::completed_with(report).drained_table(),
        Some(id)
    );
}

#[test]
fn policy_changes_before_session_open_refuse_the_old_round() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let config = lab.paths[0].home.join("config.toml");
    let replica = Replica::commons(lab.paths[0].clone(), config.clone()).unwrap();
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let _change = Change::install(0, move || {
        aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    });
    let called = std::cell::Cell::new(false);
    let result = round.local(peer, |_| {
        called.set(true);
        Ok(())
    });
    assert!(result.is_err(), "late disable must refuse the local batch");
    assert!(!called.get(), "a refused batch must never run");
}

#[derive(Default)]
struct Stream {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}
impl Read for Stream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.input.read(bytes)
    }
}
impl Write for Stream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Fixture {
    lab: Lab,
    config: PathBuf,
    directory: crate::catalog::Directory,
    scope: Scope,
    peer: Actor,
    original: Vec<u8>,
    incoming: Vec<u8>,
    observation: [u8; 64],
}
impl Fixture {
    fn new() -> Self {
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
        let config = lab.paths[0].home.join("config.toml");
        let a = lab.bootstrap(0);
        let directory = crate::catalog::Directory::new(&lab.paths[0], a.trust());
        let session = Session::open(&a).unwrap();
        let scope = directory
            .open_application(&session, "accounts", "proposals", 1)
            .unwrap();
        let mut store = directory.enroll(&session, scope).unwrap();
        let receipt = store
            .commit_catalog(&session, b"retained account proposal")
            .unwrap();
        let original = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
        let observation = session.observation().unwrap();
        drop((store, session));
        let b = lab.bootstrap(1);
        let peer = b.request().unwrap().binding().actor();
        let other = crate::catalog::Directory::new(&lab.paths[1], b.trust());
        let session = Session::open(&b).unwrap();
        assert_eq!(
            other
                .open_application(&session, "accounts", "proposals", 1)
                .unwrap(),
            scope
        );
        let mut store = other.enroll(&session, scope).unwrap();
        let receipt = store
            .commit_catalog(&session, b"incoming account proposal")
            .unwrap();
        let incoming = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
        drop((store, session));
        Self {
            lab,
            config,
            directory,
            scope,
            peer,
            original,
            incoming,
            observation,
        }
    }
    fn config(&self, policy: i64) -> Vec<u8> {
        let excluded = match policy {
            2 => [77; 32],
            4 => self.scope.digest(),
            5 => self.directory.catalog_scope().digest(),
            _ => [0; 32],
        };
        match policy {
            0 => b"[commons]\n".to_vec(),
            1 => b"# semantically unchanged\n[commons]\nexclude_tables=[]\nenabled=true\n".to_vec(),
            2 | 4 | 5 => format!(
                "[commons]\nexclude_tables=['{}']\n",
                aterm_codec::hex::encode_lower(&excluded)
            )
            .into_bytes(),
            3 => b"[commons]\nenabled=false\n".to_vec(),
            6 => b"[commons]\nenabled='false'\n".to_vec(),
            7 => b"[commons]\nexclude_table=[]\n".to_vec(),
            _ => panic!("unknown policy scenario"),
        }
    }
    fn batch(&self, round: &Round<'_>, receiving: bool) -> (bool, bool) {
        round.active_table.set(Some(self.scope.digest()));
        let mut stream = Stream::default();
        let mut report = Report::default();
        let result = if receiving {
            let mut wire = 1u32.to_le_bytes().to_vec();
            control::send_frame(&mut wire, &self.incoming).unwrap();
            stream.input = Cursor::new(wire);
            round.receive_batch(
                &mut stream,
                self.peer,
                self.scope.digest(),
                BatchKind::Operation,
                &mut Repair::default(),
                &mut report,
            )
        } else {
            let mut receipt = Sha256::digest(&self.original).to_vec();
            receipt.push(1);
            stream.input = Cursor::new(receipt);
            round.send_batch(
                &mut stream,
                self.peer,
                |_| Ok(vec![self.original.clone()]),
                BatchKind::Operation,
                &mut report,
            )
        };
        if result.is_ok() {
            if receiving {
                let mut receipt = Sha256::digest(&self.incoming).to_vec();
                receipt.push(1);
                assert_eq!(stream.output, receipt);
                assert_eq!(report.received, 1);
            } else {
                let mut expected = 1u32.to_le_bytes().to_vec();
                control::send_frame(&mut expected, &self.original).unwrap();
                assert_eq!(stream.output, expected);
                assert_eq!(report.sent, 1);
            }
        } else {
            assert!(
                stream.output.is_empty(),
                "refusal emitted a frame or receipt"
            );
            assert_eq!(report, Report::default());
        }
        (result.is_ok(), !stream.output.is_empty())
    }
    fn observe(&self, accepted: bool, receipts: bool) -> (i64, i64, i64) {
        let bootstrap = self.lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        assert_eq!(session.observation().unwrap(), self.observation);
        let store = self.directory.enroll(&session, self.scope).unwrap();
        assert_eq!(
            store
                .wire(&session, &Sha256::digest(&self.original))
                .unwrap()
                .unwrap(),
            self.original
        );
        (
            i64::from(accepted),
            i64::from(receipts),
            store.inventory(&session).unwrap().count() as i64,
        )
    }
    fn round<'a>(&self, replica: &'a Replica) -> Result<Round<'a>> {
        let mut round = replica.round(self.peer)?;
        round.tables.insert(
            self.scope.digest(),
            (self.scope, self.directory.path(self.scope).unwrap()),
        );
        Ok(round)
    }
}

#[test]
fn outbound_policy_is_rechecked_after_preparing_payload() {
    for policy in [1, 3, 4, 6] {
        let fixture = Fixture::new();
        let replica =
            Replica::commons(fixture.lab.paths[0].clone(), fixture.config.clone()).unwrap();
        let round = fixture.round(&replica).unwrap();
        round.active_table.set(Some(fixture.scope.digest()));
        let mut receipt = Sha256::digest(&fixture.original).to_vec();
        receipt.push(1);
        let mut stream = Stream {
            input: Cursor::new(receipt),
            ..Stream::default()
        };
        let mut report = Report::default();
        let result = round.send_batch(
            &mut stream,
            fixture.peer,
            |_| {
                aterm_secrets::fsx::write_atomic(&fixture.config, &fixture.config(policy), 0o600)
                    .unwrap();
                Ok(vec![fixture.original.clone()])
            },
            BatchKind::Operation,
            &mut report,
        );
        if policy == 1 {
            result.expect("a semantically identical policy still permits the payload");
            assert_eq!(report.sent, 1);
            assert!(!stream.output.is_empty());
        } else {
            assert!(
                result.is_err(),
                "a policy change during preparation must refuse"
            );
            assert!(
                stream.output.is_empty(),
                "refusal must precede the batch header"
            );
            assert_eq!(report, Report::default());
        }
    }
}

fn scenario(policy: i64, receiving: bool, point: u8) -> [(i64, i64, i64); 3] {
    let fixture = Fixture::new();
    let replica = Replica::commons(fixture.lab.paths[0].clone(), fixture.config.clone()).unwrap();
    let round = fixture.round(&replica).unwrap();
    let config = fixture.config.clone();
    let bytes = fixture.config(policy);
    let changed = std::rc::Rc::new(std::cell::Cell::new(false));
    let invoked = changed.clone();
    let change = Change::install(point, move || {
        aterm_secrets::fsx::write_atomic(&config, &bytes, 0o600).unwrap();
        invoked.set(true);
    });
    let (accepted, receipts) = fixture.batch(&round, receiving);
    assert!(
        changed.get(),
        "the policy fault must execute at session admission"
    );
    drop(change);
    let first = fixture.observe(accepted, receipts);
    let (accepted, receipts) = fixture
        .round(&replica)
        .map(|round| fixture.batch(&round, receiving))
        .unwrap_or((false, false));
    let retry = fixture.observe(accepted, receipts);
    aterm_secrets::fsx::write_atomic(&fixture.config, &fixture.config(0), 0o600).unwrap();
    let (accepted, receipts) = fixture.batch(&fixture.round(&replica).unwrap(), receiving);
    let restored = fixture.observe(accepted, receipts);
    [first, retry, restored]
}

/// Whether a fixture selection's real run is walked: half of them, so that each of the 8 policies
/// meets both directions and both session boundaries. The direction and the point only choose the
/// entry to `Round::admit` and which boundary carries the fault, and both reach the same
/// `check_table_policy`; `outbound_policy_is_rechecked_after_preparing_payload` holds the send-only
/// recheck. GAP: a defect specific to one policy, one direction and one boundary together is not
/// seen for the other 16 selections.
const fn walked(policy: i64, receiving: bool, point: u8) -> bool {
    (policy + receiving as i64 + point as i64) % 2 == 0
}

/// [`scenario`] once per fixture selection: Attempt, Retry and Restore read the three
/// phases of one real run, so a selection runs once, not once per phase.
fn scenario_once(policy: i64, receiving: bool, point: u8) -> [(i64, i64, i64); 3] {
    type Runs = BTreeMap<(i64, bool, u8), [(i64, i64, i64); 3]>;
    static RUNS: std::sync::Mutex<Runs> = std::sync::Mutex::new(BTreeMap::new());
    assert!(
        walked(policy, receiving, point),
        "a selection outside the walk would run a Lab the test does not count"
    );
    let key = (policy, receiving, point);
    if let Some(run) = RUNS.lock().unwrap().get(&key) {
        return *run;
    }
    let run = scenario(policy, receiving, point);
    *RUNS.lock().unwrap().entry(key).or_insert(run)
}

fn policy_transition(state: &aterm_spec::interp::State, action: &str) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "NextPolicy" => {
            next.insert("policy", state["policy"] + 1);
        }
        "Receiving" => {
            next.insert("receiving", 1);
        }
        "AfterOpen" => {
            next.insert("point", 1);
        }
        "Attempt" | "Retry" | "Restore" => {
            let index = match action {
                "Attempt" => 0,
                "Retry" => 1,
                _ => 2,
            };
            let (accepted, receipts, retained) = scenario_once(
                state["policy"],
                state["receiving"] == 1,
                state["point"] as u8,
            )[index];
            next.insert("phase", index as i64 + 1);
            next.insert("accepted", accepted);
            next.insert("receipts", receipts);
            next.insert("retained", retained);
        }
        _ => panic!("unknown policy action"),
    }
    next
}

#[test]
fn policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected() {
    let model = aterm_models_db::repair_policy_model();
    assert_eq!(aterm_spec::interp::prove_and_catch(&model), 128);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    let states = aterm_spec::interp::reachable_states(&model);
    let selection = |state: &aterm_spec::interp::State| {
        (
            state["policy"],
            state["receiving"] == 1,
            state["point"] as u8,
        )
    };
    // Each walked selection's real run, each on its own Lab, on the lane's places. A new policy or
    // fixture axis moves these counts.
    let selections: BTreeSet<_> = states.iter().map(selection).collect();
    let run: Vec<_> = selections
        .iter()
        .copied()
        .filter(|&(policy, receiving, point)| walked(policy, receiving, point))
        .collect();
    assert_eq!((selections.len(), run.len()), (32, 16));
    aterm_spec::fs_lane::each(&run, |&(policy, receiving, point)| {
        scenario_once(policy, receiving, point)
    });
    for state in states {
        let (policy, receiving, point) = selection(&state);
        for action in &model.actions {
            if matches!(action.name, "Attempt" | "Retry" | "Restore")
                && !walked(policy, receiving, point)
            {
                continue;
            }
            if model.action_enabled(action.name, &state) {
                let next = policy_transition(&state, action.name);
                let successors = model.successors(action.name, &state);
                assert!(
                    successors.contains(&next),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                if matches!(action.name, "NextPolicy" | "Receiving" | "AfterOpen") {
                    assert!(
                        !successors.contains(&state),
                        "{}: ignoring a fixture selection must fail conformance",
                        action.name
                    );
                }
            }
        }
    }
    let mut changed = model.init_state();
    changed.insert("policy", 3);
    changed.insert("point", 1);
    let mut forged = policy_transition(&changed, "Attempt");
    forged.insert("accepted", 1);
    assert!(!model.successors("Attempt", &changed).contains(&forged));
    assert!(!model.check_invariant("LateChangeRefuses", &forged));
    forged.insert("accepted", 0);
    forged.insert("receipts", 1);
    assert!(!model.check_invariant("NoFalseReceipt", &forged));
}

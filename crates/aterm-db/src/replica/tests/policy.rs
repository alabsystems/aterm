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

/// What [`table_admission`] calls, given the round's active table.
type AdmissionHook = Box<dyn FnMut(Option<Digest>)>;
thread_local! {
    static TABLE_ADMISSION: RefCell<Option<AdmissionHook>> = RefCell::new(None);
}

/// Each round admission's hook on this thread (`Round::admit`), given the round's active table,
/// before the admission reads its sources or opens its session: no journal lock or writer of this
/// side is held then. Where the round's group keeps a look at this hook (`GroupLook`: the look of
/// the read admission before it, whose glance first saw the control anchor settled, with no call
/// on the round's stream, write admission, witness read or batch error since), that look holds
/// this side's vault state lock shared: a read admission there is a follower, lent it
/// (`Replica::follow`), and a write admission lets it go first. A hook that changes the vault
/// there does it under that hold, as a hook that ingests into the control journal or renames a
/// file can.
pub(in crate::replica) fn table_admission(active: Option<Digest>) {
    let hook = TABLE_ADMISSION.with(|slot| slot.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook(active);
        TABLE_ADMISSION.with(|slot| {
            slot.borrow_mut().get_or_insert(hook);
        });
    }
}

/// [`table_admission`]'s hook while this lives, on this thread.
pub(super) struct Admissions;
impl Admissions {
    pub(super) fn install(hook: impl FnMut(Option<Digest>) + 'static) -> Self {
        TABLE_ADMISSION.with(|slot| {
            assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
        });
        Self
    }
}
impl Drop for Admissions {
    fn drop(&mut self) {
        TABLE_ADMISSION.with(|slot| {
            slot.borrow_mut().take();
        });
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
/// once after it: `Replica::admit`) hands one look at the vault
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
/// NEGATIVE CONTROLS (2026-11-09): with `Replica::admit`'s peer check on its own call
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

/// A local read batch's look derives each value of the moment it stands for once
/// (`aterm_secrets::client::test_look_derivations`): its admission and the checks its batch makes
/// through it (a peer's currency and roster, its observation and its legacy one, an endpoint's
/// context) judge the look's lock, stamp each set of the vault's files, look at its state, digest
/// the log's state and judge the access's currency, each at least once, and none of them again
/// after the look kept it. Run on a log-backed pair (`compatibility::log_pair`), its files judged
/// settled (`client::test_as_if_later`), after a batch that warms the caches; the exact counts are
/// `aterm-secrets`' look laws', whose caches are each test's own.
/// NEGATIVE CONTROL (2026-11-11): with `Under::currency` never giving what it kept, the measured
/// batch judges its currency 5 times while its look kept it (`rederived_after_kept` `currency:
/// 5`, `a read batch's look derives each value once`).
#[test]
fn a_local_read_batch_derives_each_value_of_its_look_once() {
    let p = super::compatibility::log_pair();
    let replica = p.replica(0);
    let peer = p.lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    let batch = |session: &Session<'_>| -> Result<()> {
        session.peer(peer)?;
        session.observation()?;
        session.legacy_observation()?;
        session.control_context()?;
        Ok(())
    };
    aterm_secrets::client::test_as_if_later(Duration::from_secs(3600), || {
        round.local(peer, batch).unwrap();
        let before = aterm_secrets::client::test_look_derivations();
        round.local(peer, batch).unwrap();
        let derived = aterm_secrets::client::test_look_derivations().since(before);
        let each = |counts: aterm_secrets::client::Derivations| {
            [
                counts.lock_judgments,
                counts.vault_stamps,
                counts.observation_stamps,
                counts.state_looks,
                counts.log_states,
                counts.currency,
            ]
        };
        assert!(
            each(derived.derived).iter().all(|derived| *derived >= 1),
            "a read batch's look derives each value: {derived:?}"
        );
        assert_eq!(
            derived.rederived_after_kept,
            aterm_secrets::client::Derivations::default(),
            "a read batch's look derives each value once"
        );
    });
}

/// A store read batch's opening and closing fences (`Session::read_context`,
/// `Session::close_read`, each through `Session::fenced`) ask the session's currency afresh, on
/// their own calls, where its access is not log-backed, though the session keeps its batch's look:
/// a table-repair read batch's store open over a journal pair's two records replays them in one
/// read batch, whose two fences each ask afresh ([`crate::table::FENCES_AFRESH`]), and whose other
/// checks ask nothing on their own calls ([`crate::table::OWN_CHECKS`]).
/// NEGATIVE CONTROL (2026-11-11, against both fences' `Session::fenced`): with `fenced` judging
/// its context through the session's look (`context_judged(scope, false)`), its fences ask none
/// afresh (`a read batch's fences`: `left: 0, right: 2`).
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
/// it takes, makes none beyond its opening fence's own currency check (`Session::read_context`;
/// [`crate::table::FENCES`] counts it and the closing) and its closing check of each key it reused
/// ([`crate::table::KEYS_REVALIDATED`]), which ask afresh by design: a lone record, replayed with
/// no read batch and no fence, and two, replayed in one read batch whose keys it takes as it goes,
/// the second reusing the first's. The batch's closing (`Session::close_read`,
/// [`crate::table::CLOSES`]) judges its currency through one look of its own
/// (`aterm_secrets::client::test_looks_observed`), which observes no roster of its own. A write
/// batch's probe asks each on its own call.
/// NEGATIVE CONTROLS (2026-11-11): with `Session::close_read` judging its currency by
/// `Access::is_current`, on its own call, the two records' open makes one more roster
/// observation (`... of two records`: `left: (3, 0), right: (2, 0)`); with `close_read` judging
/// through the look the session keeps, it opens no look of its own (`a read batch's closing
/// looks`: `left: 0, right: 1`).
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
            crate::table::CLOSES.get(),
            aterm_secrets::client::test_looks_observed(),
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
                    (now.2 - afresh_before.2, now.3 - afresh_before.3),
                ))
            })
            .unwrap()
    };
    let (probe, opened, fenced, (closed, looked)) = read(1);
    assert_eq!(probe, (0, 0), "a read batch's history probe");
    assert_eq!(
        (fenced, closed, looked),
        ((0, 0), 0, 0),
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
    let (probe, opened, fenced, (closed, looked)) = read(2);
    assert_eq!(
        probe,
        (0, 0),
        "a read batch's history probe, over two records"
    );
    assert!(
        fenced.0 > 0 && fenced.1 > 0 && closed == 1,
        "CONTROL: two records' replay is fenced, closes once and reuses a key: {fenced:?} \
         {closed}"
    );
    assert_eq!(
        opened,
        (fenced.0 - closed + fenced.1, 0),
        "a read batch's record history and keys, of two records"
    );
    assert_eq!(looked, closed, "a read batch's closing looks");
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

/// How far a round group's laws move their clocks: far enough that every file their fixtures
/// wrote reads as settled long ago (`aterm_secrets::client::test_as_if_later`), so a group's first
/// look finds the control anchor settled and the group keeps it for its followers (`GroupLook`).
const SETTLED: Duration = Duration::from_secs(3600);

/// One side's end of a round's stream that, at each call, first tries the side's state lock
/// exclusively (a raw `try_lock`, as a writer would) and records the first call that found it
/// held, counting the calls and the bytes this side writes.
struct Probe<S> {
    inner: S,
    dir: PathBuf,
    calls: usize,
    written: usize,
    held: Option<(usize, &'static str)>,
}
impl<S> Probe<S> {
    fn new(inner: S, dir: &std::path::Path) -> Self {
        Self {
            inner,
            dir: dir.to_owned(),
            calls: 0,
            written: 0,
            held: None,
        }
    }
    fn try_writer(&mut self, call: &'static str) {
        self.calls += 1;
        let (lock, _) = aterm_uds::private_file::open_private(&self.dir, ".lock").unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "the probe asks whether a look holds the state lock across a stream call"
        )]
        let attempt = lock.try_lock();
        match attempt {
            Ok(()) => lock.unlock().unwrap(),
            Err(std::fs::TryLockError::WouldBlock) => {
                self.held.get_or_insert((self.calls, call));
            }
            Err(error) => panic!("state lock probe failed: {error}"),
        }
    }
}
impl<S: Read> Read for Probe<S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.try_writer("read");
        self.inner.read(bytes)
    }
}
impl<S: Write> Write for Probe<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.try_writer("write");
        let written = self.inner.write(bytes)?;
        self.written += written;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.try_writer("flush");
        self.inner.flush()
    }
}

/// What one side of a [`probed_round`] saw.
#[derive(Debug)]
struct Sided {
    /// Its report, or its error.
    result: std::result::Result<Report, String>,
    /// Its round admissions (`table_admission`'s hooks).
    hooks: usize,
    /// Its read admissions through its round group (own, lent, reopened: `grouped`).
    grouped: (u32, u32, u32),
    /// The bytes it wrote.
    written: usize,
    /// Its stream calls, and the first that found its state lock held ([`Probe`]).
    calls: usize,
    held: Option<(usize, &'static str)>,
}

/// One repair round of `replica` on `stream` (`Replica::exchange`) where `grouped`; otherwise the
/// same exchange with its round outside any group, on the same kind of stream (a `Wire` of a group
/// nothing joins).
fn exchange_grouped(
    replica: &Replica,
    stream: &mut (impl Read + Write),
    peer: Actor,
    side: Side,
    repair: &mut Repair,
    grouped: bool,
) -> Result<Report> {
    if grouped {
        return replica.exchange(stream, peer, side, repair);
    }
    repair.deferral = None;
    repair.batch_deferral = None;
    repair.interrupted = None;
    repair.clock = None;
    let group = GroupLook::new(&replica.paths);
    let round = replica.round_in(peer, None)?;
    round.exchange_with_policy(
        &mut Wire {
            stream,
            group: &group,
        },
        peer,
        side,
        repair,
        ProbePolicy::Projection,
    )
}

/// One repair round between `pair`'s first Mac (X, on this thread, as `side`, through its round
/// group where `grouped`) and its second (on a thread of its own, through its group), over a socket
/// pair whose ends are [`Probe`]s, each side's files read as settled ([`SETTLED`]); `repairs` are
/// X's and the second Mac's. `at` runs at each of X's round admissions, given its count from 1
/// and the round's active table, before the admission reads its sources.
fn probed_round(
    pair: &Pair,
    repairs: &mut [Repair; 2],
    side: Side,
    grouped: bool,
    mut at: impl FnMut(usize, Option<Digest>) + 'static,
) -> [Sided; 2] {
    use std::{cell::Cell, rc::Rc};
    let (x, y) = (pair.replica(0), pair.replica(1));
    let (x_actor, y_actor) = (x.actor, y.actor);
    let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
    for stream in [&left, &right] {
        // Hang detectors: every read and write of a round that runs is answered at once.
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .unwrap();
    }
    let other_side = match side {
        Side::Initiator => Side::Responder,
        Side::Responder => Side::Initiator,
    };
    let [rx, ry] = repairs;
    let run = |replica: &Replica,
               stream: std::os::unix::net::UnixStream,
               dir: &std::path::Path,
               peer: Actor,
               side: Side,
               repair: &mut Repair,
               grouped: bool| {
        aterm_secrets::client::test_as_if_later(SETTLED, || {
            let before = super::grouped();
            let mut probe = Probe::new(stream, dir);
            let result = exchange_grouped(replica, &mut probe, peer, side, repair, grouped);
            let after = super::grouped();
            let Probe {
                calls,
                written,
                held,
                ..
            } = probe;
            Sided {
                result: result.map_err(|error| error.to_string()),
                hooks: 0,
                grouped: (after.0 - before.0, after.1 - before.1, after.2 - before.2),
                written,
                calls,
                held,
            }
        })
    };
    let y_dir = pair.lab.paths[1].dir.clone();
    thread::scope(|threads| {
        let other = threads.spawn(move || run(&y, right, &y_dir, x_actor, other_side, ry, true));
        let hooks = Rc::new(Cell::new(0));
        let counted = Rc::clone(&hooks);
        let hook = Admissions::install(move |active| {
            counted.set(counted.get() + 1);
            at(counted.get(), active);
        });
        let mut ours = run(&x, left, &pair.lab.paths[0].dir, y_actor, side, rx, grouped);
        drop(hook);
        ours.hooks = hooks.get();
        [ours, other.join().unwrap()]
    })
}

/// A converged [`Pair`] and the repairs two rounds left it, X as `side` in both: the first moves
/// its one record and asks the peer's witnesses, the second moves nothing and asks none, so a
/// round after them moves nothing and asks no witness ([`WITNESS_GAP_MS`]).
fn converged(side: Side) -> (Pair, [Repair; 2]) {
    let pair = Pair::new(false, 1);
    let mut repairs = [Repair::default(), Repair::default()];
    for moved in [1, 0] {
        let [x, y] = probed_round(&pair, &mut repairs, side, true, |_, _| {});
        let (x, y) = (x.result.unwrap(), y.result.unwrap());
        assert_eq!(
            x.sent + x.received + y.sent + y.received,
            moved * 2,
            "CONTROL: the warm-up rounds move the pair's record once"
        );
    }
    (pair, repairs)
}

/// LAW G-L2: no look at the vault is held across a call on a round's stream (`Wire`). Over a
/// journal pair (`Pair::new(false, 1)`) and a log-backed one (`compatibility::log_pair`), a round
/// that moves the first Mac's record and a quiet one after it, each side's files read as settled
/// so its groups keep their looks for followers, every read, write and flush either side makes
/// on its stream first tries that side's state lock exclusively, as a writer would ([`Probe`]),
/// and takes it. CONTROL: each side's quiet round lends its group's look to some admissions.
/// NEGATIVE CONTROLS (2026-11-12): `Wire::flush` not letting the look go fails at the initiator's
/// ninth call, a flush (`side 0's (call, kind)`: `Some((9, "flush"))`), its keys' send with no key
/// to write; no `Read` method of `Wire` letting it go fails at the responder's first call, its
/// probe's read (`side 1's`: `Some((1, "read"))`). `Wire::read` alone not letting it go passes:
/// a round reads through `read_exact`, which `Wire` forwards.
#[test]
fn no_look_is_held_across_a_call_on_the_rounds_stream() {
    for log_backed in [false, true] {
        let pair = if log_backed {
            super::compatibility::log_pair()
        } else {
            Pair::new(false, 1)
        };
        let mut repairs = [Repair::default(), Repair::default()];
        for moved in [1, 0] {
            let sides = probed_round(&pair, &mut repairs, Side::Initiator, true, |_, _| {});
            for (n, side) in sides.iter().enumerate() {
                let report = side.result.as_ref().expect("CONTROL: the round completes");
                assert_eq!(
                    report.sent + report.received,
                    moved,
                    "CONTROL: the record moves once"
                );
                assert!(side.calls > 0, "CONTROL: side {n} called its stream");
                assert_eq!(
                    side.held, None,
                    "side {n}'s (call, kind) that found its state lock held (log-backed \
                     {log_backed}, moved {moved})"
                );
                if moved == 0 {
                    assert!(
                        side.grouped.1 > 0,
                        "CONTROL: side {n}'s quiet round lends its group's look: {side:?}"
                    );
                }
            }
        }
    }
}

/// Whether X's state lock is held where this asks, as [`Probe`] asks it at a stream call: at a
/// round admission's hook, whether a group's look holds it for a follower.
fn lock_held(dir: &std::path::Path) -> bool {
    let (lock, _) = aterm_uds::private_file::open_private(dir, ".lock").unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the hook asks whether a group's look holds the state lock"
    )]
    let attempt = lock.try_lock();
    match attempt {
        Ok(()) => {
            lock.unlock().unwrap();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(error) => panic!("state lock probe failed: {error}"),
    }
}

/// What a [`Sided`] reports, for comparing runs over different fixtures: its report's counts
/// (sent, received, keys sent and received, conflicts sent and received, deferred) and whether it
/// observed a matching inventory, or its error with each digest in it spelled `<id>` (each fixture
/// is a lab of its own, with tables of its own), beside its round admissions and the bytes it
/// wrote.
type Outcome = (
    std::result::Result<([usize; 7], bool), String>,
    usize,
    usize,
);

fn outcome(side: &Sided) -> Outcome {
    let result = match &side.result {
        Ok(report) => Ok((
            [
                report.sent,
                report.received,
                report.keys_sent,
                report.keys_received,
                report.conflicts_sent,
                report.conflicts_received,
                report.deferred,
            ],
            report.matching_inventory.is_some(),
        )),
        Err(error) => Err(error
            .split(' ')
            .map(|word| {
                if word.len() == 64 && word.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    "<id>"
                } else {
                    word
                }
            })
            .collect::<Vec<_>>()
            .join(" ")),
    };
    (result, side.hooks, side.written)
}

/// LAW G-L3: a control change at any admission is refused at that admission, grouped or not.
/// Over a converged journal pair ([`converged`]), the first Mac's (X's) retirement of the second
/// is prepared before the round, and ingested into X's control journal at X's k-th round
/// admission's hook, for every k from 1 to X's admissions in a quiet round, as initiator and as
/// responder, its round through its group (`GroupLook`: some of those admissions are lent its
/// look, as the control finds) and outside any. Every run refuses at admission k, with no hook
/// after it, except at X's last, its matching inventory's fenced read
/// (`Round::fenced_observation`), whose refusal leaves the read to its fallback
/// (`Round::inventory`), one more admission, which refuses too: the round completes with no
/// matching inventory. Each grouped run writes the same bytes and ends as its group-less run
/// does: a follower reads the control journal itself (`Session::observe_after_in`).
/// NEGATIVE CONTROL (2026-11-12): a group's first follower opening its session on the control
/// journal its group's first admission read, kept for it, accepts the change at k and refuses
/// at the next admission, after writing more (`Initiator at 1: grouped (Err("table repair peer
/// is not a current member"), 2, 73), group-less (Err(..), 1, 0)`; so at 3, 6, 10 and 14 as
/// initiator and at 1 and 6 as responder, each a group's first follower).
#[test]
fn a_control_change_at_any_admission_refuses_there() {
    use std::{cell::Cell, rc::Rc};
    let sides = [Side::Initiator, Side::Responder];
    let quiet = aterm_spec::fs_lane::each(&sides, |&side| {
        let (pair, mut repairs) = converged(side);
        let dir = pair.lab.paths[0].dir.clone();
        let lent = Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = Rc::clone(&lent);
        let [x, _] = probed_round(&pair, &mut repairs, side, true, move |_, _| {
            seen.borrow_mut().push(lock_held(&dir));
        });
        let report = x
            .result
            .as_ref()
            .expect("CONTROL: the quiet round completes");
        assert!(
            report.matching_inventory.is_some(),
            "CONTROL: a quiet round observes its matching inventory"
        );
        let lent = lent.borrow().clone();
        assert_eq!(lent.len(), x.hooks);
        assert_eq!(
            lent.iter().filter(|held| **held).count(),
            x.grouped.1 as usize,
            "CONTROL: the lock is held at exactly the lent admissions"
        );
        lent
    });
    for lent in &quiet {
        assert!(
            lent.iter().any(|held| *held) && lent.iter().any(|held| !*held),
            "CONTROL: the sweep meets lent admissions and first ones: {lent:?}"
        );
    }
    let runs: Vec<_> = sides
        .iter()
        .zip(&quiet)
        .flat_map(|(&side, lent)| {
            (1..=lent.len()).flat_map(move |at| [true, false].map(|grouped| (side, at, grouped)))
        })
        .collect();
    let outcomes = aterm_spec::fs_lane::each(&runs, |&(side, at, grouped)| {
        let (pair, mut repairs) = converged(side);
        let a = pair.lab.bootstrap(0);
        let b = pair.lab.bootstrap(1);
        let node = {
            let journal = Journal::open_existing(a.journal_dir(), a.trust()).unwrap();
            let admin = aterm_secrets::client::admin_open(&pair.lab.paths[0]).unwrap();
            admin
                .prepare_commons_retirement(
                    &journal,
                    vec![Grant::recorded(
                        "m1",
                        a.request().unwrap().binding().clone(),
                        Role::Writer,
                    )],
                    &[b.request().unwrap().binding().actor()],
                )
                .unwrap()
                .bytes()
                .to_vec()
        };
        let (dir, trust) = (a.journal_dir().to_owned(), a.trust());
        let ingested = Rc::new(Cell::new(false));
        let done = Rc::clone(&ingested);
        let [x, _] = probed_round(&pair, &mut repairs, side, grouped, move |k, _| {
            if k == at {
                Journal::open_existing(&dir, trust)
                    .unwrap()
                    .ingest(&node)
                    .unwrap();
                done.set(true);
            }
        });
        assert!(ingested.get(), "CONTROL: the retirement is ingested");
        outcome(&x)
    });
    let mut wrong = Vec::new();
    for (pair, (&(side, at, grouped), outcome)) in outcomes
        .chunks(2)
        .zip(runs.iter().zip(&outcomes).step_by(2))
    {
        assert!(grouped, "runs alternate grouped and group-less");
        let last = quiet[usize::from(side == Side::Responder)].len();
        let (result, hooks, _) = outcome;
        let expected = if at == last {
            matches!(result, Ok((_, false))) && *hooks == at + 1
        } else {
            result.is_err() && *hooks == at
        };
        if !expected {
            wrong.push(format!("{side:?} at {at}: {outcome:?}"));
        }
        if pair[0] != pair[1] {
            wrong.push(format!(
                "{side:?} at {at}: grouped {:?}, group-less {:?}",
                pair[0], pair[1]
            ));
        }
    }
    assert!(wrong.is_empty(), "what went wrong: {wrong:#?}");
}

/// LAW G-L4: a follower whose look no longer stands after its control journal read admits on a
/// look of its own, as its group's first (`Replica::follow`), and a look whose glance first saw the
/// control anchor unsettled starts no follower (`GroupLook::keep`). Over a converged journal pair
/// ([`converged`]), X initiating, at X's first round admission (the round's first after its opening,
/// lent the opening's look, as the control finds) a byte-identical copy of X's control anchor is
/// renamed over it, or a new file over X's state lock (`.lock`, no writer under it). The follower
/// finds the anchor moved (`Bootstrap::anchor_stands_in`) or the lock it holds no longer at the
/// vault's path (`VaultLook::guards_source`), reopens (reopened 1), and admits as a first (own one
/// more, lent one fewer than a quiet round's); the round completes and reports as a quiet round
/// does. In a fourth row X's control anchor is given, before the round and with its bytes as they
/// are, a modification time two of [`SETTLED`] ahead, so every glance of the round (the round
/// taking less than one of [`SETTLED`]) first sees it unsettled: no group keeps its look, every
/// read admission is a first (own the quiet round's own and lent, lent 0, reopened 0, and X's first
/// round admission finds the lock free), and the round reports as a quiet round does.
/// NEGATIVE CONTROLS (2026-11-12): the follower not asking `anchor_stands_in`, and not asking
/// `guards_source`, each leave reopened 0 at its row (`the anchor row's (own, lent, reopened)`,
/// `the lock row's`: left `(12, 6, 0)`, right `(13, 5, 1)`); `GroupLook::keep` keeping every look,
/// settled anchor or not, has each follower of the fourth row reopen (`the unsettled row's`: left
/// `(18, 0, 6)`, right `(18, 0, 0)`).
#[test]
fn a_follower_whose_control_anchor_or_lock_moved_admits_on_its_own_look() {
    use std::{cell::Cell, rc::Rc};
    #[derive(Clone, Copy, Debug)]
    enum Moved {
        Nothing,
        Anchor,
        Lock,
        Unsettled,
    }
    let rows = [Moved::Nothing, Moved::Anchor, Moved::Lock, Moved::Unsettled];
    let seen = aterm_spec::fs_lane::each(&rows, |&moved| {
        let (pair, mut repairs) = converged(Side::Initiator);
        let bootstrap = pair.lab.bootstrap(0);
        let anchor = bootstrap.journal_dir().join("control.anchor");
        if matches!(moved, Moved::Unsettled) {
            // Its owner sets its times through a read-only open: its bytes stay as they are.
            std::fs::File::open(&anchor)
                .unwrap()
                .set_modified(std::time::SystemTime::now() + 2 * SETTLED)
                .unwrap();
        }
        let dir = pair.lab.paths[0].dir.clone();
        let lent = Rc::new(Cell::new(None));
        let held = Rc::clone(&lent);
        let [x, _] = probed_round(&pair, &mut repairs, Side::Initiator, true, move |k, _| {
            if k != 1 {
                return;
            }
            held.set(Some(lock_held(&dir)));
            match moved {
                Moved::Nothing | Moved::Unsettled => {}
                Moved::Anchor => {
                    let copy = anchor.with_extension("copy");
                    std::fs::copy(&anchor, &copy).unwrap();
                    std::fs::rename(&copy, &anchor).unwrap();
                }
                Moved::Lock => {
                    std::fs::copy(dir.join(".lock"), dir.join(".lock-new")).unwrap();
                    std::fs::rename(dir.join(".lock-new"), dir.join(".lock")).unwrap();
                }
            }
        });
        (x.grouped, outcome(&x), lent.get())
    });
    let [quiet, anchor, lock, unsettled] = <[_; 4]>::try_from(seen).unwrap();
    let ((own, lent, _), quiet_outcome, _) = &quiet;
    let (own, lent) = (*own, *lent);
    assert!(
        quiet_outcome.0.as_ref().is_ok_and(|(_, matched)| *matched),
        "CONTROL: a quiet round completes and observes its matching inventory: {quiet_outcome:?}"
    );
    for (row, (grouped, outcome, _), expected) in [
        ("anchor", &anchor, (own + 1, lent - 1, 1)),
        ("lock", &lock, (own + 1, lent - 1, 1)),
        ("unsettled", &unsettled, (own + lent, 0, 0)),
    ] {
        assert_eq!(*grouped, expected, "the {row} row's (own, lent, reopened)");
        assert_eq!(
            (&outcome.0, outcome.1),
            (&quiet_outcome.0, quiet_outcome.1),
            "the {row} row reports as a quiet round"
        );
    }
    assert_eq!(
        [quiet.2, anchor.2, lock.2, unsettled.2],
        [Some(true), Some(true), Some(true), Some(false)],
        "CONTROL: X's first round admission is lent its group's look, but where its anchor had not \
         settled"
    );
}

/// LAW G-L5 (BC-34b): a change made without any lock lands for a group at its next first
/// admission. Over a converged journal pair ([`converged`]), X initiating, X's default pin
/// (`commons/default.pin`) is rewritten with other bytes at X's third round admission's hook: the
/// history ask's id exchange authorize (`Round::keys`), lent the look of the history ask before
/// it, as the control finds. That follower judges its custody through the group's look, which saw
/// the pin as it stood, and passes; the round sends its key-history ids, and refuses at its next
/// admission (the keys' handoffs), its group's first, with the custody's refusal. It has written
/// what the same round writes with the pin rewritten at that next admission's hook, where it
/// refuses too. CONTROL: outside any group, the pin rewritten at the third admission refuses
/// there, before those ids are sent.
/// NEGATIVE CONTROL (2026-11-12): `GroupLook::let_go` a no-op, so every read admission after the
/// opening follows it: the round completes (`the pin rewritten at the third admission refuses`:
/// `(Ok(([0, 0, 0, 0, 0, 0, 0], true)), 17, 199)`).
#[test]
fn a_change_without_the_lock_is_refused_by_the_next_groups_first_admission() {
    use std::{cell::Cell, rc::Rc};
    const CUSTODY: &str = "Commons immutable custody changed while acquiring a lock";
    let runs = [(3, true), (4, true), (3, false)];
    let seen = aterm_spec::fs_lane::each(&runs, |&(at, grouped)| {
        let (pair, mut repairs) = converged(Side::Initiator);
        let dir = pair.lab.paths[0].dir.clone();
        let pin = dir.join("commons/default.pin");
        let lent = Rc::new(Cell::new(None));
        let held = Rc::clone(&lent);
        let [x, _] = probed_round(
            &pair,
            &mut repairs,
            Side::Initiator,
            grouped,
            move |k, _| {
                if k == at {
                    held.set(Some(lock_held(&dir)));
                    let mut bytes = std::fs::read(&pin).unwrap();
                    bytes[0] ^= 1;
                    std::fs::write(&pin, bytes).unwrap();
                }
            },
        );
        (lent.get(), outcome(&x))
    });
    let [
        (third_lent, third),
        (fourth_lent, fourth),
        (alone_lent, alone),
    ] = <[_; 3]>::try_from(seen).unwrap();
    for (row, outcome) in [("third", &third), ("fourth", &fourth)] {
        assert!(
            outcome
                .0
                .as_ref()
                .is_err_and(|error| error.contains(CUSTODY)),
            "the pin rewritten at the {row} admission refuses with the custody's refusal: \
             {outcome:?}"
        );
        assert_eq!(
            outcome.1, 4,
            "the pin rewritten at the {row} refuses at the fourth"
        );
    }
    assert_eq!(
        third.2, fourth.2,
        "the round has sent its ids by the fourth admission"
    );
    assert_eq!(
        (third_lent, fourth_lent, alone_lent),
        (Some(true), Some(false), Some(false)),
        "CONTROL: the third admission is lent its group's look, the fourth is a group's first"
    );
    assert!(
        alone.0.as_ref().is_err_and(|error| error.contains(CUSTODY)) && alone.1 == 3,
        "CONTROL: outside any group the third admission refuses: {alone:?}"
    );
    assert!(
        alone.2 < third.2,
        "CONTROL: outside any group the ids are not sent: {alone:?} {third:?}"
    );
}

/// LAW G-L7: a witness read, and the admission after it, each admit on a look of their own
/// (`Round::table_witnesses` ends the round's group before each witness read and after it,
/// `GroupLook`): the peer and the table count set how many witness reads a round makes, so no
/// group spans one. Over a fresh journal pair (`Pair::new(false, 1)`, one table both list), its
/// first round, the first Mac (X) initiating and responding, asks the responder's witnesses, which
/// the responder answers, and moves the pair's record, each side's files read as settled
/// ([`SETTLED`]). At each of X's round admissions the hook records whether a table is active
/// (`Round::table_witnesses` makes one active for its read, before the table step) and whether
/// X's state lock is held ([`lock_held`]). X's witness read (its first admission with a table
/// active) and the admission after it (the authorize before the initiator's table choice, or
/// before the responder's ID list) find the lock free; the lock is held at exactly X's lent
/// admissions; and each side's (own, lent, reopened) is as observed, the initiator's (12, 6, 0)
/// and the responder's (17, 2, 0), whichever Mac initiates.
/// CONTROLS: the record moves once; the initiator's report says it asked the witnesses; X's round
/// lends some admissions.
/// NEGATIVE CONTROLS (2026-11-12), the `self.end_group()` calls of `Round::table_witnesses`
/// removed: both, `Initiator: X's admission 9 (the one after them) found its state lock held`,
/// `Responder: X's admission 9 (a witness read) found ..`, `Responder: X's admission 10 (the one
/// after them) found ..`, the initiator's and the responder's counts (11, 7, 0) and (15, 4, 0);
/// only the one before the read, `Responder: X's admission 9 (a witness read) found ..`, and
/// (12, 6, 0) and (16, 3, 0); only the one after it, `Initiator: X's admission 9 (the one after
/// them) found ..`, `Responder: X's admission 10 (the one after them) found ..`, and (11, 7, 0)
/// and (16, 3, 0).
#[test]
fn a_witness_read_and_the_admission_after_it_admit_on_looks_of_their_own() {
    use std::rc::Rc;
    let sides = [Side::Initiator, Side::Responder];
    let seen = aterm_spec::fs_lane::each(&sides, |&side| {
        let pair = Pair::new(false, 1);
        let mut repairs = [Repair::default(), Repair::default()];
        let dir = pair.lab.paths[0].dir.clone();
        let hooks = Rc::new(RefCell::new(Vec::new()));
        let at = Rc::clone(&hooks);
        let [x, peer] = probed_round(&pair, &mut repairs, side, true, move |_, active| {
            at.borrow_mut().push((active.is_some(), lock_held(&dir)));
        });
        let reports = [&x.result, &peer.result]
            .map(|result| result.as_ref().expect("CONTROL: the round completes"));
        let moved: usize = reports
            .iter()
            .map(|report| report.sent + report.received)
            .sum();
        let asked = reports[usize::from(side == Side::Responder)].witnesses_asked;
        let hooks = hooks.borrow().clone();
        assert_eq!(hooks.len(), x.hooks);
        (moved, asked, [x.grouped, peer.grouped], hooks)
    });
    let observed = [[(12, 6, 0), (17, 2, 0)], [(17, 2, 0), (12, 6, 0)]];
    let mut wrong = Vec::new();
    for ((side, (moved, asked, grouped, hooks)), observed) in sides.iter().zip(&seen).zip(observed)
    {
        assert_eq!(*moved, 2, "CONTROL: {side:?}: the record moves once");
        assert!(
            *asked,
            "CONTROL: {side:?}: the initiator asks the witnesses"
        );
        let lent = grouped[0].1 as usize;
        assert!(
            lent > 0,
            "CONTROL: {side:?}: X's round lends some admissions"
        );
        let witness = hooks
            .iter()
            .position(|(active, _)| *active)
            .expect("CONTROL: X reads a witness");
        let after = witness
            + hooks[witness..]
                .iter()
                .take_while(|(active, _)| *active)
                .count();
        assert!(
            hooks.get(after).is_some_and(|(active, _)| !*active),
            "CONTROL: {side:?}: an admission with no table active follows X's witness reads"
        );
        for (at, (_, held)) in hooks.iter().enumerate().take(after + 1).skip(witness) {
            if *held {
                wrong.push(format!(
                    "{side:?}: X's admission {} ({}) found its state lock held",
                    at + 1,
                    if at < after {
                        "a witness read"
                    } else {
                        "the one after them"
                    }
                ));
            }
        }
        let held = hooks.iter().filter(|(_, held)| *held).count();
        if held != lent {
            wrong.push(format!(
                "{side:?}: the lock held at {held} admissions, X lent {lent}"
            ));
        }
        if *grouped != observed {
            wrong.push(format!(
                "{side:?}: (own, lent, reopened) of X and its peer {grouped:?}, observed \
                 {observed:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "what went wrong: {wrong:#?}");
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
            |session| round.prepare_operations(session, id, &held),
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

/// A policy change before a batch's session opens refuses the batch, through `Round::local` and,
/// with a table active, through `Round::local_policy_only`, which checks the policy alone
/// (`Gate::Policy`). CONTROL: with the policy restored, the policy-only batch runs, so its
/// refusal was the policy's.
/// NEGATIVE CONTROL: `Gate::Policy` skipping the policy's check as well admits the policy-only
/// batch: `` late disable must refuse the local batch ``. Neither
/// `outbound_policy_is_rechecked_after_preparing_payload` nor
/// `policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected` sees that mutant,
/// since a send's boundary and a receive's write admission check the policy again.
#[test]
fn policy_changes_before_session_open_refuse_the_old_round() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let config = lab.paths[0].home.join("config.toml");
    let replica = Replica::commons(lab.paths[0].clone(), config.clone()).unwrap();
    let peer = lab.bootstrap(1).request().unwrap().binding().actor();
    let round = replica.round(peer).unwrap();
    for policy_only in [false, true] {
        if policy_only {
            round.active_table.set(Some([7; 32]));
        }
        let disable = config.clone();
        let change = Change::install(0, move || {
            aterm_secrets::fsx::write_atomic(&disable, b"[commons]\nenabled=false\n", 0o600)
                .unwrap();
        });
        let called = std::cell::Cell::new(false);
        let run = |_: &Session<'_>| {
            called.set(true);
            Ok(())
        };
        let result = if policy_only {
            round.local_policy_only(peer, run)
        } else {
            round.local(peer, run)
        };
        assert!(result.is_err(), "late disable must refuse the local batch");
        assert!(!called.get(), "a refused batch must never run");
        drop(change);
        aterm_secrets::fsx::write_atomic(&config, b"[commons]\n", 0o600).unwrap();
    }
    assert!(
        round.local_policy_only(peer, |_| Ok(())).is_ok(),
        "CONTROL: the restored policy admits the policy-only batch"
    );
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
        self.round_in(replica, None)
    }
    /// [`Self::round`] through `group` where given (`Replica::round_in`).
    fn round_in<'a>(
        &self,
        replica: &'a Replica,
        group: Option<&'a GroupLook<'a>>,
    ) -> Result<Round<'a>> {
        let mut round = replica.round_in(self.peer, group)?;
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

/// What a withdrawal sweep commits into the first Mac's catalog at its chosen admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Commit {
    /// A payload that declares nothing (`Snapshot::inert`): the table stays subscribed.
    Inert,
    /// The table's next version, which conflicts with the one declared: the table is withdrawn.
    Incompatible,
}

/// Commit `commit` into the catalog of the Mac at `paths`, under its writer.
fn commit_into_catalog(paths: &Paths, directory: &crate::catalog::Directory, commit: Commit) {
    let bootstrap = Bootstrap::cached(paths).unwrap().unwrap();
    let session = Session::open(&bootstrap).unwrap();
    let catalog = directory.catalog_scope();
    let mut store = Store::open(&session, &directory.path(catalog).unwrap(), catalog).unwrap();
    let payload = match commit {
        Commit::Inert => b"inert catalog payload".to_vec(),
        Commit::Incompatible => {
            crate::catalog::Descriptor::application(bootstrap.trust(), "accounts", "proposals", 2)
                .unwrap()
                .encode()
                .unwrap()
        }
    };
    store.commit_catalog(&session, &payload).unwrap();
}

/// The operation inventory of `scope`'s store on the Mac at `paths`.
fn table_inventory(
    paths: &Paths,
    directory: &crate::catalog::Directory,
    scope: Scope,
) -> BTreeSet<Digest> {
    let bootstrap = Bootstrap::cached(paths).unwrap().unwrap();
    let session = Session::observe(&bootstrap).unwrap();
    let store = directory.observe_store(&session, scope).unwrap();
    store.inventory(&session).unwrap().copied().collect()
}

/// One side's end of a stream, counting the bytes this side writes.
struct Counted<S> {
    inner: S,
    written: std::rc::Rc<std::cell::Cell<usize>>,
}
impl<S: Read> Read for Counted<S> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.inner.read(bytes)
    }
}
impl<S: Write> Write for Counted<S> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.written.set(self.written.get() + written);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// What one [`Fixture::table_step`] saw of the first Mac (X).
struct Step {
    /// The active table of each of X's round admissions, in order.
    admissions: Vec<Option<Digest>>,
    /// How many of them X's table step made, before its matching inventory's read.
    table_admissions: usize,
    /// X's table step's report, or its error.
    result: std::result::Result<Report, String>,
    /// X's matching inventory after a table step that completed, where asked.
    matching: Option<MatchingInventory>,
    /// The bytes X had written and its table's inventory when the change committed.
    at_change: Option<(usize, BTreeSet<Digest>)>,
    /// The bytes X wrote in all.
    written: usize,
    /// X's table's inventory at the end.
    inventory: BTreeSet<Digest>,
    /// How many of X's admissions were lent its round group's look (`GroupLook`).
    lent: u32,
}

impl Fixture {
    /// The second Mac's catalog directory and config.
    fn other(&self) -> (crate::catalog::Directory, PathBuf) {
        (
            crate::catalog::Directory::new(&self.lab.paths[1], self.lab.bootstrap(1).trust()),
            self.lab.paths[1].home.join("config.toml"),
        )
    }

    /// One table step of this fixture's table between the first Mac (X, on this thread, as
    /// `side`) and the second (on a thread of its own), driven directly ([`Round::table`]) over a
    /// socket pair, so the step repairs this table whatever else either Mac holds. X's round
    /// admits through a round group on that stream (`GroupLook`, `Wire`), as a round on a stream
    /// does, its files read as settled ([`SETTLED`]) so its groups lend their looks. Where `change`
    /// is given, `commit` goes into X's catalog at X's `at`-th admission (counting from 1), before
    /// that admission's session opens. With `matching`, X then reads its matching inventory
    /// ([`Round::matching_inventory`]), whose admissions count on from the table step's.
    fn table_step(&self, side: Side, change: Option<(usize, Commit)>, matching: bool) -> Step {
        use std::{cell::Cell, rc::Rc};
        let id = self.scope.digest();
        let x = Replica::commons(self.lab.paths[0].clone(), self.config.clone()).unwrap();
        let x_actor = x.actor;
        let (directory, config) = self.other();
        let y = Replica::commons(self.lab.paths[1].clone(), config).unwrap();
        let y_path = directory.path(self.scope).unwrap();
        let (left, mut right) = std::os::unix::net::UnixStream::pair().unwrap();
        for stream in [&left, &right] {
            // Hang detectors: every read and write of a step that runs is answered at once.
            stream
                .set_read_timeout(Some(Duration::from_secs(60)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(60)))
                .unwrap();
        }
        let written = Rc::new(Cell::new(0));
        let admissions = Rc::new(std::cell::RefCell::new(Vec::new()));
        let at_change = Rc::new(std::cell::RefCell::new(None));
        let hook = {
            let (written, admissions, at_change) =
                (written.clone(), admissions.clone(), at_change.clone());
            let (paths, directory, scope) = (
                self.lab.paths[0].clone(),
                self.directory.clone(),
                self.scope,
            );
            move |active: Option<Digest>| {
                admissions.borrow_mut().push(active);
                let at = admissions.borrow().len();
                if let Some((_, commit)) = change.filter(|(chosen, _)| *chosen == at) {
                    commit_into_catalog(&paths, &directory, commit);
                    *at_change.borrow_mut() =
                        Some((written.get(), table_inventory(&paths, &directory, scope)));
                }
            }
        };
        let (result, table_admissions, matching, lent) = thread::scope(|threads| {
            let other = threads.spawn(move || {
                let mut round = y.round(x_actor).unwrap();
                round.tables.insert(id, (self.scope, y_path));
                let mut report = Report {
                    table: Some(id),
                    ..Report::default()
                };
                let side = match side {
                    Side::Initiator => Side::Responder,
                    Side::Responder => Side::Initiator,
                };
                let went = round.table(
                    &mut right,
                    x_actor,
                    side,
                    id,
                    &mut Repair::default(),
                    &mut report,
                );
                drop(right);
                went.map_err(|error| error.to_string())
            });
            let (went, report, table_admissions, matching, lent) =
                aterm_secrets::client::test_as_if_later(SETTLED, || {
                    let group = GroupLook::new(&x.paths);
                    let round = self.round_in(&x, Some(&group)).unwrap();
                    let _hook = Admissions::install(hook);
                    let mut stream = Counted {
                        inner: left,
                        written: written.clone(),
                    };
                    let mut report = Report {
                        table: Some(id),
                        ..Report::default()
                    };
                    let before = super::grouped().1;
                    let went = round.table(
                        &mut Wire {
                            stream: &mut stream,
                            group: &group,
                        },
                        self.peer,
                        side,
                        id,
                        &mut Repair::default(),
                        &mut report,
                    );
                    // The other side reads to its end, or to this end's close where this side
                    // stops.
                    drop(stream);
                    let table_admissions = admissions.borrow().len();
                    let matching = (matching && went.is_ok())
                        .then(|| round.matching_inventory(self.peer, id, &report))
                        .flatten();
                    (
                        went,
                        report,
                        table_admissions,
                        matching,
                        super::grouped().1 - before,
                    )
                });
            let _ = other.join().unwrap();
            (
                went.map(|()| report).map_err(|error| error.to_string()),
                table_admissions,
                matching,
                lent,
            )
        });
        let admissions = admissions.borrow().clone();
        let at_change = at_change.borrow_mut().take();
        Step {
            admissions,
            table_admissions,
            result,
            matching,
            at_change,
            written: written.get(),
            inventory: table_inventory(&self.lab.paths[0], &self.directory, self.scope),
            lent,
        }
    }
}

/// A table withdrawn from this Mac's subscriptions while it repairs the table loses nothing more
/// to the peer from the moment this Mac can see it. The table step is driven for the application
/// table on both Macs (`Fixture::table_step`), each holding a record the other lacks, so a step
/// sends and ingests one. A control run counts the first Mac's admissions (11 as initiator and 11
/// as responder, every one the application table's); then, on a fresh fixture each, an
/// incompatible declaration is committed into the first Mac's catalog at each of those
/// admissions in turn, before its session opens: no byte the first Mac writes and no record it
/// ingests follows, and its step fails with the subscription's refusal (never the observation
/// pin's). CONTROL: an inert catalog payload committed at the same admission leaves the step
/// whole, one record sent and one ingested, so the commit itself (a writer session on the first
/// Mac) refuses nothing. Code that checked the subscription at every admission passes too: this
/// is the guarantee `Gate` keeps.
/// X's round admits through a round group on its stream (`GroupLook`), its files read as settled,
/// so some of its admissions are lent the group's look (a CONTROL counts them) and some changes
/// commit at a lent admission's hook, under the group's hold: a follower checks the subscription
/// as a first does (`Directory::permits`, reading the catalog under its session).
/// NEGATIVE CONTROL (2026-11-12): a follower checking only the policy at a `Gate::Subscription`
/// admission (`Round::admit` taking `Gate::Policy` while the group holds a look) lets bytes leave
/// after the change, as initiator at admissions 1 to 4 (`5 bytes written after the change`) and 6
/// to 8 (`37 bytes written after the change`).
/// NEGATIVE CONTROLS: the send boundary's check in `Round::send_batch` as `Gate::Policy` sends
/// after the change (`` 4 bytes written after the change `` for an empty conflict batch,
/// `` 395 bytes written after the change `` for the record's); the operation inventory's
/// authorize in `Round::table` through `Round::local_policy_only` sends the inventory
/// (`` 37 bytes written after the change ``); `Round::local_write` admitting with `Gate::Policy`
/// ingests the record (`` 1 records ingested after the change ``).
/// GAP: one primary application table, both inventories differing (so no matching inventory is
/// read: `a_withdrawal_at_the_matching_inventory_read_observes_no_match` sweeps that read), no
/// conflict proofs and stored on both sides. Recovery directories (`retain_primary`'s write, a
/// recovery store's choice), witness rounds (`table_witnesses`' admissions), ring tables and
/// a first enrollment are not swept.
#[test]
fn no_table_byte_leaves_after_a_visible_withdrawal() {
    const CHANGE: &str = "table subscription changed during repair";
    let sides = [Side::Initiator, Side::Responder];
    let counts = aterm_spec::fs_lane::each(&sides, |&side| {
        let fixture = Fixture::new();
        let step = fixture.table_step(side, None, false);
        assert!(
            step.lent > 0,
            "CONTROL: some of the step's admissions are lent its group's look"
        );
        let report = step.result.expect("CONTROL: the step completes");
        assert_eq!(
            (report.sent, report.received),
            (1, 1),
            "CONTROL: one record each way"
        );
        assert!(
            step.admissions
                .iter()
                .all(|active| *active == Some(fixture.scope.digest())),
            "CONTROL: each admission is the application table's: {:?}",
            step.admissions
        );
        step.admissions.len()
    });
    assert_eq!(
        counts,
        [11, 11],
        "CONTROL: the table step's admissions, as initiator and as responder"
    );
    let runs: Vec<_> = sides
        .iter()
        .zip(&counts)
        .flat_map(|(&side, &count)| {
            (1..=count).flat_map(move |at| {
                [Commit::Inert, Commit::Incompatible].map(|commit| (side, at, commit))
            })
        })
        .collect();
    let verdicts = aterm_spec::fs_lane::each(&runs, |&(side, at, commit)| {
        let fixture = Fixture::new();
        let step = fixture.table_step(side, Some((at, commit)), false);
        let id = Some(fixture.scope.digest());
        let mut wrong = Vec::new();
        if !step.admissions.iter().all(|active| *active == id) {
            wrong.push(format!("another table's admission: {:?}", step.admissions));
        }
        let Some((written, inventory)) = step.at_change else {
            wrong.push("the change never committed".to_owned());
            return wrong;
        };
        match commit {
            Commit::Inert => match step.result {
                Ok(report) if (report.sent, report.received) == (1, 1) => {}
                Ok(report) => wrong.push(format!(
                    "the step moved {} and {}, not one record each way",
                    report.sent, report.received
                )),
                Err(error) => wrong.push(format!("an inert payload refused the step: {error}")),
            },
            Commit::Incompatible => {
                match step.result {
                    Err(error) if error.contains(CHANGE) => {}
                    Err(error) => wrong.push(format!("refused otherwise: {error}")),
                    Ok(report) => wrong.push(format!("the step completed: {report:?}")),
                }
                if step.written != written {
                    wrong.push(format!(
                        "{} bytes written after the change",
                        step.written - written
                    ));
                }
                if step.inventory != inventory {
                    wrong.push(format!(
                        "{} records ingested after the change",
                        step.inventory.difference(&inventory).count()
                    ));
                }
            }
        }
        wrong
    });
    let wrong: Vec<_> = runs
        .iter()
        .zip(verdicts)
        .filter(|(_, wrong)| !wrong.is_empty())
        .collect();
    assert!(
        wrong.is_empty(),
        "(side, admission, commit) and what went wrong: {wrong:#?}"
    );
}

/// A withdrawal the matching inventory's read can see publishes no match: on a quiet table step
/// (both Macs holding both records), an incompatible declaration committed into the first Mac's
/// catalog at its matching inventory's first admission (`Round::fenced_observation`'s) leaves it
/// none, though the inventories match; that read, and its fallback (`Round::inventory`), check
/// the table's subscription (`Gate::Subscription`). CONTROL: the quiet step with nothing
/// committed, and with an inert payload committed there, observes its matching inventory on the
/// fence, in one admission.
/// NEGATIVE CONTROLS: `Round::fenced_observation` reading through `Round::local_policy_only`
/// publishes the match, and so does its fallback, `Round::inventory`, reading with `Gate::Policy`:
/// `` assertion failed: step.matching.is_none() ``.
#[test]
fn a_withdrawal_at_the_matching_inventory_read_observes_no_match() {
    let fixture = Fixture::new();
    let first = fixture.table_step(Side::Initiator, None, false);
    let report = first.result.expect("CONTROL: the first step completes");
    assert_eq!(
        (report.sent, report.received),
        (1, 1),
        "CONTROL: one record each way"
    );
    let quiet = fixture.table_step(Side::Initiator, None, true);
    let at = quiet.table_admissions + 1;
    assert!(
        quiet.matching.is_some(),
        "CONTROL: a quiet step observes its matching inventory"
    );
    assert_eq!(
        quiet.admissions.len(),
        at,
        "CONTROL: the matching inventory is observed on its fence, in one admission"
    );
    let inert = fixture.table_step(Side::Initiator, Some((at, Commit::Inert)), true);
    assert!(
        inert.at_change.is_some(),
        "CONTROL: the inert payload commits"
    );
    assert!(
        inert.matching.is_some(),
        "CONTROL: an inert payload at the matching inventory's read leaves its match"
    );
    let step = fixture.table_step(Side::Initiator, Some((at, Commit::Incompatible)), true);
    assert!(step.at_change.is_some(), "CONTROL: the change commits");
    assert!(
        step.result.is_ok(),
        "CONTROL: the change came after the table step"
    );
    assert!(step.matching.is_none());
}

/// When [`a_send_whose_preparation_fails_on_a_withdrawn_table_meets_the_withdrawal`] withdraws
/// its table, by an incompatible declaration committed into the first Mac's catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Withdrawn {
    /// Never: the table stays subscribed.
    Never,
    /// Before the batch's admission.
    BeforeAdmission,
    /// Inside the preparation, after the batch's admission checked the policy.
    DuringPreparation,
}

/// A send whose preparation fails while its table is withdrawn meets the withdrawal's refusal:
/// the batch's admission checks the policy alone (`Gate::Policy`), and `Round::send_batch` checks
/// the subscription before it returns a preparation's error. Withdrawn before the batch's
/// admission, the refusal is the one an admission that checked the subscription before preparing
/// gave. Withdrawn during the preparation, it is new behaviour: that admission had checked a
/// table still subscribed and let the preparation's own error through. With the send's admission
/// through `Round::local` and the preparation's error returned before the boundary's check, as
/// that code did, the withdrawal before the admission is refused and the one during the
/// preparation reports `` table record disappeared ``. CONTROL: a preparation that fails on a
/// table still subscribed returns its own error. None writes a byte. Each case runs on a fresh
/// fixture.
/// NEGATIVE CONTROL: returning the preparation's error first, before the subscription's check,
/// reports `` table record disappeared `` for both withdrawn tables.
#[test]
fn a_send_whose_preparation_fails_on_a_withdrawn_table_meets_the_withdrawal() {
    let cases = [
        Withdrawn::Never,
        Withdrawn::BeforeAdmission,
        Withdrawn::DuringPreparation,
    ];
    let refusals = aterm_spec::fs_lane::each(&cases, |&withdrawn| {
        let fixture = Fixture::new();
        let replica =
            Replica::commons(fixture.lab.paths[0].clone(), fixture.config.clone()).unwrap();
        let round = fixture.round(&replica).unwrap();
        round.active_table.set(Some(fixture.scope.digest()));
        let withdraw = || {
            commit_into_catalog(
                &fixture.lab.paths[0],
                &fixture.directory,
                Commit::Incompatible,
            );
        };
        if withdrawn == Withdrawn::BeforeAdmission {
            withdraw();
        }
        let mut stream = Stream::default();
        let mut report = Report::default();
        let refused = round
            .send_batch(
                &mut stream,
                fixture.peer,
                |_| {
                    if withdrawn == Withdrawn::DuringPreparation {
                        withdraw();
                    }
                    Err(Error::new("table record disappeared"))
                },
                BatchKind::Operation,
                &mut report,
            )
            .expect_err("a failed preparation sends nothing");
        assert!(stream.output.is_empty(), "a refusal writes nothing");
        refused.to_string()
    });
    assert_eq!(
        refusals,
        [
            "table record disappeared",
            "table subscription changed during repair",
            "table subscription changed during repair"
        ]
    );
}

/// A table withdrawn while a batch is prepared is refused at the batch's send boundary, after a
/// preparation that succeeds: the preparation commits an incompatible declaration into the first
/// Mac's catalog and then returns the batch, and `Round::send_batch`, whose admission checked the
/// policy alone (`Gate::Policy`), refuses with the subscription's refusal and writes nothing to
/// the stream. Code whose admission checked the subscription before preparing passes too: its
/// boundary checked it again after. CONTROL: an inert catalog payload committed the same way
/// sends the batch (one record, its receipt read), so the commit itself refuses nothing. Each
/// case runs on a fresh fixture.
/// NEGATIVE CONTROL: the send's admission through `Round::local`, which checks the subscription
/// before preparing, with the boundary's check as `Gate::Policy`, sends the batch:
/// `` (Ok(()), "the batch", 1) `` for the withdrawn table.
#[test]
fn a_withdrawal_while_a_batch_is_prepared_is_refused_at_its_send_boundary() {
    let commits = [Commit::Inert, Commit::Incompatible];
    let sends = aterm_spec::fs_lane::each(&commits, |&commit| {
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
        let mut batch = 1u32.to_le_bytes().to_vec();
        control::send_frame(&mut batch, &fixture.original).unwrap();
        let committed = std::cell::Cell::new(false);
        let sent = round
            .send_batch(
                &mut stream,
                fixture.peer,
                |_| {
                    if !committed.replace(true) {
                        commit_into_catalog(&fixture.lab.paths[0], &fixture.directory, commit);
                    }
                    Ok(vec![fixture.original.clone()])
                },
                BatchKind::Operation,
                &mut report,
            )
            .map_err(|error| error.to_string());
        assert!(committed.get(), "CONTROL: the preparation ran");
        let written = if stream.output.is_empty() {
            "nothing"
        } else if stream.output == batch {
            "the batch"
        } else {
            "other bytes"
        };
        (sent, written, report.sent)
    });
    assert_eq!(
        sends,
        [
            (Ok(()), "the batch", 1),
            (
                Err("table subscription changed during repair".to_owned()),
                "nothing",
                0
            ),
        ]
    );
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
/// entry to `Round::admit` and which boundary carries the fault: a send meets its admission's
/// policy check (`Gate::Policy`) and, after preparing, the policy and subscription check at its
/// boundary, and a receive meets its write admission's check of both; each reaches the same
/// `check_table_policy`. `outbound_policy_is_rechecked_after_preparing_payload` holds the
/// send-only recheck. GAP: a defect specific to one policy, one direction and one boundary
/// together is not seen for the other 16 selections.
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

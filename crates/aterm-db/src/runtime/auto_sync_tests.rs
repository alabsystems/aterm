// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Automatic owners on isolated native loopback multicast. Generated vaults and
//! admitted devices exercise startup, discovery and repair without live lab data.

use super::*;
use crate::{accounts::proposals::Proposals, database::Change};
use astream_wire::sync::Digest;
use aterm_net::lan::{Multicast, Scope as LinkScope};
use aterm_secrets::commons::roster::Role;
use aterm_types::network_state::NetworkObservation;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

/// What a runtime's transports have reported ([`Attachment::observe`]).
#[derive(Default)]
pub(super) struct Progress {
    /// The most operations each table's rounds saw retained. A wait asks for at least so many,
    /// and a rebuilt transport, which has seen no round yet, says nothing of what its store holds.
    pub(super) retained: BTreeMap<Digest, usize>,
    /// Application rounds that completed, on any table or none, whether or not each exchanged it.
    pub(super) rounds: u64,
    /// Whether a round of the runtime's, on any table or none, was in flight at its transport's
    /// last turn ([`super::transport::Transport::rounds_in_flight`]).
    pub(super) in_flight: bool,
}
impl Progress {
    pub(super) fn held(&self, table: &Digest) -> usize {
        self.retained.get(table).copied().unwrap_or(0)
    }

    /// What the runtime's rounds did, as a wait on processes this one started reads it
    /// ([`Minute::worked`]).
    pub(super) fn rounds(&self) -> Rounds {
        Rounds {
            ended: self.rounds,
            in_flight: self.in_flight,
        }
    }
}

/// What the runtimes doing a wait's repair did of their rounds ([`Minute::worked`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Rounds {
    /// How many rounds they ended, counted from any start.
    pub(super) ended: u64,
    /// Whether one of their rounds is in flight.
    pub(super) in_flight: bool,
}

/// Why a wait ended without what it waited for ([`Minute::remaining`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unmet {
    /// A hang detector's minute passed with no progress ([`Minute`]): `quiet` is how long it has
    /// been since the last, or since the wait began.
    Hung { quiet: Duration },
    /// Progress kept the wait open for [`aterm_spec::wait::WORKING_GUARD`] without what it waited
    /// for: the processes doing the repair work on and never bring it.
    Guard,
}

/// A hang detector's minute ([`aterm_spec::wait::MUST_HAPPEN`]) for a wait whose repair other
/// runtimes do: progress starts it again, and a wait that progress keeps open ends at
/// [`aterm_spec::wait::WORKING_GUARD`]. What counts as progress is the waiter's: [`Watch`] counts
/// its table's retained operations, and where processes this one started do the repair, a wait
/// counts the work they do on it ([`Self::worked`]).
pub(super) struct Minute {
    began: Instant,
    progressed_at: Instant,
    /// What the processes doing the repair had done at the last note ([`Self::worked`]): how many
    /// rounds they had ended, and their work count.
    noted: Option<(u64, u64)>,
}
impl Minute {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            began: now,
            progressed_at: now,
            noted: None,
        }
    }

    /// Progress at `now`: the minute starts again.
    pub(super) fn progressed(&mut self, now: Instant) {
        self.progressed_at = now;
    }

    /// Note what the processes this one started did, where they do the repair: their `rounds`,
    /// and `used`, a count that rises whenever they used CPU or did I/O
    /// ([`aterm_uds::deadline::descendants_work`]). A round they ended since the last note is
    /// progress, and so is their work while one of their rounds is in flight: a loaded Mac
    /// stretches a round's wall time without bound while they work on it, so its minute is
    /// theirs. Their work between rounds is not: their timers, beacons and polls bring no record,
    /// so a pair that rests until its audit ends the wait a minute after its last round, and a
    /// write that waits for an audit is named at a minute, not at the guard.
    pub(super) fn worked(&mut self, rounds: Rounds, used: u64, now: Instant) {
        if let Some((ended, was)) = self.noted.replace((rounds.ended, used))
            && (rounds.ended > ended || (rounds.in_flight && used != was))
        {
            self.progressed_at = now;
        }
    }

    /// How long a wait may sleep before this must be asked again, or why it ended unmet.
    pub(super) fn remaining(&self, now: Instant) -> std::result::Result<Duration, Unmet> {
        if now.saturating_duration_since(self.began) >= aterm_spec::wait::WORKING_GUARD {
            return Err(Unmet::Guard);
        }
        let quiet = now.saturating_duration_since(self.progressed_at);
        aterm_spec::wait::MUST_HAPPEN
            .checked_sub(quiet)
            .filter(|left| !left.is_zero())
            .ok_or(Unmet::Hung { quiet })
    }

    /// How long the wait has run.
    pub(super) fn ran(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.began)
    }
}

/// A minute without progress on the requested table is a hang ([`Minute`]). Progress is a higher
/// retained count on that table and, where processes this one started do the repair, the work
/// they do on it ([`Self::worked`]). For runtimes in this process alone, unrelated or deferred
/// rounds neither exhaust nor renew that table's wait, and no count of rounds judges it.
pub(super) struct Watch {
    table: Digest,
    rounds_at_start: u64,
    high_water: usize,
    minute: Minute,
}
impl Watch {
    pub(super) fn new(table: Digest, progress: &Progress, now: Instant) -> Self {
        Self {
            table,
            rounds_at_start: progress.rounds,
            high_water: progress.held(&table),
            minute: Minute::new(now),
        }
    }

    /// Note what the processes this one started did of their `rounds`, and their work count
    /// `used`, where they do the repair ([`Minute::worked`]).
    pub(super) fn worked(&mut self, rounds: Rounds, used: u64, now: Instant) {
        self.minute.worked(rounds, used, now);
    }

    /// How long a wait may sleep before this must be asked again, or why it ended unmet.
    pub(super) fn remaining(
        &mut self,
        progress: &Progress,
        now: Instant,
    ) -> std::result::Result<Duration, Unmet> {
        if progress.held(&self.table) > self.high_water {
            self.high_water = progress.held(&self.table);
            self.minute.progressed(now);
        }
        self.minute.remaining(now)
    }

    /// How many application rounds completed since the wait began, and how long it has run.
    pub(super) fn ran(&self, progress: &Progress, now: Instant) -> (u64, Duration) {
        (
            progress.rounds.saturating_sub(self.rounds_at_start),
            self.minute.ran(now),
        )
    }
}

/// What a wait on processes this one started reads beside what it waits for: the clock, their
/// work ([`Minute::worked`]), and how long it pauses between two looks. A live wait reads
/// [`Instant::now`] and [`aterm_uds::deadline::descendants_work`]; a test of the wait passes its
/// own, with the clock in its hands.
pub(super) struct Seams<'a> {
    pub(super) now: &'a mut dyn FnMut() -> Instant,
    pub(super) used: &'a mut dyn FnMut() -> u64,
    pub(super) pause: Duration,
}

/// How often a wait whose repair runs in processes this one started reads their progress: a read
/// of each process in this one's tree (a whole-table sweep off macOS), so not once a report.
const WORK_TICK: Duration = Duration::from_millis(500);

/// What a wait for one table's operations saw ([`Attachment::observe_until`]).
#[derive(Debug)]
struct Observed {
    /// What the runtime's rounds saw retained, by table.
    retained: BTreeMap<Digest, usize>,
    /// Why the wait ended without the operations; none where they came.
    unmet: Option<Unmet>,
    /// The application rounds the runtime completed while the wait ran.
    rounds: u64,
    /// How long the wait ran.
    elapsed: Duration,
}
impl Observed {
    fn held(&self, table: &Digest) -> usize {
        self.retained.get(table).copied().unwrap_or(0)
    }

    /// Fail, naming the `phase`, why the wait ended, its rounds and its time, unless the runtime's
    /// rounds saw `operations` on `table`.
    #[track_caller]
    fn holds(&self, table: &Digest, operations: usize, phase: &str) {
        assert!(
            self.held(table) >= operations,
            "{phase}: automatic repair did not retain {operations} operations; observed {:?}; \
             the wait ended {:?} after {} rounds in {:?}",
            self.retained,
            self.unmet,
            self.rounds,
            self.elapsed,
        );
    }
}

/// Told whether a round of an attachment's runtime is in flight ([`Attachment::telling_flight`]).
type Flight = Arc<dyn Fn(bool) + Send + Sync>;

#[derive(Clone)]
pub(super) struct Attachment {
    pub(super) port: u16,
    /// Whether the isolated attachment delivers its links' sends ([`super::transport::Transport::isolated`]).
    pub(super) delivering: Arc<std::sync::atomic::AtomicBool>,
    progress: Arc<(Mutex<Progress>, Condvar)>,
    /// Told whether a round of its runtime's is in flight, each time that changes
    /// ([`Self::telling_flight`]).
    flight: Option<Flight>,
}
impl Attachment {
    pub(super) fn new(port: u16) -> Self {
        Self {
            port,
            delivering: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            progress: Arc::new((Mutex::new(Progress::default()), Condvar::new())),
            flight: None,
        }
    }

    /// This attachment, telling `tell` whether a round of its runtime's is in flight each time
    /// that changes: a child runtime's fixture logs it for the waits of the test that started it
    /// ([`Minute::worked`]).
    pub(super) fn telling_flight(mut self, tell: impl Fn(bool) + Send + Sync + 'static) -> Self {
        self.flight = Some(Arc::new(tell));
        self
    }

    /// Note what a transport saw retained, the `rounds` it finished since its last call, and
    /// whether a round of its is `in_flight`.
    pub(super) fn observe(&self, observed: &BTreeMap<Digest, usize>, rounds: u64, in_flight: bool) {
        let (state_mu, changed) = &*self.progress;
        let mut state = state_mu.lock().unwrap_or_else(|p| p.into_inner());
        let flew = std::mem::replace(&mut state.in_flight, in_flight) != in_flight;
        let mut moved = rounds != 0 || flew;
        state.rounds += rounds;
        for (table, retained) in observed {
            let held = state.retained.entry(*table).or_default();
            if retained > held {
                *held = *retained;
                moved = true;
            }
        }
        if moved {
            changed.notify_all();
        }
        drop(state);
        if flew && let Some(tell) = &self.flight {
            tell(in_flight);
        }
    }

    /// What this runtime's rounds did ([`Progress::rounds`]).
    pub(super) fn rounds(&self) -> Rounds {
        let (state_mu, _) = &*self.progress;
        state_mu.lock().unwrap_or_else(|p| p.into_inner()).rounds()
    }

    /// Wait until this runtime's rounds saw `operations` on `table`, the `phase` this is, where
    /// runtimes in this process alone do the repair ([`Watch`]). A wait that ends unmet fails,
    /// naming why, after how many rounds and how long, and what the rounds saw retained.
    #[track_caller]
    pub(super) fn wait_for(&self, table: Digest, operations: usize, phase: &str) {
        self.observe_until(table, operations, None)
            .holds(&table, operations, phase);
    }

    /// [`Self::wait_for`] where processes this one started take part in the repair, in rounds
    /// with this runtime: the wait's minute is theirs while they work on a round
    /// ([`Minute::worked`]).
    #[track_caller]
    pub(super) fn wait_for_while_children_work(
        &self,
        table: Digest,
        operations: usize,
        phase: &str,
    ) {
        let mut used = aterm_uds::deadline::descendants_work();
        self.while_children(
            table,
            operations,
            &mut Seams {
                now: &mut Instant::now,
                used: &mut used,
                pause: WORK_TICK,
            },
        )
        .holds(&table, operations, phase);
    }

    /// [`Self::wait_for_while_children_work`]'s wait, over its `seams`.
    fn while_children(&self, table: Digest, operations: usize, seams: &mut Seams<'_>) -> Observed {
        self.observe_until(table, operations, Some(seams))
    }

    /// What this runtime's rounds saw retained once they saw `operations` on `table`, or once the
    /// wait ended unmet ([`Watch`]), and why. `children` are the seams of a wait on processes this
    /// one started, where they take part in the repair; with none, the wall's clock judges.
    fn observe_until(
        &self,
        table: Digest,
        operations: usize,
        mut children: Option<&mut Seams<'_>>,
    ) -> Observed {
        fn clock(children: &mut Option<&mut Seams<'_>>) -> Instant {
            match children {
                Some(seams) => (seams.now)(),
                None => Instant::now(),
            }
        }
        let (state_mu, changed) = &*self.progress;
        let lock = || state_mu.lock().unwrap_or_else(|p| p.into_inner());
        let mut state = lock();
        let mut watch = Watch::new(table, &state, clock(&mut children));
        let mut unmet = None;
        while state.held(&table) < operations {
            let remaining = match watch.remaining(&state, clock(&mut children)) {
                Ok(remaining) => remaining,
                Err(why) => {
                    unmet = Some(why);
                    break;
                }
            };
            let Some(seams) = children.as_deref_mut() else {
                state = changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(|p| p.into_inner())
                    .0;
                continue;
            };
            state = changed
                .wait_timeout(state, remaining.min(seams.pause))
                .unwrap_or_else(|p| p.into_inner())
                .0;
            let rounds = state.rounds();
            // Read the process table without the lock, which the transports' reports take.
            drop(state);
            let used = (seams.used)();
            watch.worked(rounds, used, (seams.now)());
            state = lock();
        }
        let (rounds, elapsed) = watch.ran(&state, clock(&mut children));
        // Release the observer's lock before any caller's assertion can unwind.
        Observed {
            retained: state.retained.clone(),
            unmet,
            rounds,
            elapsed,
        }
    }
}

#[test]
fn a_wait_tracks_its_tables_progress_without_a_round_count_budget() {
    let table = [7; 32];
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let began = Instant::now();
    let mut progress = Progress::default();
    let mut watch = Watch::new(table, &progress, began);
    assert_eq!(watch.remaining(&progress, began), Ok(hang));
    progress.rounds = 128;
    assert_eq!(watch.remaining(&progress, began), Ok(hang));
    progress.retained.insert([8; 32], 9);
    assert_eq!(
        watch.remaining(&progress, began + Duration::from_secs(59)),
        Ok(Duration::from_secs(1))
    );
    progress.retained.insert(table, 1);
    assert_eq!(
        watch.remaining(&progress, began + Duration::from_secs(59)),
        Ok(hang)
    );
    progress.rounds += 1;
    assert_eq!(
        watch.remaining(&progress, began + hang),
        Ok(Duration::from_secs(59))
    );
    progress.retained.insert(table, 2);
    assert_eq!(
        watch.remaining(&progress, began + Duration::from_secs(118)),
        Ok(hang)
    );
    progress.rounds += 128;
    progress.retained.insert([8; 32], 10);
    assert_eq!(
        watch.remaining(&progress, began + Duration::from_secs(177)),
        Ok(Duration::from_secs(1))
    );
    assert_eq!(
        watch.remaining(&progress, began + Duration::from_secs(178)),
        Err(Unmet::Hung { quiet: hang })
    );
    assert_eq!(
        watch.ran(&progress, began + Duration::from_secs(178)),
        (257, Duration::from_secs(178))
    );
}

/// A wait whose repair runs in processes this one started, with the clock in the test's hands:
/// their work while a round of theirs is in flight keeps its minute open however long the wall
/// runs, as does each round they end; their work between rounds (timers, beacons, polls) does
/// not, so a minute of it with no round ends the wait; and work that never brings the table's
/// operations ends it at the guard.
#[test]
fn a_wait_on_children_lasts_while_they_work_on_rounds_and_ends_a_minute_into_their_rest() {
    let table = [7; 32];
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let second = Duration::from_secs(1);
    let flying = Rounds {
        ended: 0,
        in_flight: true,
    };
    let began = Instant::now();
    let progress = Progress::default();
    let mut watch = Watch::new(table, &progress, began);
    watch.worked(flying, 0, began);
    // Each reading rises just inside the minute since the last while a round is in flight:
    // three minutes of wall go by.
    for minute in 1..=3 {
        let at = began + hang * minute - second;
        watch.worked(flying, u64::from(minute), at);
        assert_eq!(
            watch.remaining(&progress, at),
            Ok(hang),
            "children that work on a round keep the wait open"
        );
    }
    let last = began + hang * 3 - second;
    // The same reading again is no work, a round in flight or not.
    watch.worked(flying, 3, last + hang / 2);
    assert_eq!(watch.remaining(&progress, last + hang - second), Ok(second));
    assert_eq!(
        watch.remaining(&progress, last + hang),
        Err(Unmet::Hung { quiet: hang }),
        "a minute in which the children did nothing is a hang"
    );
    assert_eq!(watch.ran(&progress, last + hang), (0, hang * 4 - second));

    // At rest the children still work: their work is no progress, a round they end is.
    let mut watch = Watch::new(table, &progress, began);
    let resting = Rounds::default();
    watch.worked(resting, 0, began);
    let ended = Rounds {
        ended: 1,
        in_flight: false,
    };
    for (reading, (rounds, at, left)) in [
        (resting, began + hang / 2, hang / 2),
        (ended, began + hang - second, hang),
        (ended, began + hang * 3 / 2, hang / 2 - second),
    ]
    .into_iter()
    .enumerate()
    {
        watch.worked(rounds, u64::try_from(reading).unwrap() + 1, at);
        assert_eq!(
            watch.remaining(&progress, at),
            Ok(left),
            "reading {reading}: work at rest renews nothing, a round ended does"
        );
    }
    watch.worked(ended, 9, began + hang * 2 - second);
    assert_eq!(
        watch.remaining(&progress, began + hang * 2 - second),
        Err(Unmet::Hung { quiet: hang }),
        "a minute of work at rest after the last round is a hang"
    );

    let guard = aterm_spec::wait::WORKING_GUARD;
    let mut watch = Watch::new(table, &progress, began);
    let mut reading = 0;
    let mut at = began;
    while at < began + guard {
        reading += 1;
        watch.worked(flying, reading, at);
        assert!(watch.remaining(&progress, at).is_ok());
        at += hang / 2;
    }
    watch.worked(flying, reading + 1, began + guard);
    assert_eq!(
        watch.remaining(&progress, began + guard),
        Err(Unmet::Guard),
        "work that never brings the operations ends at the guard"
    );
}

/// The attachment's wait on processes this one started (`wait_for_while_children_work`, over its
/// seams), with the clock and their work in the test's hands: every look advances the clock
/// ten seconds and finds their work count risen, and the table's operation comes three minutes
/// in. While a round of this runtime's is in flight, or one ends every thirty seconds, the wait
/// lasts until it comes; where none is in flight and none ends, the children's work is their
/// rest, and the wait ends a minute in, unmet.
#[test]
fn an_attachments_wait_on_children_counts_their_work_on_rounds_alone() {
    let table = [7; 32];
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let step = Duration::from_secs(10);
    for case in ["in flight", "ending", "resting"] {
        let attachment = Attachment::new(0);
        attachment.observe(&BTreeMap::new(), 0, case == "in flight");
        let began = Instant::now();
        let at = std::cell::Cell::new(began);
        let mut count = 0;
        let mut used = || {
            at.set(at.get() + step);
            count += 1;
            let ended = u64::from(case == "ending" && count % 3 == 0);
            let came = at.get() >= began + hang * 3;
            attachment.observe(
                &BTreeMap::from([(table, usize::from(came))]),
                ended,
                case == "in flight",
            );
            count
        };
        let observed = attachment.while_children(
            table,
            1,
            &mut Seams {
                now: &mut || at.get(),
                used: &mut used,
                pause: Duration::ZERO,
            },
        );
        if case == "resting" {
            assert!(
                matches!(observed.unmet, Some(Unmet::Hung { quiet }) if quiet >= hang),
                "{case}: {observed:?}"
            );
            assert!(observed.elapsed < hang + step * 2, "{case}: {observed:?}");
            assert_eq!(observed.held(&table), 0, "{case}");
        } else {
            assert_eq!(observed.unmet, None, "{case}: {observed:?}");
            assert!(observed.elapsed >= hang * 3, "{case}: {observed:?}");
            assert_eq!(observed.held(&table), 1, "{case}");
        }
    }
}

/// Retained counts only rise: a round that read no inventory, or a rebuilt transport's empty map,
/// takes nothing from what an earlier round saw.
#[test]
fn an_attachment_keeps_what_its_rounds_saw_retained() {
    let table = [7; 32];
    let attachment = Attachment::new(0);
    attachment.observe(&BTreeMap::from([(table, 4)]), 1, false);
    attachment.observe(&BTreeMap::from([(table, 0)]), 1, false);
    attachment.observe(&BTreeMap::new(), 0, false);
    // Already held, so this returns at once: the detector never runs.
    attachment.wait_for(table, 4, "a held count stays held");
}

fn start(
    lab: &crate::table::tests::Lab,
    observers: &Path,
    machine: usize,
    port: u16,
) -> (Runtime, Attachment) {
    let attachment = Attachment::new(port);
    let observer = observers.join(machine.to_string());
    aterm_secrets::fsx::ensure_private_dir(&observer).unwrap();
    let runtime = Runtime::start_on(
        lab.paths[machine].clone(),
        observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(attachment.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    (runtime, attachment)
}

#[test]
fn a_later_runtime_discovers_the_request_and_the_custodian_takes_a_holders_value_automatically() {
    use crate::credentials;
    use aterm_secrets::{
        need::{self, Purpose, Want},
        text::Audience,
    };
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let observers = aterm_tempfile::tempdir().unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let (custodian, _) = start(&lab, observers.path(), 0, port);
    let (holder, holder_attachment) = start(&lab, observers.path(), 1, port);

    let requester_paths = &lab.paths[2];
    need::raise(
        requester_paths,
        Want::Mailbox("automatic".into()),
        Purpose::ByHand,
    )
    .unwrap();
    credentials::pass(requester_paths).unwrap();
    let scope = crate::catalog::Descriptor::application(
        lab.bootstrap(2).trust(),
        "credentials",
        "requests",
        1,
    )
    .unwrap()
    .scope
    .digest();
    // The requester joins the running owners through discovery; no endpoint/address is supplied.
    let (requester, requester_attachment) = start(&lab, observers.path(), 2, port);
    holder_attachment.wait_for(scope, 1, "a new runtime's request reaches a running holder");
    // The holder's copy is given by hand: a value its store holds is the vault's, which the
    // custodian copies itself. Giving it is the holder's local work; it opens no connection.
    let given = credentials::give_from(&lab.paths[1], "mailbox:automatic", || {
        Ok(aterm_secrets::SecretBytes::from(
            b"automatic-scratch-password".as_slice(),
        ))
    })
    .unwrap();
    assert_eq!((given.retained, given.published), (1, 0));
    // The request, the holder's offer and the custodian's Published.
    requester_attachment.wait_for(
        scope,
        3,
        "an automatic runtime handoff retains Published on the requester",
    );
    requester.stop();
    holder.stop();
    custodian.stop();
    let book = credentials::observe(requester_paths).unwrap().book;
    assert!(book.threads.values().any(
        |thread| thread.want == Want::Mailbox("automatic".into()) && thread.published.is_some()
    ));
    let admin = client::admin_cached(&lab.paths[0]).unwrap();
    assert_eq!(
        admin
            .doc
            .entry(&Audience::Hosts(vec!["m3".into()]), "automatic_password")
            .unwrap()
            .value
            .as_slice(),
        b"automatic-scratch-password"
    );
    for host in ["m1", "m2"] {
        assert!(
            admin
                .doc
                .entry(&Audience::Hosts(vec![host.into()]), "automatic_password")
                .is_none()
        );
    }
}

#[test]
fn a_request_retirement_reaches_the_holder_and_cleans_its_private_give_without_a_custodian() {
    use crate::credentials;
    use aterm_secrets::{
        SecretBytes,
        need::{self, Purpose, Want},
    };
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        need::test_alab_policy(paths);
    }
    let observers = aterm_tempfile::tempdir().unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let requester_paths = &lab.paths[2];
    need::raise(
        requester_paths,
        Want::Mailbox("withdrawn".into()),
        Purpose::ByHand,
    )
    .unwrap();
    credentials::pass(requester_paths).unwrap();
    let scope = crate::catalog::Descriptor::application(
        lab.bootstrap(2).trust(),
        "credentials",
        "requests",
        1,
    )
    .unwrap()
    .scope
    .digest();
    // The custodian is offline: only the two non-admin runtimes participate.
    let (holder, holder_attachment) = start(&lab, observers.path(), 1, reservation.port());
    let (requester, requester_attachment) = start(&lab, observers.path(), 2, reservation.port());
    holder_attachment.wait_for(scope, 1, "the holder retained the request");
    credentials::give_from(&lab.paths[1], "mailbox:withdrawn", || {
        Ok(SecretBytes::from(b"scratch-withdrawn-password".as_slice()))
    })
    .unwrap();
    requester_attachment.wait_for(scope, 2, "the requester retained the holder's Offer");
    need::retire(
        requester_paths,
        "mailbox:withdrawn",
        "scratch request ended",
    )
    .unwrap();
    credentials::pass(requester_paths).unwrap();
    holder_attachment.wait_for(scope, 3, "the holder retained the requester's retirement");
    requester.stop();
    holder.stop();
    let retained = credentials::observe(&lab.paths[1]).unwrap();
    assert_eq!(retained.book.threads.len(), 1, "{retained:?}");
    assert!(
        retained.book.threads.values().all(|thread| thread.ended()),
        "{retained:?}"
    );
    credentials::pass(&lab.paths[1]).unwrap();
    assert!(std::fs::read_dir(&lab.paths[1].dir).unwrap().all(|file| {
        !file
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("credential-give-")
    }));
}

#[test]
fn damaged_catalog_recovers_peer_data_automatically_without_resuming_its_writer() {
    for (healthy_primary, retained_recovery) in [(false, false), (true, false), (true, true)] {
        damaged_catalog_recovery(healthy_primary, retained_recovery);
    }
}

fn damaged_catalog_recovery(healthy_primary: bool, retained_recovery: bool) {
    use crate::{
        catalog::Directory,
        table::{Session, store::Store},
    };
    let roles = vec![Role::Writer; if retained_recovery { 3 } else { 2 }];
    let lab = crate::table::tests::Lab::new(&roles);
    let source_bootstrap = lab.bootstrap(0);
    let source_session = Session::open(&source_bootstrap).unwrap();
    let source_directory = Directory::new(&lab.paths[0], source_bootstrap.trust());
    let scope = source_directory
        .open_application(&source_session, "recovery-probe", "records", 1)
        .unwrap();
    let mut source_store = Store::open(
        &source_session,
        &source_directory.path(scope).unwrap(),
        scope,
    )
    .unwrap();
    let marker = source_store
        .commit_catalog(&source_session, b"peer-data-after-catalog-loss")
        .unwrap();
    drop((source_store, source_session));

    let receiver_bootstrap = lab.bootstrap(1);
    let receiver_session = Session::open(&receiver_bootstrap).unwrap();
    let receiver_directory = Directory::new(&lab.paths[1], receiver_bootstrap.trust());
    receiver_directory.snapshot(&receiver_session).unwrap();
    let local_marker = healthy_primary.then(|| {
        assert_eq!(
            receiver_directory
                .open_application(&receiver_session, "recovery-probe", "records", 1)
                .unwrap(),
            scope
        );
        receiver_directory
            .open_store(&receiver_session, scope)
            .unwrap()
            .commit_catalog(
                &receiver_session,
                b"retained-only-on-the-recovering-machine",
            )
            .unwrap()
    });
    let recovered_marker = retained_recovery.then(|| {
        let offline = lab.bootstrap(2);
        let session = Session::open(&offline).unwrap();
        let directory = Directory::new(&lab.paths[2], offline.trust());
        assert_eq!(
            directory
                .open_application(&session, "recovery-probe", "records", 1)
                .unwrap(),
            scope
        );
        let mut store = directory.open_store(&session, scope).unwrap();
        let receipt = store
            .commit_catalog(&session, b"retained-from-an-offline-third-peer")
            .unwrap();
        let wire = store.wire(&session, &receipt.id).unwrap().unwrap().to_vec();
        Store::join_recovery(
            &receiver_session,
            &receiver_directory.recovery().path(scope).unwrap(),
            scope,
        )
        .unwrap()
        .ingest(&receiver_session, &wire)
        .unwrap();
        receipt
    });
    let catalog_scope = receiver_directory.catalog_scope();
    let original_directory = receiver_directory.path(catalog_scope).unwrap();
    let original_claim_path = crate::table::store::claim_dir(&receiver_session)
        .unwrap()
        .join(crate::table::store::claim_name(catalog_scope));
    let original_claim = std::fs::read(&original_claim_path).unwrap();
    let displaced = lab._home.path().join("preserved-original-catalog");
    std::fs::rename(&original_directory, &displaced).unwrap();
    let original_log = std::fs::read(displaced.join("table.astream")).unwrap();
    assert!(
        receiver_directory
            .snapshot_existing(&receiver_session)
            .is_err()
    );
    drop(receiver_session);

    let observers = aterm_tempfile::tempdir().unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let (source, source_attachment) = start(&lab, observers.path(), 0, reservation.port());
    let (receiver, attachment) = start(&lab, observers.path(), 1, reservation.port());
    attachment.wait_for(
        scope.digest(),
        1,
        "a recovery replica retained peer data automatically",
    );
    receiver.stop();
    let session = Session::observe(&receiver_bootstrap).unwrap();
    let store = receiver_directory.observe_table(&session, scope).unwrap();
    assert!(
        store
            .inventory(&session)
            .unwrap()
            .any(|id| *id == marker.id),
        "ordinary readers must see the newly received peer row beside healthy primary data"
    );
    drop((store, session));
    if healthy_primary {
        source_attachment.wait_for(
            scope.digest(),
            2 + usize::from(retained_recovery),
            "a healthy primary row is relayed while its catalog recovers",
        );
    }
    let source_session = Session::open(&source_bootstrap).unwrap();
    let mut source_store = Store::open(
        &source_session,
        &source_directory.path(scope).unwrap(),
        scope,
    )
    .unwrap();
    let next = source_store
        .commit_catalog(&source_session, b"after-recovery-restart")
        .unwrap();
    drop((source_store, source_session));
    let (receiver, attachment) = start(&lab, observers.path(), 1, reservation.port());
    attachment.wait_for(
        scope.digest(),
        2 + usize::from(healthy_primary) + usize::from(retained_recovery),
        "the restarted recovery replica caught up automatically",
    );
    receiver.stop();
    source.stop();

    let bootstrap = lab.bootstrap(1);
    let session = Session::observe(&bootstrap).unwrap();
    let observed = receiver_directory.snapshot_existing(&session).unwrap();
    assert!(observed.recovery);
    assert!(observed.entries.contains_key(&scope.digest()));
    let store = receiver_directory.observe_table(&session, scope).unwrap();
    assert_eq!(store.is_recovery(), !healthy_primary || retained_recovery);
    let records = store.catalog(&session).unwrap();
    let expected: BTreeSet<_> = [marker.id, next.id]
        .into_iter()
        .chain(local_marker.map(|receipt| receipt.id))
        .chain(recovered_marker.map(|receipt| receipt.id))
        .collect();
    assert_eq!(
        records
            .iter()
            .map(|record| record.header().id)
            .collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(std::fs::read(&original_claim_path).unwrap(), original_claim);
    assert_eq!(
        std::fs::read(displaced.join("table.astream")).unwrap(),
        original_log
    );
    assert!(
        !original_directory.exists(),
        "recovery cannot recreate the original writer directory"
    );
    drop((store, session));
    let session = Session::open(&bootstrap).unwrap();
    assert!(
        receiver_directory
            .open_application(&session, "recovery-probe", "records", 1)
            .is_err()
    );
    let recovery_directory = receiver_directory.recovery();
    let fenced_scope = if healthy_primary {
        catalog_scope
    } else {
        scope
    };
    let mut store = recovery_directory
        .open_store(&session, fenced_scope)
        .unwrap();
    assert!(
        store
            .commit_catalog(&session, b"never-resume-the-old-sequence")
            .is_err()
    );
}

pub(super) fn wait_network(
    handle: &ObservationHandle,
    phase: &str,
    complete: impl Fn(&NetworkObservation) -> bool,
) -> NetworkObservation {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let state = handle.snapshot();
        if complete(&state) {
            return state;
        }
        assert!(Instant::now() < until, "{phase}: {state:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn proposal(key: &str) -> Change {
    Change {
        collection: "accounts".into(),
        key: key.into(),
        value: Some(aterm_json::json!({"provider":"claude","method":"oauth"})),
    }
}

#[test]
fn automatic_round_callbacks_release_control_locks_and_report_real_inventory() {
    use aterm_secrets::commons::journal::Journal;
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-callback-")
        .tempdir_in("/tmp")
        .unwrap();
    let owner_observer = observers.path().join("0");
    aterm_secrets::fsx::ensure_private_dir(&owner_observer).unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write([21; 16], vec![proposal("callback-proof")])
        .unwrap();
    let bootstrap = lab.bootstrap(0);
    let journal = bootstrap.journal_dir().to_owned();
    let trust = bootstrap.trust();
    drop(bootstrap);
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    let (send, receive) = std::sync::mpsc::channel();
    let attachment = Attachment::new(port);
    let owner = Runtime::start_on_with_reports(
        lab.paths[0].clone(),
        owner_observer.join("aterm.toml"),
        |_| {},
        Network {
            isolated: Some(attachment),
            ..Network::default()
        },
        move |report| {
            // Waiting for the actual same-custody lock is a hang detector:
            // another worker may briefly own it, but this callback must not.
            let available = Journal::open_existing_until(
                &journal,
                trust,
                Instant::now() + Duration::from_secs(60),
            )
            .map(|_| ());
            let _ = send.send((report, available.map_err(|error| error.to_string())));
        },
    )
    .unwrap();
    let (receiver, progress) = start(&lab, observers.path(), 1, port);
    progress.wait_for(table.digest(), 1, "callback receiver retained data");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (report, available) = receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        assert!(
            available.is_ok(),
            "callback retained a control lock: {available:?}"
        );
        if report.control.is_ok()
            && report.actor.is_some()
            && report.application.is_some_and(|app| {
                app.table == Some(table.digest())
                    && app.local_known.is_some_and(|summary| summary.count == 1)
            })
        {
            assert!(report.application_error.is_none());
            assert_eq!(report.dropped_before, 0);
            break;
        }
    }
    receiver.stop();
    owner.stop();
}

#[test]
fn automatic_runtime_discovers_tables_catches_up_late_devices_and_transfers_local_ownership() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 3]);
    // Darwin limits Unix socket paths to 103 bytes. Keep the observer sockets
    // short independently of the generated vaults' deeper scratch paths.
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-sync-")
        .tempdir_in("/tmp")
        .unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write([1; 16], vec![proposal("before-start")])
        .unwrap();
    // Neither receiver opens an application table before discovery. This also
    // catches a test accidentally pre-provisioning all three local replicas.
    for path in &lab.paths[1..] {
        assert!(!path.dir.join("commons/tables").exists());
    }

    // An ephemeral receive socket reserves an isolated discovery port. Owners
    // use the native multicast/TCP/TLS path and their ordinary scheduler threads.
    let group = Ipv4Addr::new(239, 255, 31, 84);
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port = reservation.port();
    let (owner, _) = start(&lab, observers.path(), 0, port);
    let owner_state = owner.network_observation();
    let identity = wait_network(&owner_state, "first local owner", |state| {
        state.observer.is_some() && state.visibility == Visibility::Active
    })
    .observer
    .unwrap();
    let (receiver, receiver_progress) = start(&lab, observers.path(), 1, port);
    receiver_progress.wait_for(table.digest(), 1, "receiver discovered the table");

    let (follower, follower_progress) = start(&lab, observers.path(), 0, port);
    let follower_state = follower.network_observation();
    let relayed = wait_network(
        &follower_state,
        "second local runtime uses owner relay",
        |state| {
            state.via_local_process == Some(std::process::id())
                && state.visibility == Visibility::Active
                && state.observer.as_ref() == Some(&identity)
        },
    );
    assert_eq!(relayed.observer.as_ref(), Some(&identity));
    assert!(
        follower_progress
            .progress
            .0
            .lock()
            .unwrap()
            .retained
            .is_empty()
    );
    owner.stop();
    let taken_over = wait_network(
        &follower_state,
        "remaining runtime becomes local owner",
        |state| {
            state.via_local_process.is_none()
                && state.visibility == Visibility::Active
                && state.observer.as_ref() == Some(&identity)
        },
    );
    assert_eq!(taken_over.observer.as_ref(), Some(&identity));
    follower_progress.wait_for(table.digest(), 1, "new owner repaired the existing table");

    // This third device is already admitted, but its aterm starts for the first
    // time after publication. No receiver-side table open or manual sync occurs.
    let (late, late_progress) = start(&lab, observers.path(), 2, port);
    late_progress.wait_for(table.digest(), 1, "late device discovered existing data");
    receiver.stop();
    follower.stop();
    let offline = Proposals::at(lab.paths[1].clone());
    let conflict = |email: &str| Change {
        value: Some(aterm_json::json!({"provider":"claude","method":"oauth","email":email})),
        ..proposal("offline-conflict")
    };
    let left = source
        .write([2; 16], vec![conflict("left@example.test")])
        .unwrap();
    let right = offline
        .write([3; 16], vec![conflict("right@example.test")])
        .unwrap();
    for (db, absent) in [(&source, right.id), (&offline, left.id)] {
        let snapshot = db.snapshot().unwrap();
        assert_eq!(snapshot.operations.len(), 2);
        assert!(snapshot.operations.iter().all(|op| op.header.id != absent));
        assert_eq!(
            snapshot.cells[&("accounts".into(), "offline-conflict".into())]
                .candidates
                .len(),
            1,
            "each isolated writer sees only its own edit before reconnection"
        );
    }
    let (restarted_owner, owner_progress) = start(&lab, observers.path(), 0, port);
    late_progress.wait_for(
        table.digest(),
        2,
        "online late device received the next change",
    );
    let (restarted_receiver, restarted_progress) = start(&lab, observers.path(), 1, port);
    restarted_progress.wait_for(
        table.digest(),
        3,
        "offline writer caught up without discarding its own edit",
    );
    owner_progress.wait_for(
        table.digest(),
        3,
        "original writer received the offline edit",
    );
    late_progress.wait_for(
        table.digest(),
        3,
        "late device received both concurrent edits",
    );
    restarted_receiver.stop();
    late.stop();
    restarted_owner.stop();

    // Completed inventories are only synchronization progress. Check actual
    // application values and signed operation identity on every durable replica.
    let expected = source.snapshot().unwrap();
    assert_eq!(expected.operations.len(), 3);
    assert_eq!(expected.cells.len(), 2);
    let concurrent = &expected.cells[&("accounts".into(), "offline-conflict".into())].candidates;
    assert_eq!(
        concurrent.len(),
        2,
        "concurrent values remain an explicit conflict"
    );
    for (receipt, email) in [(left, "left@example.test"), (right, "right@example.test")] {
        assert!(concurrent.iter().any(|candidate| {
            candidate.operation == receipt.id && candidate.value["email"].as_str() == Some(email)
        }));
    }
    for path in &lab.paths[1..] {
        let actual = Proposals::at(path.clone()).snapshot().unwrap();
        assert_eq!(actual.cells, expected.cells);
        assert_eq!(
            actual
                .operations
                .iter()
                .map(|operation| operation.header.id)
                .collect::<Vec<_>>(),
            expected
                .operations
                .iter()
                .map(|operation| operation.header.id)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn an_application_created_after_both_runtimes_start_is_discovered_and_writable_on_both() {
    use crate::catalog::{Descriptor, Directory};
    use crate::table::Session;

    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-late-table-")
        .tempdir_in("/tmp")
        .unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let (owner, owner_progress) = start(&lab, observers.path(), 0, reservation.port());
    let (receiver, receiver_progress) = start(&lab, observers.path(), 1, reservation.port());
    for runtime in [&owner, &receiver] {
        wait_network(
            &runtime.network_observation(),
            "runtime before publication",
            |state| state.visibility == Visibility::Active && state.observer.is_some(),
        );
    }

    // This application's identity is first created after both owners are active.
    // The receiver has neither a table handle nor a pre-created replica.
    let bootstrap = lab.bootstrap(0);
    let descriptor =
        Descriptor::fresh(bootstrap.trust(), "late-application", "records", 1).unwrap();
    let scope = descriptor.scope;
    let payloads: [&[u8]; 2] = [b"created-after-startup", b"written-by-the-discovering-peer"];
    {
        let peer = lab.bootstrap(1);
        let session =
            Session::observe_until(&peer, Instant::now() + Duration::from_secs(60)).unwrap();
        assert!(
            Directory::new(&lab.paths[1], peer.trust())
                .observe_table_if_exists(&session, scope)
                .unwrap()
                .is_none()
        );
    }
    let first = {
        let session =
            Session::open_until(&bootstrap, Instant::now() + Duration::from_secs(60)).unwrap();
        let directory = Directory::new(&lab.paths[0], bootstrap.trust());
        directory.publish(&session, &descriptor).unwrap();
        directory
            .open_store(&session, scope)
            .unwrap()
            .commit_catalog(&session, payloads[0])
            .unwrap()
            .id
    };
    receiver_progress.wait_for(
        scope.digest(),
        1,
        "running peer discovered the new application",
    );
    let second = {
        let peer = lab.bootstrap(1);
        let session = Session::open_until(&peer, Instant::now() + Duration::from_secs(60)).unwrap();
        // Opening the already discovered store performs no declaration or peer sync.
        Directory::new(&lab.paths[1], peer.trust())
            .open_store(&session, scope)
            .unwrap()
            .commit_catalog(&session, payloads[1])
            .unwrap()
            .id
    };
    owner_progress.wait_for(
        scope.digest(),
        2,
        "new application carries a reverse-direction write",
    );
    receiver.stop();
    owner.stop();

    // Independent reads after shutdown authenticate the exact operations and values.
    let expected = BTreeMap::from([
        (first, payloads[0].to_vec()),
        (second, payloads[1].to_vec()),
    ]);
    for paths in &lab.paths {
        let bootstrap = Bootstrap::cached(paths).unwrap().unwrap();
        let session = Session::observe(&bootstrap).unwrap();
        let directory = Directory::new(paths, bootstrap.trust());
        let catalog = directory.snapshot_existing(&session).unwrap();
        assert!(
            catalog.entries[&scope.digest()]
                .declarations
                .iter()
                .any(|entry| entry.descriptor == descriptor)
        );
        let actual: BTreeMap<_, _> = directory
            .observe_table(&session, scope)
            .unwrap()
            .catalog(&session)
            .unwrap()
            .iter()
            .map(|record| (record.header().id, record.payload().to_vec()))
            .collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn windowless_background_runtime_replicates_yields_to_window_and_resumes_with_negative_control() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-bg-")
        .tempdir_in("/tmp")
        .unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write([31; 16], vec![proposal("windowless-first")])
        .unwrap();

    // Negative control ("runtime only in windows"): when no interactive window runs on
    // device 1 and the node job starts no background runtime, device 1 never discovers or
    // replicates the table.
    let window_only_runtime =
        |window_running: bool| -> Option<bool> { window_running.then_some(true) };
    assert_eq!(window_only_runtime(false), None);
    assert!(!lab.paths[1].dir.join("commons/tables").exists());

    let group = Ipv4Addr::new(239, 255, 31, 84);
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port = reservation.port();
    let (owner, _) = start(&lab, observers.path(), 0, port);

    let observer_1 = observers.path().join("1");
    aterm_secrets::fsx::ensure_private_dir(&observer_1).unwrap();
    let config_1 = observer_1.join("aterm.toml");
    let bg_progress = Attachment::new(port);
    let (bg_tx, bg_rx) = std::sync::mpsc::channel();
    let bg_runtime = Runtime::start_background_on(
        lab.paths[1].clone(),
        config_1.clone(),
        move |status| {
            let _ = bg_tx.send(status);
        },
        Network {
            isolated: Some(bg_progress.clone()),
            ..Network::default()
        },
    )
    .unwrap();

    // Windowless member replicates while no window runs.
    bg_progress.wait_for(
        table.digest(),
        1,
        "windowless background runtime replicated initial write",
    );
    let bg_obs = bg_runtime.network_observation();
    wait_network(&bg_obs, "background runtime owns observation", |state| {
        state.via_local_process.is_none() && state.visibility == Visibility::Active
    });
    #[cfg(target_os = "macos")]
    assert_eq!(
        crate::network_state::directory(&config_1),
        aterm_uds::quiet_state::destination(&config_1.with_file_name("network-state")),
        "window priority must follow the directory actually migrated by the background owner",
    );

    // A window claims priority and takes the transport and observation.
    let win_progress = Attachment::new(port);
    let win_runtime = Runtime::start_on(
        lab.paths[1].clone(),
        config_1.clone(),
        |_| {},
        Network {
            isolated: Some(win_progress.clone()),
            ..Network::default()
        },
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status = bg_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("background runtime must yield with Status::Waiting when window opens");
        if status == Status::Waiting {
            break;
        }
    }
    let win_obs = win_runtime.network_observation();
    wait_network(&win_obs, "window runtime takes local ownership", |state| {
        state.via_local_process.is_none() && state.visibility == Visibility::Active
    });
    source
        .write([32; 16], vec![proposal("while-window-owns")])
        .unwrap();
    win_progress.wait_for(
        table.digest(),
        2,
        "window runtime replicated write while holding lease",
    );

    // Closing the window releases WindowClaim and Lease; background runtime resumes and
    // replicates the next change.
    win_runtime.stop();
    source
        .write([33; 16], vec![proposal("after-window-closed")])
        .unwrap();
    bg_progress.wait_for(
        table.digest(),
        3,
        "background runtime resumed after window exit and replicated third write",
    );

    bg_runtime.stop();
    owner.stop();

    let expected = source.snapshot().unwrap();
    let actual = Proposals::at(lab.paths[1].clone()).snapshot().unwrap();
    assert_eq!(expected.operations.len(), 3);
    assert_eq!(actual.cells, expected.cells);
}

/// A push hands a claims row on now: once the owner's push says a peer took it, the peer reads the
/// row's new revision, though its rounds take turns with another table. A runtime that serves no
/// repair says so at once.
///
/// NEGATIVE CONTROL: drop `board.take()` from `serve_mode`'s loop (the owner never takes the ask),
/// and the push waits out its one-minute deadline: `TimedOut`, not `Reached`.
#[test]
fn a_pushed_claims_row_reaches_a_peer_before_the_push_returns() {
    use crate::accounts::claims;
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-push-")
        .tempdir_in("/tmp")
        .unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source.write([41; 16], vec![proposal("push")]).unwrap();
    let row = |counter: u64| {
        let now = aterm_time::unix_millis().unwrap() / 1000;
        aterm_json::json!({"counter": counter, "until": now + 3600, "claims": []})
    };
    claims::write(&lab.paths[0], &row(1)).unwrap();
    let me = claims::read(&lab.paths[0]).unwrap().me;
    let counter = |n: usize| {
        claims::read(&lab.paths[n]).ok().and_then(|read| {
            read.rows
                .iter()
                .find(|(machine, _)| *machine == me)
                .and_then(|(_, body)| body["counter"].as_u64())
        })
    };
    assert_eq!(
        claims::push(
            &lab.paths[0],
            &Pusher::default(),
            Instant::now() + Duration::from_secs(60)
        )
        .unwrap(),
        Pushed::NotServing
    );

    let group = Ipv4Addr::new(239, 255, 31, 84);
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port = reservation.port();
    let (owner, _) = start(&lab, observers.path(), 0, port);
    let (peer, progress) = start(&lab, observers.path(), 1, port);
    progress.wait_for(table.digest(), 1, "the peer took the other table");
    let until = Instant::now() + Duration::from_secs(60);
    while counter(1) != Some(1) {
        assert!(Instant::now() < until, "the peer never took the first row");
        thread::sleep(Duration::from_millis(10));
    }

    claims::write(&lab.paths[0], &row(2)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let pushed = loop {
        // The owner knows its peer once a round it began with it succeeded.
        match claims::push(&lab.paths[0], &owner.pusher(), deadline).unwrap() {
            Pushed::NoPeers if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10))
            }
            pushed => break pushed,
        }
    };
    assert_eq!(pushed, Pushed::Reached);
    assert_eq!(counter(1), Some(2), "the peer holds the pushed revision");
    peer.stop();
    owner.stop();
}

#[test]
fn two_loopback_islands_write_admit_and_remove_then_join_and_match_inventories() {
    use aterm_secrets::{
        client::{
            JoinOptions, JoinPassword, Paths, admin_cached, admin_commons_init, admin_open,
            admin_publish_approved, admit_member_in_log, create, join, load_records,
            machine_identity, remove_member_in_log, save_records, sync, test_sign_genesis,
        },
        commons::{
            binding::Pending,
            bootstrap::{Bootstrap, Device},
            journal::Journal,
        },
        membership_log::{Entrant, Evidence},
    };
    use std::collections::VecDeque;

    let join_local = |paths: &Paths, source: &str, host: &str| {
        join(
            paths,
            &JoinOptions {
                source: source.to_owned(),
                host: Some(host.to_owned()),
                interactive: false,
                wait: Duration::from_secs(2),
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .expect("join local vault");
    };
    let enroll_v1 = |owner_paths: &Paths, member_paths: &Paths| {
        sync(member_paths, None).expect("sync member");
        let owner = Bootstrap::required(owner_paths).expect("owner bootstrap");
        let peer = Bootstrap::required(member_paths).expect("peer bootstrap");
        let req = peer.request().expect("peer request");
        let (mut pending, ch) =
            Pending::issue(&req, owner.trust().network, owner.trust().authority).unwrap();
        let ans = peer.device().answer(&req, &ch).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin_cached(owner_paths)
            .unwrap()
            .enroll_commons_member(&owner, &mut journal, pending.verify(&ans).unwrap())
            .expect("v1 enroll");
    };

    let root = aterm_tempfile::tempdir().unwrap();
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-f18-rt-")
        .tempdir_in("/tmp")
        .unwrap();
    let vault_file = root.path().join("rt.vault");

    // Found `{m1, m2, m3, m4}` where `{m1, m2, m3}` are bound in `Genesis` and `m4` is an unproved
    // founder (`binding: None`) that must enroll automatically over `ACE1` against non-admin `m2`!
    let m1 = Paths::for_home(&root.path().join("m1"));
    let created = create(&m1, vault_file.to_str().unwrap(), None, Some("m1")).unwrap();
    admin_commons_init(&m1).unwrap();
    let pinned = format!("{}#{}", vault_file.display(), created.signer);

    let m2 = Paths::for_home(&root.path().join("m2"));
    let m3 = Paths::for_home(&root.path().join("m3"));
    let m4 = Paths::for_home(&root.path().join("m4"));
    let r2 = machine_identity(&m2).unwrap().recipient().to_bech32();
    let r3 = machine_identity(&m3).unwrap().recipient().to_bech32();
    let r4 = machine_identity(&m4).unwrap().recipient().to_bech32();
    {
        let admin = admin_open(&m1).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine("m2", &r2).unwrap();
        doc.add_machine("m3", &r3).unwrap();
        doc.add_machine("m4", &r4).unwrap();
        admin_publish_approved(&m1, &admin, &doc, &[r2.clone(), r3.clone(), r4.clone()]).unwrap();
    }
    for (p, h) in [(&m2, "m2"), (&m3, "m3"), (&m4, "m4")] {
        join_local(p, &pinned, h);
    }
    enroll_v1(&m1, &m2);
    enroll_v1(&m1, &m3);
    // Intentionally leave `m4` un-enrolled before `sign_genesis` so `Genesis` lists `m4` with no
    // binding (`unproved`).
    for p in [&m2, &m3, &m4] {
        sync(p, None).unwrap();
    }
    let genesis_out = root.path().join("rt.genesis");
    test_sign_genesis(&m1, &genesis_out, &created.signer).unwrap();
    let seeded_records = load_records(&m1).unwrap().read_encoded().unwrap();
    for p in [&m2, &m3, &m4] {
        save_records(p, &seeded_records).unwrap();
    }
    let trust = Bootstrap::required(&m1).unwrap().trust();
    assert!(
        Bootstrap::required(&m4)
            .unwrap()
            .log_witness()
            .unwrap()
            .is_none()
    );

    let reports = Arc::new(Mutex::new(BTreeMap::<String, VecDeque<RepairRound>>::new()));
    let start_paths = |paths: &Paths, tag: &str, port: u16| -> (Runtime, Attachment) {
        let att = Attachment::new(port);
        let obs = observers.path().join(tag);
        aterm_secrets::fsx::ensure_private_dir(&obs).unwrap();
        let reports = reports.clone();
        let tag = tag.to_owned();
        let rt = Runtime::start_on_with_reports(
            paths.clone(),
            obs.join("aterm.toml"),
            |_| {},
            Network {
                isolated: Some(att.clone()),
                ..Network::default()
            },
            move |round| {
                let mut reports = reports.lock().unwrap_or_else(|p| p.into_inner());
                let recent = reports.entry(tag.clone()).or_default();
                if recent.len() == 8 {
                    recent.pop_front();
                }
                recent.push_back(round);
            },
        )
        .unwrap();
        (rt, att)
    };

    let group = Ipv4Addr::new(239, 255, 31, 84);
    let res_a = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let res_b = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let res_joined = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port_a = res_a.port();
    let port_b = res_b.port();
    let port_joined = res_joined.port();

    // --- ISLAND A (`port_a`): `m1` initializes `Proposals`, admits newcomer `m5` via the log,
    //     both `m1` and `m5` write, and sync on `port_a`. ---
    let p1 = Proposals::at(m1.clone());
    let table = p1.initialize().unwrap();
    p1.write([41; 16], vec![proposal("island-a-m1")]).unwrap();

    let m5 = Paths::for_home(&root.path().join("m5"));
    aterm_secrets::client::configure_log_member(&m5, &pinned, "m5").unwrap();
    let _ = machine_identity(&m5).unwrap();
    let req5 = Device::open(&m5)
        .unwrap()
        .unwrap()
        .request(trust.network, trust.authority)
        .unwrap();
    let actor5 = req5.binding().actor();
    admit_member_in_log(
        &m1,
        Entrant {
            request: req5,
            node_key: [0x55; 32],
            name: "m5".to_owned(),
            label: "m5".to_owned(),
        },
        Evidence::Phrase,
    )
    .unwrap();
    save_records(&m5, &load_records(&m1).unwrap().read_encoded().unwrap()).unwrap();

    let (rt_a1, att_a1) = start_paths(&m1, "a1", port_a);
    let (rt_a5, att_a5) = start_paths(&m5, "a5", port_a);
    att_a5.wait_for(table.digest(), 1, "m5 discovered table on island A");
    Proposals::at(m5.clone())
        .write([45; 16], vec![proposal("island-a-m5")])
        .unwrap();
    att_a1.wait_for(table.digest(), 2, "m1 received m5 write on island A");
    att_a5.wait_for(table.digest(), 2, "m5 retained 2 ops on island A");
    rt_a5.stop();
    rt_a1.stop();

    // --- ISLAND B (`port_b`): non-admin `m2` initializes `Proposals` and writes; unproved founder
    //     `m4` enrolls over `ACE1` against non-admin `m2`, removes `m3`, and writes! ---
    let p2 = Proposals::at(m2.clone());
    assert_eq!(p2.initialize().unwrap(), table);
    p2.write([42; 16], vec![proposal("island-b-m2")]).unwrap();

    let (rt_b2, att_b2) = start_paths(&m2, "b2", port_b);
    let (rt_b4, att_b4) = start_paths(&m4, "b4", port_b);
    att_b4.wait_for(
        table.digest(),
        1,
        "unproved m4 enrolled over ACE1 against non-admin m2 and replicated table",
    );
    // rt_b4 runs on m4: its repair rounds take and release commons.lock.
    assert!(
        Bootstrap::required_until(&m4, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
            .unwrap()
            .log_witness()
            .unwrap()
            .is_some()
    );

    let actor_m3 = Bootstrap::required(&m3)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .actor();
    remove_member_in_log(&m4, actor_m3).expect("m4 removes m3 on island B");
    Proposals::at(m4.clone())
        .write([44; 16], vec![proposal("island-b-m4-after-remove")])
        .unwrap();
    att_b2.wait_for(
        table.digest(),
        2,
        "m2 received m4 write and removal on island B",
    );
    att_b4.wait_for(table.digest(), 2, "m4 retained 2 ops on island B");
    rt_b4.stop();
    rt_b2.stop();

    // --- JOIN (`port_joined`): both loopback islands move onto one multicast port and trade
    //     their records over `ACX2`, which signs none. `m4`'s rotation left newcomer `m5` a
    //     `Wrap` gap: with the runtimes stopped, the holders' vault passes (each subscriber's,
    //     after it pulls records) close it, at first sight and again past every rank's step with
    //     what each signed carried to the others between, and the runtimes, started again, carry
    //     the `Wrap` and converge to identical 4-operation inventories! ---
    let joined = [
        start_paths(&m1, "j1", port_joined).0,
        start_paths(&m2, "j2", port_joined).0,
        start_paths(&m4, "j4", port_joined).0,
        start_paths(&m5, "j5", port_joined).0,
    ];
    let holders = [&m1, &m2, &m4];
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "every holder holds m5's admission and m3's removal over ACX2",
        || {
            holders
                .iter()
                .all(|paths| {
                    load_records(paths).ok().is_some_and(|store| {
                        store.log().is_some_and(|log| {
                            let membership = log.membership();
                            membership.contains(&actor5) && membership.removal(&actor_m3).is_some()
                        })
                    })
                })
                .then_some(())
        },
    );
    for runtime in joined {
        runtime.stop();
    }
    let m5_recipient = machine_identity(&m5).unwrap().recipient();
    let at = aterm_time::unix_millis().unwrap_or(0);
    let mut closed = Vec::new();
    for at in [at, at + 100 * 60_000] {
        for paths in holders {
            closed.extend(aterm_secrets::client::vault_pass_at(paths, at).unwrap());
        }
        for from in holders {
            let records = load_records(from).unwrap().read_encoded().unwrap();
            for to in holders {
                save_records(to, &records).unwrap();
            }
        }
    }
    let wraps_to_m5 = |records: &[aterm_secrets::membership_log::Record]| {
        records
            .iter()
            .filter(|record| {
                matches!(
                    record.body(),
                    aterm_secrets::membership_log::Body::Wrap { recipient, .. }
                        if *recipient == m5_recipient
                )
            })
            .count()
    };
    assert!(
        wraps_to_m5(&closed) > 0,
        "a ranked holder's vault pass closes m5's gap: {closed:?}"
    );

    let (rt_j1, att_j1) = start_paths(&m1, "k1", port_joined);
    let (rt_j2, att_j2) = start_paths(&m2, "k2", port_joined);
    let (rt_j4, att_j4) = start_paths(&m4, "k4", port_joined);
    let (rt_j5, att_j5) = start_paths(&m5, "k5", port_joined);

    for (name, att) in [
        ("m1", &att_j1),
        ("m2", &att_j2),
        ("m4", &att_j4),
        ("m5", &att_j5),
    ] {
        let observed = att.observe_until(table.digest(), 4, None);
        if observed.held(&table.digest()) < 4 {
            let rounds = reports.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let peers = [&rt_j1, &rt_j2, &rt_j4, &rt_j5]
                .map(|runtime| runtime.network_observation().snapshot());
            let stores = [&m1, &m2, &m4, &m5].map(|paths| {
                Proposals::at(paths.clone())
                    .snapshot()
                    .map(|snapshot| (snapshot.retained, snapshot.withheld, snapshot.cells.len()))
            });
            panic!(
                "{name} converged after islands joined: automatic repair did not retain 4 \
                 operations; observed {observed:?}; stores (retained, withheld, cells) \
                 for m1/m2/m4/m5 {stores:?}; peers {peers:?}; recent rounds {rounds:?}"
            );
        }
    }

    rt_j5.stop();
    rt_j4.stop();
    rt_j2.stop();
    rt_j1.stop();

    let expected = Proposals::at(m1.clone()).snapshot().unwrap();
    assert_eq!(expected.operations.len(), 4);
    assert_eq!(expected.cells.len(), 4);
    for p in [&m2, &m4, &m5] {
        let actual = Proposals::at(p.clone()).snapshot().unwrap();
        assert_eq!(actual.cells, expected.cells);
        assert_eq!(
            actual
                .operations
                .iter()
                .map(|o| o.header.id)
                .collect::<Vec<_>>(),
            expected
                .operations
                .iter()
                .map(|o| o.header.id)
                .collect::<Vec<_>>()
        );
    }
}

/// The registry's default-on owner, not fleet_db's manual server: a cold peer discovers sealed
/// mailbox rows, edits one, and resumes after both owners restart. The generation is one builds
/// up to 0.200.0 read, which every Mac holds until a roll: a sign-in opens each sealed token in
/// place and asks nothing of the secret store (the closed store stands in, whose every call
/// refuses).
#[test]
fn registry_mailbox_secrets_discover_bidirectionally_and_survive_owner_restart() {
    use crate::{
        alab::{
            Registry,
            mail::{self, Credential, Withheld},
            registry::{SEALED_ADDRESS as ADDRESS, SEALED_MAILBOX as MAILBOX},
            secret::Journal,
        },
        att::{Handle, REGISTRY},
        typed::{
            cell::{self, CellKey, CellValue},
            keys,
            secret::Secret,
        },
    };
    use aterm_json::json;
    const FIRST_TOTP: &str = "JBSWY3DPEHPK3PXP";
    const SECOND_TOTP: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("registry-owner-")
        .tempdir_in("/tmp")
        .unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    for paths in &lab.paths {
        assert!(!paths.dir.join("commons/tables").exists());
    }
    let (owner, owner_progress) = start(&lab, observers.path(), 0, port);
    let (receiver, receiver_progress) = start(&lab, observers.path(), 1, port);
    for runtime in [&owner, &receiver] {
        wait_network(
            &runtime.network_observation(),
            "owner active before any application",
            |s| s.visibility == Visibility::Active && s.observer.is_some(),
        );
    }
    let login = keys::address_pk("owner@example.invalid").unwrap();
    let mailbox = keys::mailbox_pk(&login).unwrap();
    let put = |collection: &crate::typed::Collection, row: &str, column: &str, value| {
        cell::change(
            collection,
            CellKey {
                pk: row,
                col: column,
                elem: None,
            },
            &CellValue::owner(1, value),
        )
        .unwrap()
    };
    let source = Registry::at(lab.paths[0].clone());
    let table = source.open().unwrap();
    Handle::at(lab.paths[0].clone(), &REGISTRY)
        .commit_sealed_until(true, crate::admission::deadline(), |_, sealer| {
            let password = sealer.seal(
                "address",
                &login,
                "password",
                &Secret::new("fixture-initial-password".into()),
            )?;
            let totp = sealer.seal("mailbox", &mailbox, "totp", &Secret::new(FIRST_TOTP.into()))?;
            Ok((
                (),
                vec![vec![
                    put(&ADDRESS, &login, "addr", json!("owner@example.invalid")),
                    put(&ADDRESS, &login, "password", json!(password)),
                    put(&MAILBOX, &mailbox, "label", json!("fixture-mailbox")),
                    put(&MAILBOX, &mailbox, "service", json!("proton")),
                    put(&MAILBOX, &mailbox, "code_path", json!("webmail")),
                    put(&MAILBOX, &mailbox, "login", json!(login)),
                    put(&MAILBOX, &mailbox, "totp", json!(totp)),
                    put(
                        &MAILBOX,
                        &mailbox,
                        "webmail",
                        json!("https://mail.example.invalid/inbox"),
                    ),
                ]],
            ))
        })
        .unwrap();
    let first_count = source.view().unwrap().ops.len();
    receiver_progress.wait_for(
        table.digest(),
        first_count,
        "cold peer discovers registry and sealed mailbox",
    );
    let remote = Registry::at(lab.paths[1].clone());
    let check = |registry: &Registry, machine: usize, expected: &str, expected_totp: &str| {
        let rows = mail::sign_in_mailboxes(registry).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].login, "owner@example.invalid");
        assert!(rows[0].held.contains(&Credential::Password));
        assert!(rows[0].held.contains(&Credential::Totp));
        let journal = Journal::at(&observers.path().join(format!("reveals-{machine}")));
        assert_eq!(
            mail::reveal_bound(
                registry,
                &crate::alab::secret::Closed,
                &journal,
                &mailbox,
                &[Credential::Password, Credential::Totp],
                1_000,
                |found| found.login == "owner@example.invalid"
                    && found.service.as_deref() == Some("proton"),
                |values| values.len() == 2
                    && values[0].as_str() == Some(expected)
                    && values[1].as_str() == Some(expected_totp),
            )
            .unwrap(),
            Ok(true)
        );
    };
    check(&remote, 1, "fixture-initial-password", FIRST_TOTP);
    let journal = Journal::at(&observers.path().join("refused-bundles"));
    let consumed = std::cell::Cell::new(false);
    assert_eq!(
        mail::reveal_bound(
            &remote,
            &crate::alab::secret::Closed,
            &journal,
            &mailbox,
            &[Credential::Password, Credential::Totp],
            1_000,
            |found| found.login == "another@example.invalid",
            |_| consumed.set(true),
        )
        .unwrap(),
        Err(Withheld::NoMailbox)
    );
    assert!(
        !consumed.get(),
        "changed binding exposed a credential bundle"
    );
    assert!(
        !observers
            .path()
            .join("refused-bundles/reveals.jsonl")
            .exists(),
        "binding was refused before a Secret opened"
    );
    assert_eq!(
        mail::reveal_bound(
            &remote,
            &crate::alab::secret::Closed,
            &journal,
            &mailbox,
            &[Credential::Password, Credential::MailboxPassword],
            1_000,
            |found| found.login == "owner@example.invalid",
            |_| consumed.set(true),
        )
        .unwrap(),
        Err(Withheld::NotSet)
    );
    assert!(
        !consumed.get(),
        "a missing later part exposed an earlier credential"
    );
    Handle::at(lab.paths[1].clone(), &REGISTRY)
        .commit_sealed_until(false, crate::admission::deadline(), |_, sealer| {
            let password = sealer.seal(
                "address",
                &login,
                "password",
                &Secret::new("fixture-reverse-password".into()),
            )?;
            let totp = sealer.seal(
                "mailbox",
                &mailbox,
                "totp",
                &Secret::new(SECOND_TOTP.into()),
            )?;
            Ok((
                (),
                vec![vec![
                    put(&ADDRESS, &login, "password", json!(password)),
                    put(&MAILBOX, &mailbox, "totp", json!(totp)),
                ]],
            ))
        })
        .unwrap();
    let second_count = remote.view().unwrap().ops.len();
    owner_progress.wait_for(
        table.digest(),
        second_count,
        "original owner receives the reverse write",
    );
    check(&source, 0, "fixture-reverse-password", SECOND_TOTP);
    owner.stop();
    receiver.stop();
    let inventory = |registry: &Registry| {
        registry
            .view()
            .unwrap()
            .ops
            .into_iter()
            .map(|o| o.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        inventory(&source),
        inventory(&remote),
        "bidirectional equality uses authenticated operation ids"
    );
    for (machine, registry) in [(0, &source), (1, &remote)] {
        check(registry, machine, "fixture-reverse-password", SECOND_TOTP);
    }

    let (owner, _) = start(&lab, observers.path(), 0, port);
    let (receiver, progress) = start(&lab, observers.path(), 1, port);
    source
        .write(vec![vec![put(
            &MAILBOX,
            &mailbox,
            "label",
            json!("changed-after-restart"),
        )]])
        .unwrap();
    progress.wait_for(
        table.digest(),
        source.view().unwrap().ops.len(),
        "restarted owners resume automatic repair",
    );
    receiver.stop();
    owner.stop();
    assert_eq!(inventory(&source), inventory(&remote));
    assert_eq!(
        mail::sign_in_mailboxes(&remote).unwrap()[0].label,
        "changed-after-restart"
    );
    check(&remote, 1, "fixture-reverse-password", SECOND_TOTP);
}

/// The same at the generation whose secret cells name store entries, which only a test reaches
/// in this build: a cold peer discovers mailbox rows, edits one, and resumes after both owners
/// restart. One [`Memory`] store stands for the lab's shared vault; no value is in a row the
/// peers exchange.
#[test]
fn registry_mailbox_references_discover_bidirectionally_and_survive_owner_restart() {
    use crate::{
        alab::{
            Registry,
            mail::{self, Credential, Withheld},
            registry::{ADDRESS, MAILBOX},
            secret::Journal,
        },
        att::{Handle, REGISTRY},
        typed::{
            cell::{self, CellKey, CellValue},
            keys,
            secret::{Memory, Secret, Store, reference, store_cell},
        },
    };
    use aterm_json::json;
    const FIRST_TOTP: &str = "JBSWY3DPEHPK3PXP";
    const SECOND_TOTP: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    let observers = aterm_tempfile::Builder::new()
        .prefix("registry-owner-")
        .tempdir_in("/tmp")
        .unwrap();
    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let port = reservation.port();
    for paths in &lab.paths {
        assert!(!paths.dir.join("commons/tables").exists());
    }
    // Both Macs run this build, so the lab starts at the generation that takes references,
    // declared before the owner runs.
    let table = Registry::at(lab.paths[0].clone())
        .open_newest(1_800_000_000_000)
        .unwrap();
    let (owner, owner_progress) = start(&lab, observers.path(), 0, port);
    let (receiver, receiver_progress) = start(&lab, observers.path(), 1, port);
    for runtime in [&owner, &receiver] {
        wait_network(
            &runtime.network_observation(),
            "owner active before any application",
            |s| s.visibility == Visibility::Active && s.observer.is_some(),
        );
    }
    let login = keys::address_pk("owner@example.invalid").unwrap();
    let mailbox = keys::mailbox_pk(&login).unwrap();
    let put = |collection: &crate::typed::Collection, row: &str, column: &str, value| {
        cell::change(
            collection,
            CellKey {
                pk: row,
                col: column,
                elem: None,
            },
            &CellValue::owner(1, value),
        )
        .unwrap()
    };
    let source = Registry::at(lab.paths[0].clone());
    let store = Memory::default();
    let secret = |collection: &str, row: &str, column: &str, value: &str| {
        store_cell(&store, collection, column, row, &Secret::new(value.into())).unwrap()
    };
    let password = secret("address", &login, "password", "fixture-initial-password");
    let totp = secret("mailbox", &mailbox, "totp", FIRST_TOTP);
    let totp_entry = crate::typed::secret::entry_of(&totp).unwrap().to_owned();
    Handle::at(lab.paths[0].clone(), &REGISTRY)
        .commit_sealed_until(true, crate::admission::deadline(), |_, _| {
            Ok((
                (),
                vec![vec![
                    put(&ADDRESS, &login, "addr", json!("owner@example.invalid")),
                    put(&ADDRESS, &login, "password", password),
                    put(&MAILBOX, &mailbox, "label", json!("fixture-mailbox")),
                    put(&MAILBOX, &mailbox, "service", json!("proton")),
                    put(&MAILBOX, &mailbox, "code_path", json!("webmail")),
                    put(&MAILBOX, &mailbox, "login", json!(login)),
                    put(&MAILBOX, &mailbox, "totp", totp),
                    put(
                        &MAILBOX,
                        &mailbox,
                        "webmail",
                        json!("https://mail.example.invalid/inbox"),
                    ),
                ]],
            ))
        })
        .unwrap();
    let first_count = source.view().unwrap().ops.len();
    receiver_progress.wait_for(
        table.digest(),
        first_count,
        "cold peer discovers registry and mailbox",
    );
    let remote = Registry::at(lab.paths[1].clone());
    let check = |registry: &Registry, machine: usize, expected: &str, expected_totp: &str| {
        let rows = mail::sign_in_mailboxes(registry).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].login, "owner@example.invalid");
        assert!(rows[0].held.contains(&Credential::Password));
        assert!(rows[0].held.contains(&Credential::Totp));
        let journal = Journal::at(&observers.path().join(format!("reveals-{machine}")));
        assert_eq!(
            mail::reveal_bound(
                registry,
                &store,
                &journal,
                &mailbox,
                &[Credential::Password, Credential::Totp],
                1_000,
                |found| found.login == "owner@example.invalid"
                    && found.service.as_deref() == Some("proton"),
                |values| values.len() == 2
                    && values[0].as_str() == Some(expected)
                    && values[1].as_str() == Some(expected_totp),
            )
            .unwrap(),
            Ok(true)
        );
    };
    check(&remote, 1, "fixture-initial-password", FIRST_TOTP);
    let journal = Journal::at(&observers.path().join("refused-bundles"));
    let consumed = std::cell::Cell::new(false);
    assert_eq!(
        mail::reveal_bound(
            &remote,
            &store,
            &journal,
            &mailbox,
            &[Credential::Password, Credential::Totp],
            1_000,
            |found| found.login == "another@example.invalid",
            |_| consumed.set(true),
        )
        .unwrap(),
        Err(Withheld::NoMailbox)
    );
    assert!(
        !consumed.get(),
        "changed binding exposed a credential bundle"
    );
    assert!(
        !observers
            .path()
            .join("refused-bundles/reveals.jsonl")
            .exists(),
        "binding was refused before a store entry was read"
    );
    assert_eq!(
        mail::reveal_bound(
            &remote,
            &store,
            &journal,
            &mailbox,
            &[Credential::Password, Credential::MailboxPassword],
            1_000,
            |found| found.login == "owner@example.invalid",
            |_| consumed.set(true),
        )
        .unwrap(),
        Err(Withheld::NotSet)
    );
    assert!(
        !consumed.get(),
        "a missing later part exposed an earlier credential"
    );
    // A reference whose entry this Mac's store lacks reveals nothing and journals nothing.
    store.remove(&totp_entry).unwrap();
    assert_eq!(
        mail::reveal_bound(
            &remote,
            &store,
            &journal,
            &mailbox,
            &[Credential::Password, Credential::Totp],
            1_000,
            |found| found.login == "owner@example.invalid",
            |_| consumed.set(true),
        )
        .unwrap(),
        Err(Withheld::NotInStore)
    );
    assert!(
        !consumed.get(),
        "a part the store lacks exposed an earlier credential"
    );
    assert!(
        !observers
            .path()
            .join("refused-bundles/reveals.jsonl")
            .exists(),
        "a reference the store lacks journals nothing"
    );
    // The peer's edit names new entries, so the reverse write changes each cell.
    let renamed = |entry: &str, value: &str| {
        store.put(entry, &Secret::new(value.into())).unwrap();
        reference(Memory::VAULT, entry)
    };
    let password = renamed(
        &crate::typed::secret::test_entry(("address", "password", &login), 2),
        "fixture-reverse-password",
    );
    let totp = renamed(
        &crate::typed::secret::test_entry(("mailbox", "totp", &mailbox), 2),
        SECOND_TOTP,
    );
    Handle::at(lab.paths[1].clone(), &REGISTRY)
        .commit_sealed_until(false, crate::admission::deadline(), |_, _| {
            Ok((
                (),
                vec![vec![
                    put(&ADDRESS, &login, "password", password),
                    put(&MAILBOX, &mailbox, "totp", totp),
                ]],
            ))
        })
        .unwrap();
    let second_count = remote.view().unwrap().ops.len();
    owner_progress.wait_for(
        table.digest(),
        second_count,
        "original owner receives the reverse write",
    );
    check(&source, 0, "fixture-reverse-password", SECOND_TOTP);
    owner.stop();
    receiver.stop();
    let inventory = |registry: &Registry| {
        registry
            .view()
            .unwrap()
            .ops
            .into_iter()
            .map(|o| o.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        inventory(&source),
        inventory(&remote),
        "bidirectional equality uses authenticated operation ids"
    );
    for (machine, registry) in [(0, &source), (1, &remote)] {
        check(registry, machine, "fixture-reverse-password", SECOND_TOTP);
    }

    let (owner, _) = start(&lab, observers.path(), 0, port);
    let (receiver, progress) = start(&lab, observers.path(), 1, port);
    source
        .write(vec![vec![put(
            &MAILBOX,
            &mailbox,
            "label",
            json!("changed-after-restart"),
        )]])
        .unwrap();
    progress.wait_for(
        table.digest(),
        source.view().unwrap().ops.len(),
        "restarted owners resume automatic repair",
    );
    receiver.stop();
    owner.stop();
    assert_eq!(inventory(&source), inventory(&remote));
    assert_eq!(
        mail::sign_in_mailboxes(&remote).unwrap()[0].label,
        "changed-after-restart"
    );
    check(&remote, 1, "fixture-reverse-password", SECOND_TOTP);
}

/// A lone peer satisfies the import guard through its rounds over other tables: two runtimes
/// share the catalog and one application table, and the importing Mac's latest round with its
/// one peer comes to be over the application table. Its network observation still holds the
/// latest round over the catalog, which exchanged and left nothing to bring, and the guard of a
/// first import (a registry no Mac here holds) reads it and holds nothing back.
///
/// A resting pair dials nothing without a move, so writes at the peer make the rounds: each is
/// walked to the importer over the application table, and the follow-up round after it takes
/// the peer's next regular turn, which alternates between the two tables both list. Within a
/// few writes, whichever table the first sweep ended on, the importer's latest round is over
/// the application table; the count of writes is the bound, not the time they take.
/// MUTANT (`KEPT_UNREAD`, the guard before a round over the catalog was kept): the same
/// observation makes the import wait, since no latest round is over the registry or the catalog.
#[test]
fn a_lone_peers_kept_catalog_round_lets_a_first_import_through_between_its_catalog_rounds() {
    use crate::alab::command::{KEPT_UNREAD, rounds_wait};
    let lab = crate::table::tests::Lab::new(&[Role::Writer; 2]);
    // Darwin limits Unix socket paths to 103 bytes, as above.
    let observers = aterm_tempfile::Builder::new()
        .prefix("commons-guard-")
        .tempdir_in("/tmp")
        .unwrap();
    let source = Proposals::at(lab.paths[0].clone());
    let table = source.initialize().unwrap();
    source
        .write([1; 16], vec![proposal("beside-the-catalog")])
        .unwrap();
    let catalog = crate::catalog::scope(lab.bootstrap(1).trust()).digest();
    let group = Ipv4Addr::new(239, 255, 31, 84);
    let reservation = Multicast::bind(Ipv4Addr::LOCALHOST, group, 0, LinkScope::Loopback).unwrap();
    let port = reservation.port();
    let (peer, _) = start(&lab, observers.path(), 0, port);
    let (importer, progress) = start(&lab, observers.path(), 1, port);
    progress.wait_for(table.digest(), 1, "the importing Mac took its peer's table");
    let latest = |state: &NetworkObservation| match &state.peers[..] {
        [peer] => peer.last_repair.clone(),
        _ => None,
    };
    let kept = |state: &NetworkObservation| {
        state
            .last_catalog_repair
            .as_ref()
            .is_some_and(|round| round.table == Some(catalog) && round.left_nothing())
    };
    let mut written = 0;
    let mut since_ms = 0;
    let state = loop {
        let state = wait_network(
            &importer.network_observation(),
            "a round with the peer after the last write, and its quiet round over the catalog kept",
            |state| {
                kept(state) && latest(state).is_some_and(|round| round.completed_at_ms >= since_ms)
            },
        );
        if latest(&state).is_some_and(|round| round.table.is_some_and(|table| table != catalog)) {
            break state;
        }
        written += 1;
        assert!(
            written <= 4,
            "after each of four writes the latest round was over the catalog: {state:?}"
        );
        since_ms = aterm_time::unix_millis().unwrap();
        // Each write is its own request.
        let request = [1 + u8::try_from(written).unwrap(); 16];
        source
            .write(request, vec![proposal(&format!("walked-{written}"))])
            .unwrap();
    };
    importer.stop();
    peer.stop();
    let now_ms = aterm_time::unix_millis().unwrap();
    // No Mac of this lab holds or declares this table: only the catalog can speak for it.
    let registry = Some([0x52; 32]);
    assert_eq!(
        rounds_wait(&state, registry, Some(catalog), now_ms),
        None,
        "{state:?}"
    );
    KEPT_UNREAD.with(|flag| flag.set(true));
    let unread = rounds_wait(&state, registry, Some(catalog), now_ms);
    KEPT_UNREAD.with(|flag| flag.set(false));
    assert!(
        unread.is_some(),
        "MUTANT: the latest round, over another table, says nothing of the registry"
    );
}

/// What `file` reads on the stand-in Mac `paths`: its state word, why, and how many rows.
fn shared_file(
    paths: &Paths,
    file: &crate::file::Type,
) -> (aterm_file::Word, Option<aterm_file::Why>, usize) {
    let read = aterm_file::File::at(&crate::file::root(paths), &file.file()).read();
    (read.status.word, read.status.why, read.rows().len())
}

/// The owner itself records the word of every state its custody refuses in: this test runs no
/// pass and no `refresh`. A runtime owner over a stand-in Mac materializes its registry
/// (`live`). An owner started with `[commons] enabled = false`, which never serves, records
/// `off`, and `live` again once sharing is back on. An owner started on a home an unfinished
/// succession fences, which never serves that home, records `held (succession)`. Each keeps the
/// rows the live pass wrote.
/// NEGATIVE CONTROL: with the materializer passing only while the owner serves (where it was,
/// in the serving owner's upkeep), the owner that starts with sharing off never records it:
/// `` timed out after 60s waiting for: the owner records `off` while sharing is off ``.
#[test]
fn the_owner_records_the_word_of_each_state_its_custody_refuses_in() {
    use aterm_file::{Why, Word};
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let paths = lab.paths[0].clone();
    let config = lab._home.path().join("aterm.toml");
    let registry = crate::alab::Registry::at(paths.clone());
    registry.open().unwrap();
    let machine = crate::typed::keys::machine_pk("m7");
    registry
        .write(vec![vec![
            crate::typed::grid::tests::put(
                &crate::alab::registry::MACHINE,
                &machine,
                "label",
                aterm_json::json!("m7"),
            ),
            crate::typed::grid::tests::put(
                &crate::alab::registry::MACHINE,
                &machine,
                "status",
                aterm_json::json!("active"),
            ),
        ]])
        .unwrap();
    let machines = &crate::file::MACHINES;
    assert_eq!(shared_file(&paths, machines), (Word::Never, None, 0));

    let reservation = Multicast::bind(
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(239, 255, 31, 84),
        0,
        LinkScope::Loopback,
    )
    .unwrap();
    let quick = crate::alab::upkeep::Cadence {
        first: Duration::from_millis(20),
        settle: Duration::from_millis(20),
        every: Duration::from_millis(100),
        busy: Duration::from_millis(50),
    };
    let owner = |paths: &Paths| {
        Runtime::start_on_with_reports(
            paths.clone(),
            config.clone(),
            |_| {},
            Network {
                isolated: Some(Attachment::new(reservation.port())),
                files: Some(quick),
                ..Network::default()
            },
            |_| {},
        )
        .unwrap()
    };
    let reads = |what: &str, expected: (Word, Option<Why>, usize)| {
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, what, || {
            (shared_file(&paths, machines) == expected).then_some(())
        });
    };

    let runtime = owner(&paths);
    reads("the owner materializes its registry", (Word::Live, None, 1));
    runtime.stop();

    // Sharing is off before the owner starts, so it never serves: the word is its
    // materializer's alone.
    aterm_secrets::fsx::write_atomic(&config, b"[commons]\nenabled=false\n", 0o600).unwrap();
    let runtime = owner(&paths);
    reads(
        "the owner records `off` while sharing is off",
        (Word::Off, Some(Why::Policy), 1),
    );
    std::fs::remove_file(&config).unwrap();
    reads("the owner records `live` again", (Word::Live, None, 1));
    runtime.stop();

    // A succession whose candidate nobody admits fences the home: the owner that starts on it
    // never serves it, and its materializer says so.
    crate::generation::file_seams::fence_pending(&paths, &lab.bootstrap(0));
    let runtime = owner(&paths);
    reads(
        "the owner records `held (succession)` for a fenced home",
        (Word::Held, Some(Why::Succession), 1),
    );
    runtime.stop();
}

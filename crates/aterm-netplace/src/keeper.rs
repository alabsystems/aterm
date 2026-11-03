// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KEEPER and THE READER. Exactly one long-lived process per Mac keeps the record: whichever
//! holds `<state>/netplace/keeper.lock` (the window, the node job, a vault job; [`start`]). It
//! takes the lock with a try only, never a wait, and a process that did not get it tries again
//! every [`RETRY`]. The keeper reads the facts, steps the place ([`crate::place::step`]) and
//! writes the record only when it changed, waking on a route change, a path change, or its own
//! timer: [`MOVED_EVERY`] while a move settles, [`CHECKING_EVERY`] while the place is checking
//! or back home with no member heard, [`STEADY_EVERY`] otherwise. Its thread runs at background
//! QoS and never on a window's main or render thread.
//!
//! Every reporter calls [`read`]: the record, the evidence and the facts now, stepped with the
//! same pure function, writing nothing. A reader whose keeper is gone, or whose record lags a
//! move, still says where this Mac is (`computed_by=reader`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::facts::Sense;
use crate::place::{Alab, Checking, ComputedBy, Facts, Place, Where, step};
use crate::record::{self, Written};

/// The keeper's lock file name.
pub const LOCK: &str = "keeper.lock";

/// How often a process that did not get the keeper's lock tries again.
pub const RETRY: Duration = Duration::from_secs(60);

/// The keeper's look while a move settles (the gateway's hardware address unread).
pub const MOVED_EVERY: Duration = Duration::from_secs(2);

/// The keeper's look while checking, or back on the ALab LAN with no member heard yet.
pub const CHECKING_EVERY: Duration = Duration::from_secs(10);

/// The keeper's look otherwise.
pub const STEADY_EVERY: Duration = Duration::from_secs(60);

/// How long a reader's cached place stands ([`read_cached`]).
pub const CACHE_FOR: Duration = Duration::from_secs(2);

/// How long the keeper waits before its next look after `place`.
#[must_use]
pub fn next_look(place: &Place) -> Duration {
    match place.at {
        Where::Checking(Checking::Moved) => MOVED_EVERY,
        Where::Checking(_) => CHECKING_EVERY,
        Where::AlabLan if place.reachable == 0 => CHECKING_EVERY,
        _ => STEADY_EVERY,
    }
}

/// One keeper step in `dir` at `now`: `held` (the record as the keeper last wrote it) brought up
/// to `facts` and the evidence, written when it changed. Returns the place and the record now
/// held.
fn keep_once(
    dir: &Path,
    held: &Written,
    facts: &Facts,
    now: u64,
    started_ms: u64,
) -> (Written, Place) {
    let evidence = crate::heard::read_for_place(dir);
    let book = crate::heard::read_book(dir);
    let (record, place) = step(
        &held.record,
        facts,
        &evidence,
        book.as_ref(),
        now,
        Some(started_ms),
    );
    let pid = std::process::id();
    let same_writer = held.keeper_pid == Some(pid) && held.keeper_started_ms == Some(started_ms);
    if record == held.record && same_writer {
        return (held.clone(), place);
    }
    let written = Written {
        revision: held.revision + 1,
        keeper_pid: Some(pid),
        keeper_started_ms: Some(started_ms),
        written_ms: now,
        record,
    };
    match record::write(dir, &written) {
        Ok(()) => (written, place),
        Err(e) => {
            aterm_log::warn!("network place: the record could not be written: {e}");
            (held.clone(), place)
        }
    }
}

/// The keeper's loop while it holds the lock, until `stop`.
fn keep(dir: &Path, sense: &dyn Sense, stop: &AtomicBool) {
    let started_ms = crate::unix_ms_now();
    let mut held = record::read(dir).unwrap_or_default();
    let mut said: Option<(Where, Alab)> = None;
    let mut away: Option<(Option<u64>, Option<crate::place::Kind>)> = None;
    aterm_log::info!("network place: this process keeps it ({})", dir.display());
    while !stop.load(Ordering::SeqCst) {
        let facts = sense.facts();
        let now = crate::unix_ms_now();
        let (next, place) = keep_once(dir, &held, &facts, now, started_ms);
        held = next;
        let now_said = (place.at, place.alab);
        if said != Some(now_said) {
            aterm_log::info!(
                "{}",
                crate::words::header(&place, &crate::words::Words::now())
            );
            said = Some(now_said);
        }
        if let Some((since, kind)) = returned(&mut away, &place) {
            aterm_log::info!("{}", crate::words::back_line(since, kind, now));
        }
        sense.wait(next_look(&place));
    }
}

/// Follows the ALab axis across the keeper's looks (`away`: since when and on what kind of
/// network this Mac was last away), answering where `place` brings it back onto the ALab LAN
/// after being away: once per return, for the one aterm.log line a Mac keeps of it.
fn returned(
    away: &mut Option<(Option<u64>, Option<crate::place::Kind>)>,
    place: &Place,
) -> Option<(Option<u64>, Option<crate::place::Kind>)> {
    match place.alab {
        Alab::Away { since_ms } => {
            let kind = match place.at {
                Where::Remote(kind) => Some(kind),
                _ => away.and_then(|(_, kind)| kind),
            };
            *away = Some((since_ms, kind));
            None
        }
        Alab::Here => away.take(),
        Alab::Learning | Alab::Unknown | Alab::NoFleet => None,
    }
}

/// Sets this thread's QoS class to background (macOS).
fn background() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: pthread_set_qos_class_self_np takes a class value and a relative priority and
        // changes only the calling thread's scheduling.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
        }
    }
}

/// Whether this process started a keeper candidate already.
static STARTED: AtomicBool = AtomicBool::new(false);

/// A keeper candidate's handle: dropping it does not stop the thread (a process's keeper lives
/// as long as the process); [`Candidate::stop`] does, at its next wake.
#[derive(Debug, Clone)]
pub struct Candidate(Arc<AtomicBool>);

impl Candidate {
    /// Asks the thread to end at its next wake, letting the lock go.
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// STARTS THIS PROCESS'S KEEPER CANDIDATE for the state root `state` (once per process; a
/// second call returns `None`): a background thread over the live facts that keeps the place
/// while it holds `keeper.lock` and tries for it every [`RETRY`] otherwise.
#[must_use]
pub fn start(state: &Path) -> Option<Candidate> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return None;
    }
    let candidate = spawn(
        record::dir(state),
        Box::new(|| Box::new(crate::facts::Live::start()) as Box<dyn Sense>),
    );
    if candidate.is_none() {
        STARTED.store(false, Ordering::SeqCst);
    }
    candidate
}

/// A keeper candidate over `dir` whose facts `sense` makes on its thread.
fn spawn(dir: PathBuf, sense: Box<dyn FnOnce() -> Box<dyn Sense> + Send>) -> Option<Candidate> {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("aterm-netplace-keeper".to_owned())
        .spawn(move || {
            background();
            let sense = sense();
            candidate_loop(&dir, &*sense, &stopped);
        });
    match spawned {
        Ok(_) => Some(Candidate(stop)),
        Err(e) => {
            aterm_log::warn!("network place: the keeper could not start: {e}");
            None
        }
    }
}

/// Tries for the lock, keeps while held, and tries again every [`RETRY`], until `stop`.
fn candidate_loop(dir: &Path, sense: &dyn Sense, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        let held = aterm_uds::private_file::ensure_private_dir(dir).and_then(|_| {
            aterm_uds::private_file::PrivateLock::take(
                dir,
                LOCK,
                Duration::ZERO,
                "another process keeps the network place",
            )
        });
        match held {
            Ok(lock) => {
                keep(dir, sense, stop);
                drop(lock);
            }
            Err(_) => {
                sense.wait(RETRY);
            }
        }
    }
}

/// Whether the record's keeper is alive: its pid runs.
fn keeper_alive(written: &Written) -> bool {
    written
        .keeper_pid
        .is_some_and(aterm_uds::process::pid_alive)
}

/// THE READER: the place in `state`'s record, the evidence and `facts` (the route read now; the
/// path monitor's reading where this process runs one, else the record's last while its keeper
/// is alive and the record's current stay continues the route read now), at `now`, written
/// nowhere. A record whose keeper is gone, or whose stay this route does not continue, lends no
/// path reading: the window that wrote "expensive" on a phone hotspot and quit says nothing of
/// the network this Mac is on now. `computed_by` is the keeper's where its keeper is alive and
/// the record already holds this stay.
#[must_use]
pub fn read_with(state: &Path, facts: Facts, now: u64) -> Place {
    let dir = record::dir(state);
    let written = record::read(&dir).unwrap_or_default();
    let mut facts = facts;
    if facts.path.is_none()
        && keeper_alive(&written)
        && written
            .record
            .current()
            .is_some_and(|stay| stay.continues(&facts))
    {
        facts.path = written.record.path;
    }
    let evidence = crate::heard::read_for_place(&dir);
    let book = crate::heard::read_book(&dir);
    let (_, mut place) = step(&written.record, &facts, &evidence, book.as_ref(), now, None);
    let held_stay = written.record.current().map(|stay| stay.start_ms);
    place.computed_by =
        if keeper_alive(&written) && held_stay.is_some() && held_stay == place.since_ms {
            ComputedBy::Keeper
        } else {
            ComputedBy::Reader
        };
    place
}

/// [`read_with`] over the route read now and the record's path reading (the scripted facts
/// where they are scripted, [`crate::facts::reader_facts`]), at this clock.
#[must_use]
pub fn read(state: &Path) -> Place {
    read_with(state, crate::facts::reader_facts(), crate::unix_ms_now())
}

/// The last [`read`] per state root, with when it was made.
static CACHE: Mutex<Option<(PathBuf, std::time::Instant, Place)>> = Mutex::new(None);

/// [`read`], cached for [`CACHE_FOR`]: what `status` reads on every call.
#[must_use]
pub fn read_cached(state: &Path) -> Place {
    {
        let cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((root, at, place)) = cache.as_ref()
            && root == state
            && at.elapsed() < CACHE_FOR
        {
            return place.clone();
        }
    }
    let place = read(state);
    *CACHE.lock().unwrap_or_else(PoisonError::into_inner) = Some((
        state.to_path_buf(),
        std::time::Instant::now(),
        place.clone(),
    ));
    place
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Scripted;
    use crate::place::{Lane, Link, PathFacts, RouteFacts};
    use std::net::Ipv4Addr;

    fn facts(hw: u8) -> Facts {
        Facts {
            route: Some(RouteFacts {
                interface: "en0".to_owned(),
                address: Ipv4Addr::new(192, 168, 4, 31),
                prefix: 22,
                gateway: Some(Ipv4Addr::new(192, 168, 4, 1)),
                gateway_hw: Some(vec![0, 1, 2, 3, 4, hw]),
            }),
            tunnel: None,
            path: Some(PathFacts {
                usable: Some(true),
                expensive: false,
                constrained: false,
                link: Link::Wifi,
            }),
        }
    }

    /// Waits up to the one-minute hang detector for `done`, polling the record.
    fn until(dir: &Path, done: impl Fn(&Written) -> bool) -> Written {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(written) = record::read(dir)
                && done(&written)
            {
                return written;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the keeper never wrote"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Polls `done` up to the one-minute hang detector.
    fn until_true(what: &str, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !done() {
            assert!(
                std::time::Instant::now() < deadline,
                "{what} never happened"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// One keeper writes and a second candidate in the same state root writes nothing: once the
    /// second has tried for the lock and gone to wait, the record still names the first, and a
    /// scripted move reaches the record through the first. When the first stops and lets its
    /// lock go, the second takes over from the first's record.
    /// NEGATIVE CONTROL: keeping without the lock (`Err(_) => keep(dir, sense, stop)` in
    /// `candidate_loop`) makes the second write its own record before it waits, and "the second
    /// wrote nothing" fails.
    #[test]
    fn one_keeper_writes_and_a_second_waits_and_takes_over() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = record::dir(root.path());
        let first_facts = Arc::new(Scripted::new(facts(5)));
        let sense: Arc<Scripted> = Arc::clone(&first_facts);
        let first = spawn(
            dir.clone(),
            Box::new(move || Box::new(sense) as Box<dyn Sense>),
        )
        .expect("the first");
        let written = until(&dir, |w| w.record.current().is_some());
        let first_start = written.keeper_started_ms;
        let second_facts = Arc::new(Scripted::new(facts(5)));
        let sense: Arc<Scripted> = Arc::clone(&second_facts);
        let second = spawn(
            dir.clone(),
            Box::new(move || Box::new(sense) as Box<dyn Sense>),
        )
        .expect("the second");
        until_true("the second's wait", || second_facts.waits() >= 1);
        assert_eq!(
            record::read(&dir).and_then(|w| w.keeper_started_ms),
            first_start,
            "the second wrote nothing"
        );
        first_facts.set(facts(9));
        let moved = until(&dir, |w| w.record.stays.len() == 2);
        assert_eq!(
            moved.keeper_started_ms, first_start,
            "the first wrote the move"
        );
        first.stop();
        first_facts.set(facts(9));
        // The first lets its lock go: taking it here (with the hang detector's wait) proves it.
        let released =
            aterm_uds::private_file::PrivateLock::take(&dir, LOCK, Duration::from_secs(60), "held")
                .expect("the first let its lock go");
        drop(released);
        second_facts.set(facts(7));
        let taken = until(&dir, |w| w.keeper_started_ms != first_start);
        assert_eq!(
            taken.record.stays.len(),
            3,
            "the second keeps from the first's record"
        );
        second.stop();
        second_facts.set(facts(7));
    }

    /// The keeper keeps one return line a return: away on the hotspot (a check between keeps what
    /// it knew), then back on the ALab LAN, once; home again says nothing more.
    /// NEGATIVE CONTROL: `returned` answering from a copy (`away.take()` → `*away`, never taken)
    /// says the return again at the next look home, and "once a return" fails.
    #[test]
    fn a_return_is_said_once() {
        let mut away = None;
        let hotspot = crate::sample::hotspot(1_000, 900);
        let home = crate::sample::home(2_000, 3);
        let checking = Place {
            at: Where::Checking(Checking::Moved),
            alab: Alab::Unknown,
            ..home.clone()
        };
        assert_eq!(returned(&mut away, &home), None, "home from the start");
        assert_eq!(returned(&mut away, &hotspot), None);
        assert_eq!(returned(&mut away, &checking), None);
        let back = returned(&mut away, &home).expect("the return");
        assert_eq!(back.0, Some(900));
        assert_eq!(
            back.1,
            Some(crate::place::Kind::Hotspot { by_range: false })
        );
        assert_eq!(returned(&mut away, &home), None, "once a return");
    }

    /// A reader whose keeper is gone works the place out from the record and the facts now,
    /// writing nothing, and says so; with its keeper alive and the stay held it says keeper.
    /// NEGATIVE CONTROL: saying `keeper` whatever the record's keeper (`if true || …` in
    /// `read_with`) fails the dead keeper's `Reader`.
    #[test]
    fn a_reader_works_the_place_out_and_writes_nothing() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = record::dir(root.path());
        let now = 1_800_000_000_000;
        let (record, _) = step(
            &crate::place::Record::default(),
            &facts(5),
            &Default::default(),
            None,
            now,
            Some(now),
        );
        let dead = Written {
            revision: 3,
            keeper_pid: Some(u32::MAX - 1),
            keeper_started_ms: Some(now),
            written_ms: now,
            record,
        };
        record::write(&dir, &dead).expect("the record");
        let place = read_with(root.path(), facts(5), now + 1_000);
        assert_eq!(place.computed_by, ComputedBy::Reader);
        assert_eq!(place.since_ms, Some(now), "the record's stay");
        assert_eq!(record::read(&dir), Some(dead.clone()), "nothing written");
        let alive = Written {
            keeper_pid: Some(std::process::id()),
            ..dead
        };
        record::write(&dir, &alive).expect("the record");
        assert_eq!(
            read_with(root.path(), facts(5), now + 1_000).computed_by,
            ComputedBy::Keeper
        );
        let moved = read_with(root.path(), facts(9), now + 2_000);
        assert_eq!(
            moved.computed_by,
            ComputedBy::Reader,
            "the record lags this move"
        );
        crate::heard::note_answer(&dir, (Lane::Fleet, "m1"), None, (None, true), (now, now));
    }

    /// A reader that runs no path monitor takes the record's path reading only from a live
    /// keeper whose stay the route continues: the window quit on a phone hotspot (its record says
    /// expensive), and at home a command reads home's route with no path of its own; that is no
    /// hotspot, whatever the record's last reading.
    /// NEGATIVE CONTROL: lending the record's path whatever its keeper and stay (the first
    /// `if facts.path.is_none() { facts.path = written.record.path; }`) reads the home route as
    /// the hotspot, and "a dead keeper's hotspot flag says nothing here" fails.
    #[test]
    fn a_reader_takes_no_stale_path_reading() {
        let root = aterm_tempfile::tempdir().expect("a scratch directory");
        let dir = record::dir(root.path());
        let now = 1_800_000_000_000;
        let hotspot = Facts {
            route: Some(RouteFacts {
                interface: "en0".to_owned(),
                address: Ipv4Addr::new(172, 20, 10, 2),
                prefix: 28,
                gateway: Some(Ipv4Addr::new(172, 20, 10, 1)),
                gateway_hw: Some(vec![2, 0, 0, 0, 0, 1]),
            }),
            tunnel: None,
            path: Some(PathFacts {
                usable: Some(true),
                expensive: true,
                constrained: false,
                link: Link::Wifi,
            }),
        };
        let (record, place) = step(
            &crate::place::Record::default(),
            &hotspot,
            &Default::default(),
            None,
            now,
            Some(now),
        );
        assert!(matches!(place.at, Where::Remote(_)), "PRECONDITION");
        let quit = Written {
            revision: 1,
            keeper_pid: Some(u32::MAX - 1),
            keeper_started_ms: Some(now),
            written_ms: now,
            record,
        };
        record::write(&dir, &quit).expect("the record");
        let home_route = Facts {
            path: None,
            ..facts(5)
        };
        let place = read_with(root.path(), home_route.clone(), now + 60_000);
        assert!(
            !matches!(place.at, Where::Remote(_)),
            "a dead keeper's hotspot flag says nothing here: {:?}",
            place.at
        );
        assert_eq!(place.path, None);
        let alive = Written {
            keeper_pid: Some(std::process::id()),
            ..quit
        };
        record::write(&dir, &alive).expect("the record");
        let place = read_with(root.path(), home_route, now + 60_000);
        assert!(
            !matches!(place.at, Where::Remote(_)),
            "a live keeper's stay this route does not continue lends nothing: {:?}",
            place.at
        );
        let hotspot_route = Facts {
            path: None,
            ..hotspot
        };
        let place = read_with(root.path(), hotspot_route, now + 60_000);
        assert_eq!(
            place.path.map(|path| path.expensive),
            Some(true),
            "a live keeper's own stay lends its reading"
        );
    }
}

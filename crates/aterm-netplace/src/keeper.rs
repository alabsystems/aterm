// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KEEPER and THE READER. Exactly one long-lived process per Mac keeps the record: whichever
//! holds its keeper lease (the window, the node job, a vault job; [`start`]):
//! `<state>/.ubd/netplace-keeper.lock` on macOS, `<state>/netplace/keeper.lock` elsewhere. A
//! process that did not get it waits for it in the kernel
//! ([`aterm_uds::private_file::PrivateLock::wait_for`]): it spends nothing while another keeps
//! the place and takes over the moment that keeper ends (ATERM-KI-1105-20). The keeper reads the facts, steps the place ([`crate::place::step`]) and
//! writes the record only when it changed, waking on a route change, a path change, or its own
//! semantic deadline or its bounded fact timer: [`MOVED_EVERY`] while a move settles, [`CHECKING_EVERY`] while the place is checking
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

use crate::facts::{Sense, StopWake};
#[cfg(test)]
use crate::place::step;
use crate::place::{
    Alab, Checking, Facts, Place, Where, step_owned, step_with_deadline, step_with_deadline_owned,
};
use crate::record::{self, Written};

/// The keeper's lock file name.
pub const LOCK: &str = "keeper.lock";

/// The keeper's look while a move settles (the gateway's hardware address unread).
pub const MOVED_EVERY: Duration = Duration::from_secs(2);

/// The keeper's look while checking, or back on the ALab LAN with no member heard yet.
pub const CHECKING_EVERY: Duration = Duration::from_secs(10);

/// The keeper's look otherwise.
pub const STEADY_EVERY: Duration = Duration::from_secs(60);

/// How long a reporter's cached observation or [`crate::routes::paced_now`] result stands.
pub const CACHE_FOR: Duration = Duration::from_secs(2);

/// How long the keeper waits before its next look after `place`.
#[must_use]
pub fn next_look(place: &Place) -> Duration {
    match place.at {
        Where::Checking(Checking::Moved) => MOVED_EVERY,
        Where::Checking(_) => CHECKING_EVERY,
        Where::AlabLan if place.heard == 0 => CHECKING_EVERY,
        _ => STEADY_EVERY,
    }
}

struct Kept {
    written: Written,
    place: Place,
    next_change_ms: Option<u64>,
}

/// Bounds semantic time by the cadence for facts that have no complete invalidation source.
/// `now` is sampled after publication and logging, immediately before waiting.
fn next_wait(place: &Place, next_change_ms: Option<u64>, now: u64) -> Duration {
    let cap = next_look(place);
    next_change_ms.map_or(cap, |at| {
        Duration::from_millis(at.saturating_sub(now)).min(cap)
    })
}

/// One keeper step in `dir` at `now`: `held` (the record as the keeper last wrote it) brought up
/// to `facts` and the evidence, written when it changed. Returns the place and the record now
/// held.
fn keep_once(dir: &Path, held: Written, facts: &Facts, now: u64, started_ms: u64) -> Kept {
    let (evidence, book) = match aterm_uds::private_file::JudgedDir::judge(dir) {
        Ok(judged) => (
            crate::heard::read_for_place_in(dir, &judged),
            crate::heard::read_book_in(&judged),
        ),
        Err(_) => (
            crate::heard::read_for_place(dir),
            crate::heard::read_book(dir),
        ),
    };
    let mut projection = step_with_deadline(
        &held.record,
        facts,
        &evidence,
        book.as_ref(),
        now,
        Some(started_ms),
    );
    // A deadline at this same instant asks for normalization, not a short timer. Feed the
    // returned record back through the fixed facts before publishing the stable result.
    while projection.next_change_ms == Some(now) {
        projection = step_with_deadline_owned(
            projection.record,
            facts,
            &evidence,
            book.as_ref(),
            now,
            Some(started_ms),
        );
    }
    let crate::place::Projection {
        record,
        place,
        next_change_ms,
    } = projection;
    let pid = std::process::id();
    let same_writer = held.keeper_pid == Some(pid) && held.keeper_started_ms == Some(started_ms);
    if record == held.record && same_writer {
        return Kept {
            written: held,
            place,
            next_change_ms,
        };
    }
    let written = Written {
        revision: held.revision + 1,
        keeper_pid: Some(pid),
        keeper_started_ms: Some(started_ms),
        written_ms: now,
        record,
    };
    match record::write(dir, &written) {
        Ok(()) => Kept {
            written,
            place,
            next_change_ms,
        },
        Err(e) => {
            aterm_log::warn!("network place: the record could not be written: {e}");
            // The prospective record is not durable. Retry on the positive fact cadence;
            // its speculative deadline must neither promote it nor cause a zero-wait loop.
            Kept {
                written: held,
                place,
                next_change_ms: None,
            }
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
        let Kept {
            written,
            place,
            next_change_ms,
        } = keep_once(dir, held, &facts, now, started_ms);
        held = written;
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
        sense.wait(
            next_wait(&place, next_change_ms, crate::unix_ms_now()),
            stop,
        );
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
/// as long as the process); [`Candidate::stop`] wakes active fact waits. A candidate blocked
/// on another keeper's lock still waits for the actual holder to release it.
#[derive(Debug, Clone)]
pub struct Candidate(Arc<StopState>);

#[derive(Debug, Default)]
struct StopState {
    stopped: AtomicBool,
    wake: Mutex<Option<StopWake>>,
}

impl StopState {
    fn install(&self, wake: StopWake) {
        let mut held = self.wake.lock().unwrap_or_else(PoisonError::into_inner);
        *held = Some(wake);
        if self.stopped.load(Ordering::SeqCst) {
            held.as_ref().unwrap().ring();
        }
    }
}

impl Candidate {
    /// Asks the thread to end and wakes its fact wait, letting its lock go. A blocked lock
    /// acquisition is not cancelled; after the holder releases it, this candidate lets it go.
    pub fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
        if let Some(wake) = self
            .0
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            wake.ring();
        }
    }
}

/// STARTS THIS PROCESS'S KEEPER CANDIDATE for the state root `state` (once per process; a
/// second call returns `None`): a background thread over the live facts that keeps the place
/// while it holds `keeper.lock` and otherwise waits for it.
#[must_use]
pub fn start(state: &Path) -> Option<Candidate> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return None;
    }
    let candidate = match crate::facts::Live::prepare() {
        Ok(prepared) => spawn_mode(
            state.join(record::DIR),
            Box::new(move || Box::new(prepared.start()) as Box<dyn Sense>),
            cfg!(target_os = "macos"),
        )
        .map(|(candidate, _worker)| candidate),
        Err(error) => {
            aterm_log::warn!("network place: the keeper cannot receive stop: {error}");
            None
        }
    };
    if candidate.is_none() {
        STARTED.store(false, Ordering::SeqCst);
    }
    candidate
}

/// A keeper candidate over `dir` whose facts `sense` makes on its thread.
#[cfg(test)]
fn spawn(dir: PathBuf, sense: Box<dyn FnOnce() -> Box<dyn Sense> + Send>) -> Option<Candidate> {
    spawn_owned(dir, sense).map(|(candidate, _worker)| candidate)
}

#[cfg(test)]
fn spawn_owned(
    dir: PathBuf,
    sense: Box<dyn FnOnce() -> Box<dyn Sense> + Send>,
) -> Option<(Candidate, std::thread::JoinHandle<()>)> {
    spawn_mode(dir, sense, false)
}

fn spawn_mode(
    dir: PathBuf,
    sense: Box<dyn FnOnce() -> Box<dyn Sense> + Send>,
    quiet: bool,
) -> Option<(Candidate, std::thread::JoinHandle<()>)> {
    let stop = Arc::new(StopState::default());
    let stopped = Arc::clone(&stop);
    let spawned = std::thread::Builder::new()
        .name("aterm-netplace-keeper".to_owned())
        .spawn(move || {
            background();
            let sense = sense();
            let wake = match sense.stop_wake() {
                Ok(wake) => wake,
                Err(error) => {
                    aterm_log::warn!("network place: the keeper cannot receive stop: {error}");
                    return;
                }
            };
            stopped.install(wake);
            if quiet {
                quiet_candidate_loop(&dir, &*sense, &stopped.stopped);
            } else {
                candidate_loop(&dir, &*sense, &stopped.stopped);
            }
        });
    match spawned {
        Ok(worker) => Some((Candidate(stop), worker)),
        Err(e) => {
            aterm_log::warn!("network place: the keeper could not start: {e}");
            None
        }
    }
}

/// How long a candidate whose lock could not be opened or waited for (its folder not private, a
/// failed `flock`) waits before it tries again: no wait on another keeper, which blocks instead.
const REFUSED_AGAIN: Duration = Duration::from_secs(60);

fn prepare_quiet(dir: &Path) -> std::io::Result<PathBuf> {
    use aterm_uds::private_file::PrivateLock;
    aterm_uds::quiet_state::prepare(dir, || {
        Ok((
            PrivateLock::take(dir, LOCK, Duration::ZERO, "legacy keeper is running")?,
            PrivateLock::take(
                dir,
                crate::heard::LOCK,
                Duration::ZERO,
                "evidence writer is running",
            )?,
            PrivateLock::take(
                dir,
                crate::heard::DAYS_LOCK,
                Duration::ZERO,
                "home-days writer is running",
            )?,
        ))
    })?;
    let selected = aterm_uds::quiet_state::selected(dir);
    if selected != dir {
        match aterm_uds::private_file::open_private(&selected, LOCK) {
            Ok(_) => std::fs::remove_file(selected.join(LOCK))?,
            Err(aterm_uds::private_file::PrivateFileError::Missing) => {}
            Err(error) => return Err(std::io::Error::other(error)),
        }
    }
    Ok(selected)
}

fn quiet_candidate_loop(legacy: &Path, sense: &dyn Sense, stop: &AtomicBool) {
    // New keepers use a distinct lease outside the exchanged directory. An old binary
    // already waiting on its old keeper inode can wake after migration, but the fence
    // refuses its writes and that old inode cannot block a new keeper.
    let quiet = aterm_uds::quiet_state::destination(legacy);
    let Some(parent) = quiet.parent() else {
        return;
    };
    while !stop.load(Ordering::SeqCst) {
        let held = aterm_uds::private_file::create_private_dir(parent).and_then(|_| {
            aterm_uds::private_file::PrivateLock::wait_for(parent, "netplace-keeper.lock", || {
                sense.lock_held()
            })
        });
        let result = held.and_then(|_lease| {
            let dir = prepare_quiet(legacy)?;
            keep(&dir, sense, stop);
            Ok(())
        });
        if let Err(error) = result {
            aterm_log::warn!("network place: quiet keeper is refused: {error}");
            sense.wait(REFUSED_AGAIN, stop);
        }
    }
}

/// Waits for the lock, keeps while held, until `stop`. A candidate stopped while it waits lets the
/// lock go as soon as it gets it.
fn candidate_loop(dir: &Path, sense: &dyn Sense, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        let held = aterm_uds::private_file::ensure_private_dir(dir).and_then(|_| {
            aterm_uds::private_file::PrivateLock::wait_for(dir, LOCK, || sense.lock_held())
        });
        match held {
            Ok(lock) => {
                keep(dir, sense, stop);
                drop(lock);
            }
            Err(e) => {
                aterm_log::warn!("network place: the keeper's lock is refused: {e}");
                sense.wait(REFUSED_AGAIN, stop);
            }
        }
    }
}

/// THE READER: recomputes the place from historical memory, current `facts` and evidence at
/// `now`, writing nothing. Only the caller's facts supply a current path reading. A recorded
/// writer PID says nothing about whether its keeper thread or monitor still owns resources.
/// This computation is always attributed to the reader, even when it continues a saved stay.
#[must_use]
pub fn read_with(state: &Path, facts: Facts, now: u64) -> Place {
    let dir = record::dir(state);
    let judged = aterm_uds::private_file::JudgedDir::judge(&dir).ok();
    let written = judged
        .as_ref()
        .and_then(record::read_in)
        .unwrap_or_default();
    let (evidence, book) = match judged.as_ref() {
        Some(judged) => (
            crate::heard::read_for_place_in(&dir, judged),
            crate::heard::read_book_in(judged),
        ),
        None => (
            crate::heard::read_for_place(&dir),
            crate::heard::read_book(&dir),
        ),
    };
    step_owned(written.record, &facts, &evidence, book.as_ref(), now, None).1
}

/// [`read_with`] over current reader facts (or their script), at this clock. A reader running
/// no path monitor reports no path reading.
#[must_use]
pub fn read(state: &Path) -> Place {
    read_with(state, crate::facts::reader_facts(), crate::unix_ms_now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn quiet_handoff_preserves_records_and_excludes_every_legacy_writer() {
        use aterm_uds::private_file::PrivateLock;
        use std::os::unix::fs::MetadataExt as _;
        let root = aterm_tempfile::tempdir().unwrap();
        let legacy = root.path().join(record::DIR);
        let written = Written {
            revision: 41,
            ..Written::default()
        };
        record::write(&legacy, &written).unwrap();
        let before = std::fs::metadata(&legacy).unwrap().ino();
        for name in [LOCK, crate::heard::LOCK, crate::heard::DAYS_LOCK] {
            let lease = PrivateLock::take(&legacy, name, Duration::ZERO, "held").unwrap();
            assert!(
                prepare_quiet(&legacy).is_err(),
                "{name} must exclude migration"
            );
            assert_eq!(record::read(&legacy).unwrap(), written);
            drop(lease);
        }
        let quiet = prepare_quiet(&legacy).unwrap();
        assert!(aterm_tempfile::is_quiet(&quiet));
        assert_eq!(record::dir(root.path()), quiet);
        assert_eq!(std::fs::metadata(&quiet).unwrap().ino(), before);
        assert_eq!(record::read(&quiet).unwrap(), written);
        assert!(
            !quiet.join(LOCK).exists(),
            "the old keeper lease is obsolete"
        );
        assert!(
            record::write(&legacy, &written).is_err(),
            "an old keeper cannot fork history"
        );
        assert_eq!(prepare_quiet(&legacy).unwrap(), quiet);
    }
    use crate::facts::Scripted;
    use crate::place::{ComputedBy, Lane, Link, PathFacts, RouteFacts};
    use std::net::Ipv4Addr;

    fn observed(done: impl Fn() -> bool) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while !done() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        done()
    }

    fn join_stopped(worker: std::thread::JoinHandle<()>, rescue: impl FnOnce()) {
        let automatic = observed(|| worker.is_finished());
        if !automatic {
            rescue();
        }
        worker.join().unwrap();
        assert!(automatic, "the stopped candidate did not finish");
    }

    #[test]
    fn stopping_the_active_candidate_ends_it_without_changing_facts() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let facts = Arc::new(Scripted::new(facts(5)));
        let worker_facts = Arc::clone(&facts);
        let (candidate, worker) =
            spawn_owned(dir.clone(), Box::new(move || Box::new(worker_facts))).unwrap();
        let waited = observed(|| facts.waits() > 0);
        let held = record::read(&dir);
        // Only the worker retains the state while the last external handle is gone.
        let weak = Arc::downgrade(&candidate.0);
        drop(candidate);
        let candidate = weak.upgrade().map(Candidate);
        let was_stopped = candidate
            .as_ref()
            .is_none_or(|candidate| candidate.0.stopped.load(Ordering::SeqCst));
        if let Some(candidate) = candidate {
            candidate.stop();
        }
        join_stopped(worker, || facts.set(self::facts(5)));
        assert!(waited, "the actual fact wait began");
        assert!(
            !was_stopped,
            "dropping the last external handle does not stop the keeper"
        );
        assert_eq!(record::read(&dir), held);
        assert_eq!(facts.facts(), self::facts(5));
        let lock = aterm_uds::private_file::PrivateLock::take(
            &dir,
            LOCK,
            Duration::ZERO,
            "keeper still owns it",
        )
        .unwrap();
        drop(lock);
    }

    #[test]
    fn a_joined_keeper_in_the_live_process_lends_no_current_path_or_attribution() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let mut current = facts(5);
        current.path.as_mut().unwrap().expensive = true;
        let sensor = Arc::new(Scripted::new(current.clone()));
        let worker_sensor = Arc::clone(&sensor);
        let (candidate, worker) =
            spawn_owned(dir.clone(), Box::new(move || Box::new(worker_sensor))).unwrap();
        let waited = observed(|| sensor.waits() > 0);
        candidate.stop();
        join_stopped(worker, || sensor.set(current.clone()));
        assert!(waited, "the actual keeper reached its fact wait");
        let written = record::read(&dir).unwrap();
        assert_eq!(written.keeper_pid, Some(std::process::id()));
        let bytes = std::fs::read(dir.join(record::FILE)).unwrap();
        let now = crate::unix_ms_now();
        let no_monitor = Facts {
            path: None,
            ..current.clone()
        };
        let read = read_with(root.path(), no_monitor, now);
        assert_eq!(
            read.path, None,
            "a positively joined keeper has no current monitor reading: {read:?}"
        );
        assert_eq!(read.computed_by, ComputedBy::Reader);
        assert!(!matches!(
            read.at,
            Where::Remote(crate::place::Kind::Hotspot { .. })
        ));
        let monitored = read_with(root.path(), current, now);
        assert!(monitored.path.unwrap().expensive);
        assert!(matches!(
            monitored.at,
            Where::Remote(crate::place::Kind::Hotspot { by_range: false })
        ));
        assert_eq!(monitored.computed_by, ComputedBy::Reader);
        assert_eq!(std::fs::read(dir.join(record::FILE)).unwrap(), bytes);
    }

    #[test]
    fn stop_before_wake_install_is_latched_without_a_fact_change() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let facts = Arc::new(Scripted::new(facts(5)));
        let worker_facts = Arc::clone(&facts);
        let worker_dir = dir.clone();
        let (release, gate) = std::sync::mpsc::channel();
        let (created, receiving) = std::sync::mpsc::channel();
        let creator = std::thread::spawn(move || {
            let pair = spawn_owned(
                worker_dir,
                Box::new(move || {
                    gate.recv().unwrap();
                    Box::new(worker_facts)
                }),
            )
            .unwrap();
            created.send(pair).unwrap();
        });
        let pair = receiving.recv_timeout(Duration::from_secs(60));
        if let Ok((candidate, _)) = &pair {
            candidate.stop();
        }
        release.send(()).unwrap();
        creator.join().unwrap();
        let (candidate, worker) = match pair {
            Ok(pair) => pair,
            Err(error) => {
                let (candidate, worker) = receiving.recv_timeout(Duration::from_secs(60)).unwrap();
                candidate.stop();
                facts.set(self::facts(5));
                join_stopped(worker, || facts.set(self::facts(5)));
                panic!("candidate creation waited for its fact factory: {error}");
            }
        };
        join_stopped(worker, || facts.set(self::facts(5)));
        assert!(candidate.0.stopped.load(Ordering::SeqCst));
        assert!(record::read(&dir).is_none());
        assert_eq!(facts.facts(), self::facts(5));
    }

    #[test]
    fn a_stopped_lock_waiter_only_releases_after_the_actual_holder() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        aterm_uds::private_file::ensure_private_dir(&dir).unwrap();
        let lock =
            aterm_uds::private_file::PrivateLock::take(&dir, LOCK, Duration::ZERO, "fixture held")
                .unwrap();
        let facts = Arc::new(Scripted::new(facts(5)));
        let worker_facts = Arc::clone(&facts);
        let (candidate, worker) =
            spawn_owned(dir.clone(), Box::new(move || Box::new(worker_facts))).unwrap();
        let contended = observed(|| facts.lock_waits() == 1);
        candidate.stop();
        let before = record::read(&dir);
        drop(lock);
        join_stopped(worker, || facts.set(self::facts(5)));
        assert!(contended, "actual lock contention was observed");
        assert!(before.is_none());
        assert!(
            record::read(&dir).is_none(),
            "stopped acquisition writes nothing"
        );
        assert_eq!(facts.waits(), 0);
    }

    #[test]
    #[ignore = "manual keeper allocation measurement, not a timing gate"]
    fn keeper_unchanged_cost() {
        use crate::place::{Known, NetworkId, Record, Stay, StayAt};
        let mut facts = facts(5);
        facts.path.as_mut().unwrap().expensive = true;
        let route = facts.route.as_ref().unwrap();
        for count in [1, 16, crate::place::STAYS_MAX] {
            let root = aterm_tempfile::tempdir().unwrap();
            let dir = record::dir(root.path());
            let seed = Written {
                revision: 7,
                keeper_pid: Some(std::process::id()),
                keeper_started_ms: Some(1),
                written_ms: 100_000,
                record: Record {
                    stays: (0..count)
                        .map(|index| Stay {
                            start_ms: (index as u64 + 1) * 1000,
                            end_ms: (index + 1 < count).then_some((index as u64 + 2) * 1000),
                            at: StayAt::Remote,
                            network: if index + 1 == count {
                                NetworkId::of(route)
                            } else {
                                NetworkId::parse(&format!("n{:016x}", 100 + index))
                            },
                            subnet: Some("192.168.4.0/22".to_owned()),
                            gateway: route.gateway,
                            from_start: false,
                        })
                        .collect(),
                    remembered: (0..crate::place::REMEMBERED_MAX)
                        .map(|index| Known {
                            network: NetworkId::parse(&format!("n{index:016x}")).unwrap(),
                            learned_ms: 1,
                            confirmed_ms: 2,
                        })
                        .collect(),
                },
            };
            let held = keep_once(&dir, seed, &facts, 100_000, 1).written;
            assert_eq!(held.record.stays.len(), count);
            record::write(&dir, &held).unwrap();
            let bytes = std::fs::read(dir.join(record::FILE)).unwrap();
            let inputs = vec![held.clone(); 64];
            let (_, allocations) = aterm_heap_count::allocations_in(|| {
                for input in inputs {
                    let next = keep_once(std::hint::black_box(&dir), input, &facts, 100_000, 1);
                    assert_eq!(next.written, held);
                    std::hint::black_box(next);
                }
            });
            eprintln!("stays={count} remembered=16 rounds=64 allocations={allocations}");
            assert_eq!(std::fs::read(dir.join(record::FILE)).unwrap(), bytes);
        }
    }

    fn folding_history(dir: &Path, count: usize, now: u64) -> (Facts, Written) {
        use crate::place::{Known, NetworkId};
        let mut facts = facts(5);
        facts.path.as_mut().unwrap().expensive = true;
        let mut held = keep_once(dir, Written::default(), &facts, now, now).written;
        let stay = held.record.stays[0].clone();
        held.record.stays = (0..count)
            .map(|index| crate::place::Stay {
                start_ms: now + index as u64,
                end_ms: (index + 1 < count).then_some(now + index as u64 + 1),
                ..stay.clone()
            })
            .collect();
        held.record.remembered = (0..crate::place::REMEMBERED_MAX)
            .map(|index| Known {
                network: NetworkId::parse(&format!("n{index:016x}")).unwrap(),
                learned_ms: 1,
                confirmed_ms: 2,
            })
            .collect();
        record::write(dir, &held).unwrap();
        (facts, held)
    }

    #[test]
    #[ignore = "manual normalization allocation measurement, not a timing gate"]
    fn keeper_normalization_cost() {
        let now = 100_000;
        for count in [1, 2, 16, crate::place::STAYS_MAX] {
            let root = aterm_tempfile::tempdir().unwrap();
            let dir = record::dir(root.path());
            let (facts, held) = folding_history(&dir, count, now);
            let expected = keep_once(&dir, held.clone(), &facts, now + 64, now);
            assert_eq!(expected.written.record.stays.len(), 1);
            assert_eq!(
                expected.written.revision,
                held.revision + u64::from(count > 1)
            );
            let bytes = std::fs::read(dir.join(record::FILE)).unwrap();
            let inputs = vec![held; 64];
            let (_, allocations) = aterm_heap_count::allocations_in(|| {
                for input in inputs {
                    let next = keep_once(std::hint::black_box(&dir), input, &facts, now + 64, now);
                    assert_eq!(next.written, expected.written);
                    assert_eq!(next.place, expected.place);
                    assert_eq!(next.next_change_ms, expected.next_change_ms);
                    std::hint::black_box(next);
                }
            });
            eprintln!("folding_stays={count} remembered=16 rounds=64 allocations={allocations}");
            assert_eq!(std::fs::read(dir.join(record::FILE)).unwrap(), bytes);
        }
    }

    #[test]
    fn failed_normalization_retains_every_durable_stay_until_recovery() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let now = 100_000;
        let (facts, held) = folding_history(&dir, 8, now);
        let durable = held.clone();
        let storage = held.record.stays.as_ptr();
        std::fs::remove_file(dir.join(record::FILE)).unwrap();
        std::fs::create_dir(dir.join(record::FILE)).unwrap();
        let failed = keep_once(&dir, held, &facts, now + 64, now);
        assert_eq!(failed.written, durable);
        assert_eq!(failed.written.record.stays.as_ptr(), storage);
        assert_eq!(failed.next_change_ms, None);
        assert!(!next_wait(&failed.place, failed.next_change_ms, now + 64).is_zero());
        std::fs::remove_dir(dir.join(record::FILE)).unwrap();
        let recovered = keep_once(&dir, failed.written, &facts, now + 65, now);
        assert_eq!(recovered.written.record.stays.len(), 1);
        assert_eq!(recovered.written.revision, durable.revision + 1);
        assert_eq!(record::read(&dir).unwrap(), recovered.written);
    }

    #[test]
    fn internet_expiry_schedules_the_actual_keeper_before_its_fact_cap() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let now = 10_000_000;
        let facts = facts(5);
        let first = keep_once(&dir, Written::default(), &facts, now, now);
        crate::heard::note_internet(
            &dir,
            true,
            crate::place::InternetSource::ApiProbe,
            "fixture",
            now,
        );
        let last = now + crate::place::INTERNET_FRESH_MS;
        let before = keep_once(&dir, first.written, &facts, last, now);
        assert_eq!(
            before.place.internet.source,
            crate::place::InternetSource::ApiProbe
        );
        assert_eq!(
            next_wait(&before.place, before.next_change_ms, last),
            Duration::from_millis(1)
        );
        // Publication/logging time is already spent when the wait is chosen.
        assert_eq!(
            next_wait(&before.place, before.next_change_ms, last + 1),
            Duration::ZERO
        );
        let after = keep_once(&dir, before.written, &facts, last + 1, now);
        assert_eq!(
            after.place.internet.source,
            crate::place::InternetSource::Path
        );
        let cap = next_look(&after.place);
        assert_eq!(next_wait(&after.place, None, last), cap);
        assert_eq!(next_wait(&after.place, Some(u64::MAX), last), cap);
        assert_eq!(next_wait(&after.place, Some(last + 1), 0), cap);
    }

    #[test]
    fn same_time_folds_are_normalized_before_one_durable_publication() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let now = 10_000_000;
        let facts = facts(5);
        let mut held = keep_once(&dir, Written::default(), &facts, now, now).written;
        let stay = held.record.stays[0].clone();
        held.record.stays = (0..8)
            .map(|i| {
                let mut row = stay.clone();
                row.start_ms = now - 8 + i;
                row.end_ms = (i < 7).then_some(now - 7 + i);
                row
            })
            .collect();
        record::write(&dir, &held).unwrap();
        assert_eq!(
            step_with_deadline(
                &held.record,
                &facts,
                &Default::default(),
                None,
                now,
                Some(now)
            )
            .next_change_ms,
            Some(now)
        );
        let kept = keep_once(&dir, held.clone(), &facts, now, now);
        assert_eq!(kept.written.record.stays.len(), 1);
        assert_eq!(kept.written.record.stays[0].start_ms, now - 8);
        assert_eq!(kept.written.revision, held.revision + 1);
        assert_ne!(kept.next_change_ms, Some(now));
        assert_eq!(record::read(&dir).unwrap(), kept.written);
    }

    #[test]
    fn confirmation_deadline_persists_a_record_change_without_a_place_change() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let now = 10_000_000;
        let facts = facts(5);
        let route = facts.route.as_ref().unwrap();
        let network = crate::place::NetworkId::of(route).unwrap();
        let mut held = keep_once(&dir, Written::default(), &facts, now, now).written;
        held.record.remembered.push(crate::place::Known {
            network: network.clone(),
            learned_ms: now,
            confirmed_ms: now,
        });
        record::write(&dir, &held).unwrap();
        crate::heard::note_answers(
            &dir,
            vec![crate::place::Answer {
                lane: Lane::Fleet,
                member: crate::place::member_key(Lane::Fleet, "fixture"),
                addr: Some(std::net::IpAddr::V4(route.address)),
                via: Some(route.address),
                iface: Some(route.interface.clone()),
                network: Some(network),
                logged: true,
                began_ms: now,
                at_ms: now,
            }],
        );
        let due = now + crate::place::CONFIRM_EVERY_MS;
        let before = keep_once(&dir, held.clone(), &facts, due - 1, now);
        assert_eq!(
            next_wait(&before.place, before.next_change_ms, due - 1),
            Duration::from_millis(1)
        );
        let at = keep_once(&dir, before.written.clone(), &facts, due, now);
        assert_eq!(at.place, before.place);
        assert_eq!(at.written.record.remembered[0].confirmed_ms, due);
        assert_eq!(at.written.revision, before.written.revision + 1);
        assert_eq!(record::read(&dir).unwrap(), at.written);
    }

    #[test]
    fn failed_publication_keeps_the_old_record_and_a_positive_retry() {
        let root = aterm_tempfile::tempdir().unwrap();
        let dir = record::dir(root.path());
        let now = 10_000_000;
        let held = keep_once(&dir, Written::default(), &facts(5), now, now).written;
        std::fs::remove_file(dir.join(record::FILE)).unwrap();
        std::fs::create_dir(dir.join(record::FILE)).unwrap();
        let supplied = held.clone();
        let storage = supplied.record.stays.as_ptr();
        let failed = keep_once(&dir, supplied, &facts(6), now + 1, now);
        assert_eq!(failed.written, held);
        assert_eq!(
            failed.written.record.stays.as_ptr(),
            storage,
            "a refused write retains the owned durable history"
        );
        assert_eq!(failed.next_change_ms, None);
        assert_eq!(
            next_wait(&failed.place, failed.next_change_ms, now + 1),
            next_look(&failed.place)
        );
        assert!(!next_wait(&failed.place, failed.next_change_ms, now + 1).is_zero());
        std::fs::remove_dir(dir.join(record::FILE)).unwrap();
        let recovered = keep_once(&dir, failed.written, &facts(6), now + 2, now);
        assert_eq!(recovered.written.revision, held.revision + 1);
        assert_ne!(recovered.written.record, held.record);
        assert_eq!(record::read(&dir).unwrap(), recovered.written);
    }

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
    /// second has found the lock held and gone to wait for it, the record still names the first,
    /// and a scripted move reaches the record through the first. When the first stops and lets
    /// its lock go, the second, which never looked again on a clock, takes over from the first's
    /// record at once (within the one-minute hang detector, which a retry every minute would
    /// not meet from a wait begun before the release).
    /// NEGATIVE CONTROL: keeping without the lock (`|| { keep(dir, sense, stop); sense.lock_held() }`
    /// as `wait_for`'s hook in `candidate_loop`) makes the second write its own record before it
    /// waits, and "the second wrote nothing" fails; going back to a try and a sleep of a minute
    /// (`PrivateLock::take` with `Duration::ZERO`, `sense.wait(REFUSED_AGAIN)` on its refusal)
    /// counts no lock wait, and "the second's wait" fails.
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
        until_true("the second's wait", || second_facts.lock_waits() >= 1);
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
        second_facts.set(facts(7));
        let taken = until(&dir, |w| w.keeper_started_ms != first_start);
        assert_eq!(
            taken.record.stays.len(),
            3,
            "the second keeps from the first's record"
        );
        assert_eq!(
            second_facts.lock_waits(),
            1,
            "the second waited once, on no clock"
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

    /// Writer metadata never changes attribution of the reader's own computation. Historical
    /// stays remain available, and neither a live PID nor a changed route authorizes a write.
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
            ComputedBy::Reader
        );
        let moved = read_with(root.path(), facts(9), now + 2_000);
        assert_eq!(
            moved.computed_by,
            ComputedBy::Reader,
            "the record lags this move"
        );
        crate::heard::note_answer(&dir, (Lane::Fleet, "m1"), None, (None, true), (now, now));
    }
}

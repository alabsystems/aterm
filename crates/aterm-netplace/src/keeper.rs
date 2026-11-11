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
        let held = aterm_uds::quiet_state::prepare_private_parent(parent).and_then(|_| {
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
#[path = "keeper_tests.rs"]
mod tests;

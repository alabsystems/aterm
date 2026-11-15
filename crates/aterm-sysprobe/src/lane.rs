// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A reader the kernel may hold, run on a long-lived thread of its own and waited for a bounded
//! time: a lane.
//!
//! A read of another process (the process table, one process's record, its arguments) can sit in
//! the kernel for as long as something else takes: on 2026-11-06 a `posix_spawn` that never
//! finished held every `KERN_PROC_ALL` read on the machine for 13 minutes, and a process's
//! arguments cannot be read while it is held mid-start (leftover M29). A caller that makes such a
//! read on its own thread stops with it. A lane makes it on its worker instead, one call at a
//! time, and the caller waits at most its bound:
//!
//! - **One worker, one call.** The worker starts at the lane's first ask and lives as long as the
//!   lane; no thread is started per call. At most one call is ever outstanding, so a read the
//!   kernel never answers holds one thread, however many callers follow.
//! - **Stalled, named.** A call past its asker's bound is *overdue*: every later ask reports the
//!   lane [`Read::Stalled`] at once, without waiting, and so does every ask already waiting for
//!   it, naming the kernel call the reader was in ([`mark`]) and since when. A reader that asks
//!   another lane is in that lane's call while it waits, and is named by it. A call still within
//!   its asker's bound is another caller's ordinary read: a later caller waits for it (within its
//!   own bound), then asks its own.
//! - **Ended at the next ask.** When an overdue call returns, its late answer is dropped (it
//!   stands for an instant long past), and the next ask reads afresh, its answer naming the
//!   episode it ends ([`Read::Answered`]'s `ended`).
//! - **Never joined.** A worker is never waited for: at a quit, one stuck in the kernel is left
//!   there.

use std::cell::OnceCell;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

/// Where a lane's reader is: the kernel call it is in, and about which process.
#[derive(Debug, Default)]
struct Crumb {
    at: Mutex<Spot>,
}

/// A reader's place ([`Crumb`]).
#[derive(Clone, Debug, Default)]
struct Spot {
    /// The kernel call it last [`mark`]ed.
    call: Option<&'static str>,
    /// The process that call reads.
    pid: Option<u32>,
    /// The lane it is waiting on, by name, and where that lane's reader is: set while it asks
    /// another lane ([`Lane::ask_within`]).
    waiting_on: Option<(&'static str, Arc<Crumb>)>,
}

/// How many lanes deep a reader's wait is followed ([`Crumb::read_crumb`]).
const NESTED_MAX: usize = 4;

impl Crumb {
    fn read_spot(&self) -> Spot {
        self.at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn put_spot(&self, spot: Spot) {
        *self.at.lock().unwrap_or_else(PoisonError::into_inner) = spot;
    }

    fn leave_crumb(&self, call: Option<&'static str>, pid: Option<u32>) {
        self.put_spot(Spot {
            call,
            pid,
            waiting_on: None,
        });
    }

    /// The call this reader is held in: its own, or, while it waits on another lane, the call
    /// that lane's reader is in (that lane's name, where its reader marked none yet).
    fn read_crumb(&self) -> (Option<&'static str>, Option<u32>) {
        let mut spot = self.read_spot();
        for _ in 0..NESTED_MAX {
            let Some((lane, inner)) = spot.waiting_on.clone() else {
                break;
            };
            let next = inner.read_spot();
            if next.call.is_none() && next.waiting_on.is_none() {
                return (Some(lane), None);
            }
            spot = next;
        }
        (spot.call, spot.pid)
    }
}

/// While a lane's worker asks another lane, its crumb names that lane's reader; dropped, the
/// worker's own place is put back.
struct Waiting {
    crumb: Arc<Crumb>,
    was: Spot,
}

impl Waiting {
    /// This thread, when it is a lane's worker, waiting on lane `lane` whose reader's place is
    /// `inner`.
    fn on(lane: &'static str, inner: &Arc<Crumb>) -> Option<Self> {
        let crumb = CRUMB.with(|crumb| crumb.get().cloned())?;
        let was = crumb.read_spot();
        crumb.put_spot(Spot {
            waiting_on: Some((lane, Arc::clone(inner))),
            ..was.clone()
        });
        Some(Self { crumb, was })
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.crumb.put_spot(self.was.clone());
    }
}

thread_local! {
    /// The crumb of the lane whose worker this thread is; unset on every other thread.
    static CRUMB: OnceCell<Arc<Crumb>> = const { OnceCell::new() };
}

/// Notes, before a kernel call that may be held, which call it is and which process it reads:
/// what a lane names when that call stalls. On a thread that is no lane's worker it does nothing.
pub fn mark(call: &'static str, pid: Option<u32>) {
    CRUMB.with(|crumb| {
        if let Some(crumb) = crumb.get() {
            crumb.leave_crumb(Some(call), pid);
        }
    });
}

/// A lane whose reader is still in the kernel past its asker's bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stall {
    /// The lane's name (`procs`, `kproc`).
    pub lane: &'static str,
    /// The kernel call its reader last [`mark`]ed (`proc_pid_rusage`); the lane's own name where
    /// its reader marked none.
    pub call: &'static str,
    /// The process that call reads, where it reads one.
    pub pid: Option<u32>,
    /// When the stalled call was asked, unix ms.
    pub since_unix_ms: i64,
    /// How long it has been in the kernel, ms.
    pub for_ms: u64,
}

/// What one ask of a lane read.
#[derive(Debug)]
pub enum Read<O> {
    /// The reader answered.
    Answered {
        /// Its answer.
        out: O,
        /// When the read began.
        at: Instant,
        /// How long it took.
        took: Duration,
        /// The stall this answer ends: an overdue call that returned since the last ask.
        ended: Option<Stall>,
    },
    /// The reader is in the kernel past a bound: this ask read nothing.
    Stalled(Stall),
    /// The lane cannot read: its worker did not start, or the read panicked.
    Off(&'static str),
}

impl<O> Read<O> {
    /// The answer, where the reader gave one.
    #[must_use]
    pub fn answer(self) -> Option<O> {
        match self {
            Self::Answered { out, .. } => Some(out),
            Self::Stalled(_) | Self::Off(_) => None,
        }
    }
}

/// The call outstanding.
#[derive(Clone, Copy, Debug)]
struct Running {
    ticket: u64,
    asked_at: Instant,
    asked_unix_ms: i64,
    /// Its asker's bound passed: every later ask reports the lane stalled at once.
    overdue: bool,
}

/// An answer a caller has not taken yet.
struct Answer<O> {
    ticket: u64,
    out: O,
    at: Instant,
    took: Duration,
}

struct State<O> {
    running: Option<Running>,
    answers: Vec<Answer<O>>,
    /// An overdue call that returned, not yet reported ended.
    ended: Option<Stall>,
    next_ticket: u64,
}

struct LaneCore<O> {
    name: &'static str,
    state: Mutex<State<O>>,
    changed: Condvar,
    crumb: Arc<Crumb>,
    /// How many times an ask parked behind another caller's call (a test's).
    #[cfg(test)]
    parked: AtomicU32,
}

impl<O> LaneCore<O> {
    fn lock(&self) -> MutexGuard<'_, State<O>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stall_of(&self, r: &Running) -> Stall {
        let (call, pid) = self.crumb.read_crumb();
        Stall {
            lane: self.name,
            call: call.unwrap_or(self.name),
            pid,
            since_unix_ms: r.asked_unix_ms,
            for_ms: u64::try_from(r.asked_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

/// What the registry asks of a lane: its stall now, if any.
trait Health: Send + Sync {
    fn stall(&self) -> Option<Stall>;
}

impl<O: Send> Health for LaneCore<O> {
    fn stall(&self) -> Option<Stall> {
        let st = self.lock();
        st.running.filter(|r| r.overdue).map(|r| self.stall_of(&r))
    }
}

/// Every registered lane ([`Lane::registered`]).
static REGISTRY: Mutex<Vec<Weak<dyn Health>>> = Mutex::new(Vec::new());

/// Every registered lane stalled now: the process-wide readers' (the table, a process's record,
/// its arguments), which no one caller owns.
#[must_use]
pub fn stalls() -> Vec<Stall> {
    let live: Vec<Arc<dyn Health>> = {
        let mut lanes = REGISTRY.lock().unwrap_or_else(PoisonError::into_inner);
        lanes.retain(|lane| lane.strong_count() > 0);
        lanes.iter().filter_map(Weak::upgrade).collect()
    };
    live.iter().filter_map(|lane| lane.stall()).collect()
}

/// A reader run on its own worker, waited for a bounded time (module docs).
pub struct Lane<I, O> {
    name: &'static str,
    read: fn(I) -> O,
    registered: bool,
    lane_core: OnceLock<Arc<LaneCore<O>>>,
    /// The worker's queue, held by the lane alone: a dropped lane's worker ends at its next
    /// wait (one in the kernel, once it returns).
    asks: Mutex<Option<SyncSender<(u64, I)>>>,
    /// The workers this lane has started.
    workers: AtomicU32,
}

impl<I: Send + 'static, O: Send + 'static> Lane<I, O> {
    /// A lane named `name` whose worker runs `read`.
    #[must_use]
    pub const fn new(name: &'static str, read: fn(I) -> O) -> Self {
        Self {
            name,
            read,
            registered: false,
            lane_core: OnceLock::new(),
            asks: Mutex::new(None),
            workers: AtomicU32::new(0),
        }
    }

    /// This lane, listed by [`stalls`] while it is stalled: a process-wide reader no one caller
    /// owns, whose stall each reading names.
    #[must_use]
    pub const fn registered(mut self) -> Self {
        self.registered = true;
        self
    }

    fn lane_core(&self) -> &Arc<LaneCore<O>> {
        self.lane_core.get_or_init(|| {
            let lane_core = Arc::new(LaneCore {
                name: self.name,
                state: Mutex::new(State {
                    running: None,
                    answers: Vec::new(),
                    ended: None,
                    next_ticket: 0,
                }),
                changed: Condvar::new(),
                crumb: Arc::new(Crumb::default()),
                #[cfg(test)]
                parked: AtomicU32::new(0),
            });
            if self.registered {
                let health: Arc<dyn Health> = lane_core.clone();
                REGISTRY
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(Arc::downgrade(&health));
            }
            lane_core
        })
    }

    /// The lane's stall now: an overdue call still in the kernel.
    #[must_use]
    pub fn stall(&self) -> Option<Stall> {
        self.lane_core
            .get()
            .and_then(|lane_core| Health::stall(&**lane_core))
    }

    /// How many workers this lane has started (one, however many asks; a test's).
    #[cfg(test)]
    pub(crate) fn workers(&self) -> u32 {
        self.workers.load(Ordering::Relaxed)
    }

    /// How many times an ask parked behind another caller's call (a test's).
    #[cfg(test)]
    pub(crate) fn parked(&self) -> u32 {
        self.lane_core
            .get()
            .map_or(0, |lane_core| lane_core.parked.load(Ordering::SeqCst))
    }

    /// Asks the reader to read `input` and waits up to `within` for it (module docs).
    pub fn ask_within(&self, input: I, within: Duration) -> Read<O> {
        let deadline = Instant::now().checked_add(within);
        let lane_core = self.lane_core();
        // A lane's worker asking this lane is held in this lane's call while it waits.
        let _waiting = Waiting::on(self.name, &lane_core.crumb);
        let asks = match self.queue(lane_core) {
            Ok(asks) => asks,
            Err(why) => return Read::Off(why),
        };
        let mut st = lane_core.lock();
        // Another caller's call: an overdue one stalls this ask at once; one within its bound is
        // waited for, within this ask's own bound.
        while let Some(r) = st.running {
            if r.overdue {
                return Read::Stalled(lane_core.stall_of(&r));
            }
            let left = deadline.map_or(Duration::ZERO, |d| {
                d.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                return Read::Stalled(lane_core.stall_of(&r));
            }
            #[cfg(test)]
            lane_core.parked.fetch_add(1, Ordering::SeqCst);
            st = lane_core
                .changed
                .wait_timeout(st, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        let ended = st.ended.take();
        st.next_ticket += 1;
        let ticket = st.next_ticket;
        lane_core.crumb.leave_crumb(None, None);
        let asked_at = Instant::now();
        st.running = Some(Running {
            ticket,
            asked_at,
            asked_unix_ms: unix_ms_now(),
            overdue: false,
        });
        // Sent with no lock held: the worker takes its answer's place under the state's lock.
        drop(st);
        let sent = asks.try_send((ticket, input)).is_ok();
        let mut st = lane_core.lock();
        if !sent {
            if st.running.is_some_and(|r| r.ticket == ticket) {
                st.running = None;
            }
            st.ended = st.ended.or(ended);
            drop(st);
            *self.asks.lock().unwrap_or_else(PoisonError::into_inner) = None;
            return Read::Off("the lane's worker ended");
        }
        loop {
            if let Some(i) = st.answers.iter().position(|a| a.ticket == ticket) {
                let a = st.answers.swap_remove(i);
                return Read::Answered {
                    out: a.out,
                    at: a.at,
                    took: a.took,
                    ended,
                };
            }
            let Some(r) = st.running.filter(|r| r.ticket == ticket) else {
                st.ended = st.ended.or(ended);
                return Read::Off("the read panicked");
            };
            let left = deadline.map_or(Duration::ZERO, |d| {
                d.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                if let Some(r) = st.running.as_mut() {
                    r.overdue = true;
                }
                st.ended = st.ended.or(ended);
                drop(st);
                // Every ask waiting for this call stalls now, not at its own bound.
                lane_core.changed.notify_all();
                return Read::Stalled(lane_core.stall_of(&r));
            }
            st = lane_core
                .changed
                .wait_timeout(st, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The worker's queue, its worker started at the lane's first ask. No other lock is held
    /// while it is taken.
    fn queue(&self, lane_core: &Arc<LaneCore<O>>) -> Result<SyncSender<(u64, I)>, &'static str> {
        let held = self
            .asks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(asks) = held {
            return Ok(asks);
        }
        let started = self.spawn_lane_worker(lane_core)?;
        let mut queue = self.asks.lock().unwrap_or_else(PoisonError::into_inner);
        // Two first asks may race: the later one's worker ends as its queue drops.
        Ok(queue.get_or_insert(started).clone())
    }

    fn spawn_lane_worker(
        &self,
        lane_core: &Arc<LaneCore<O>>,
    ) -> Result<SyncSender<(u64, I)>, &'static str> {
        self.workers.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = Arc::clone(lane_core);
        let read = self.read;
        std::thread::Builder::new()
            .name(format!("lane-{}", self.name))
            .spawn(move || work(&worker, &rx, read))
            .map(|_| tx)
            .map_err(|_| "the lane's worker did not start")
    }
}

/// The worker: each ask's read, its answer left for its asker; past its asker's bound, dropped,
/// and the episode left for the next ask to report ended.
fn work<I, O>(lane_core: &LaneCore<O>, asks: &Receiver<(u64, I)>, read: fn(I) -> O) {
    CRUMB.with(|crumb| {
        let _ = crumb.set(Arc::clone(&lane_core.crumb));
    });
    while let Ok((ticket, input)) = asks.recv() {
        let at = Instant::now();
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| read(input)));
        let took = at.elapsed();
        let mut st = lane_core.lock();
        let Some(r) = st.running.filter(|r| r.ticket == ticket) else {
            continue;
        };
        st.running = None;
        if r.overdue {
            st.ended = Some(Stall {
                for_ms: u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
                ..lane_core.stall_of(&r)
            });
        } else if let Ok(out) = out {
            st.answers.push(Answer {
                ticket,
                out,
                at,
                took,
            });
        }
        drop(st);
        lane_core.changed.notify_all();
    }
}

fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[path = "lane_tests.rs"]
mod tests;

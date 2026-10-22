// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DETERMINISTIC FABRIC SIMULATION, stage 1: one REAL bridge, every door
//! of it ([`crate::fx`]) answered by a seeded world, so an interleaving is a
//! seed and a failing seed replays exactly.
//!
//! ## What is real and what is simulated
//!
//! REAL: the whole bridge — [`crate::bridge::Bridge`]'s constructor, its
//! `start` and every `pass` of its loop, every handler, the state dir's code
//! ([`crate::state::StateDir`], on a scratch directory), astream's client
//! (`astream_broker::Client`, speaking astream's own wire through
//! `astream_broker::proto`), the mailbox, the pipeline, the reader rule
//! ([`crate::bridge::reader_took`]).
//!
//! SIMULATED, each behind its door:
//! * the CLOCKS: a virtual monotonic clock and wall clock ([`Clock`]), moved
//!   only by the bridge's own parks ([`Process::idle`]) and the scheduler;
//! * the BROKER ([`bus`]): an in-memory model of astream's broker, served over
//!   astream's wire, whose every restart is held to astream's power-loss law
//!   P2-P7 (`astream-broker` `crash_sim.rs`; [`bus::law_check`]) — among the
//!   cuts it draws a lost suffix of unescalated acked records under Ordered
//!   or Relaxed, announced by a generation move with lineage. Not every
//!   outcome the law allows is drawn, nor every clause checked ([`bus`]'s
//!   module doc): P1 holds by construction, P2 is checked as a prefix (not at
//!   a batch boundary), P8's rot — refused, or cut with a move — is not
//!   modelled, and a weak tier's generation move after a crash off the boot
//!   volume is never drawn; and in half the worlds it answers some acks
//!   after the records they name are readable ([`bus`]'s held acks), the
//!   order the bridge's non-blocking loop (aterm-0) must survive;
//! * the STATE DIR'S DEVICE ([`device`]): the state dir's writes go to a
//!   scratch directory and are journalled, and a power cut rewrites the
//!   directory to an image aterm-durable's contract allows (a durable write
//!   and a flushed directory entry kept, an atomic-only write kept or lost,
//!   whole or — POSIX's worst case, which the committed device draws — TORN:
//!   finding F1, which the bridge now survives by setting a torn file aside;
//!   an unflushed unlink kept or lost: finding F2, a landed post's in-flight
//!   entry brought back and republished behind its sender's later posts,
//!   which the bridge now closes by retiring an earlier boot's entries) —
//!   and, in every other eight seeds, a device that FAILS WRITES (sim item
//!   S7): `EIO` and `ENOSPC` on a write, a rename, a flush or an unlink, a
//!   durable replace renamed with its flush failed, an append cut part-way,
//!   a disk full for spells, until the settle phase's clean disk;
//! * ATERM'S ENDPOINT ([`endpoint`]): a model of the verb lane's bridge plane
//!   (`deliver` with its `off=` dedup, `outbox` and its retirements, receipts,
//!   `hold`, `link log` with lane 3's `continues=`, the roster, a session's
//!   `status` and `meta`, and the bus mirror's `log-take` and `log-landed`
//!   over REAL session logs, `aterm_slog::Log`), served on a socket pair by a
//!   thread that only ever answers the bridge's own requests;
//! * the PUSH LANE: aterm's `EVENT` lines are queued into the bridge's mailbox
//!   by the scheduler itself;
//! * a SIBLING under the node id, in worlds of its own: a copy of the state
//!   dir's bridge, played by the scheduler (or, racing an attach, by the
//!   broker inside one of the node's will requests) as its rows on the bus
//!   under the node's producer and the copy's mark — its attach, its
//!   session's row, its will — so the bridge's ghost sweep meets another
//!   state dir's rows, at an incarnation this dir took too in the racing world
//!   (finding F4: a ghost row whose writer's successors all died inside the
//!   sweep's patience stood for good, which the bridge closes by reading the
//!   state dir's mark on the row, and never a sibling's);
//! * the ENTROPY: the back-off's jitter is the seed's.
//!
//! ## Why it is deterministic
//!
//! One thread runs the scheduler and the bridge; the bridge never blocks on a
//! door (a park moves the virtual clock instead). The endpoint thread answers
//! only a request the bridge has just written and is blocked on, so its every
//! action is a rendezvous. No reader thread exists: a subscription is pumped
//! into the mailbox by the scheduler ([`Core::pump`]), when the seed says its
//! reader ran. Every choice — what arrives, when the broker restarts and how,
//! where a kill lands, which image a power cut leaves — is drawn from the
//! seed's stream, and the trace ([`Core::trace`]) records each one, so two runs
//! of a seed are compared trace for trace ([`world`]'s replay test).
//!
//! ## Kills land at the explicit operation hooks
//!
//! Each bus request, each verb-lane request and each state-dir write passes
//! [`Process::step`] before and after it. The scheduler arms a countdown, and
//! the step it reaches kills the process there: it is marked dead (its later
//! writes are refused, its connections end and their wills fire) and the
//! bridge's stack is unwound by a payload no panic hook sees
//! ([`std::panic::resume_unwind`], [`SimKill`]). That is a `kill -9` at that
//! hook: a request written and not answered, or a reply read and not acted
//! on. State-dir hooks surround a complete logical mutation; they do not
//! inject a kill between its actual rename and directory flush. This is an
//! operation-boundary model, not equivalence to every kernel SIGKILL point.

pub(crate) mod bus;
pub(crate) mod conform;
#[cfg(test)]
mod conform_broker;
pub(crate) mod device;
pub(crate) mod endpoint;
pub(crate) mod world;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::fx::{BusCloser, Write};
use crate::mailbox::{Mailbox, Source};
use crate::transport::Conn;

/// Lock a mutex of the simulation, through a poison: a kill unwinds through
/// the bridge, never while one of these is held, but a failing assertion may.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The payload a simulated kill unwinds the bridge with ([`Process::step`]).
#[derive(Debug)]
pub(crate) struct SimKill;

/// The seed's stream: splitmix64, so a seed names one sequence of draws on
/// every machine and build.
#[derive(Clone, Debug)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A draw in `0..n` (`0` when `n` is zero).
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }

    /// `true` with probability `num / den`.
    pub(crate) fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    /// One element of `items`, or `None` for none.
    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        let i = usize::try_from(self.below(items.len() as u64)).unwrap_or(0);
        items.get(i)
    }

    /// An independent stream for one purpose, so adding a draw to one purpose
    /// does not shift every other's.
    pub(crate) fn fork(&mut self, salt: u64) -> Rng {
        Rng(self.next_u64() ^ salt.wrapping_mul(0xd6e8_feb8_6659_fd93))
    }
}

/// The world's clocks: a monotonic one over a fixed base instant and a wall
/// one over a fixed epoch, both moved only by [`Clock::advance`].
#[derive(Debug)]
struct Clock {
    base: Instant,
    elapsed: Duration,
    /// The wall clock at `base`: a fixed moment, so `t=` stamps are the seed's.
    wall0_ms: u64,
}

/// Where the scheduler armed the next kill, and where the last one landed.
#[derive(Debug, Default)]
struct KillPlan {
    /// `(pid, steps left)`: the process dies at the step that takes this to 0.
    armed: Option<(u64, u64)>,
}

/// One subscription the bridge handed its door ([`crate::fx::Fx::start_reader`]),
/// pumped by the scheduler ([`Core::pump`]) as its reader thread would read it.
struct Reader {
    pid: u64,
    conn: u64,
    source: Source,
    generation: u64,
    sub: astream_broker::AnySubscription,
    closer: Arc<BusCloser>,
    mailbox: Arc<Mailbox>,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("pid", &self.pid)
            .field("conn", &self.conn)
            .field("source", &self.source)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// THE WORLD every door of every simulated process answers from.
#[derive(Debug)]
pub(crate) struct Core {
    clock: Mutex<Clock>,
    /// The bridge's own draws (its back-off's jitter), apart from the
    /// scheduler's, so the two never shift each other.
    draws: Mutex<Rng>,
    pub(crate) broker: bus::Broker,
    pub(crate) device: device::Device,
    kill: Mutex<KillPlan>,
    readers: Mutex<Vec<Reader>>,
    trace: Mutex<Vec<String>>,
    /// The bridge rules this world switched off ([`crate::fx::Fx::mutant`]).
    pub(crate) mutants: Mutex<BTreeSet<world::Mutant>>,
    /// What the broker held unanswered for a process when it PARKED with
    /// nothing queued ([`Process::idle`]): `(offset, subject, body)` of each
    /// record whose ack was still in its batch's flush. The oracle's idle law
    /// reads and empties it after every step.
    pub(crate) idle_held: Mutex<Vec<(u64, String, Vec<u8>)>>,
}

impl Core {
    pub(crate) fn new(seed: u64, broker: bus::Broker, device: device::Device) -> Arc<Self> {
        Arc::new(Self {
            clock: Mutex::new(Clock {
                base: Instant::now(),
                elapsed: Duration::ZERO,
                wall0_ms: 1_800_000_000_000,
            }),
            draws: Mutex::new(Rng::new(seed ^ 0x005e_ed0f_d8a3)),
            broker,
            device,
            kill: Mutex::new(KillPlan::default()),
            readers: Mutex::new(Vec::new()),
            trace: Mutex::new(Vec::new()),
            mutants: Mutex::new(BTreeSet::new()),
            idle_held: Mutex::new(Vec::new()),
        })
    }

    /// The monotonic clock now.
    pub(crate) fn now(&self) -> Instant {
        let c = lock(&self.clock);
        c.base + c.elapsed
    }

    /// The wall clock now, in milliseconds since the epoch.
    pub(crate) fn wall_ms(&self) -> u64 {
        let c = lock(&self.clock);
        c.wall0_ms
            .saturating_add(u64::try_from(c.elapsed.as_millis()).unwrap_or(u64::MAX))
    }

    /// How much virtual time has passed.
    pub(crate) fn elapsed(&self) -> Duration {
        lock(&self.clock).elapsed
    }

    /// Move the clocks on by `d`.
    pub(crate) fn advance(&self, d: Duration) {
        let mut c = lock(&self.clock);
        c.elapsed = c.elapsed.saturating_add(d);
    }

    /// Note one line in the trace two runs of a seed are compared by.
    pub(crate) fn note(&self, line: impl Into<String>) {
        lock(&self.trace).push(line.into());
    }

    /// The trace so far.
    pub(crate) fn trace(&self) -> Vec<String> {
        lock(&self.trace).clone()
    }

    /// Arm a kill of process `pid` at its `steps`-th step from now.
    pub(crate) fn arm_kill(&self, pid: u64, steps: u64) {
        lock(&self.kill).armed = Some((pid, steps.max(1)));
    }

    /// Disarm any kill not yet landed.
    pub(crate) fn disarm_kill(&self) {
        lock(&self.kill).armed = None;
    }

    /// The readers whose next read has something to answer — a frame, or the
    /// connection's end — in the order they were handed over.
    pub(crate) fn readable(&self) -> Vec<(u64, Source)> {
        lock(&self.readers)
            .iter()
            .filter(|r| self.broker.readable(r.conn))
            .map(|r| (r.conn, r.source))
            .collect()
    }

    /// RUN ONE READER ONCE: read its subscription and do with the read what a
    /// reader thread does ([`crate::bridge::reader_took`]). The reader is gone
    /// once it stops. `false` when there was no such reader.
    pub(crate) fn pump(&self, conn: u64) -> bool {
        let reader = {
            let mut readers = lock(&self.readers);
            let Some(i) = readers.iter().position(|r| r.conn == conn) else {
                return false;
            };
            readers.remove(i)
        };
        let mut reader = reader;
        let read = reader.sub.recv();
        let what = match &read {
            Ok(Some((off, subject, _))) => format!("@{off} {subject}"),
            Ok(None) => "end".to_string(),
            Err(e) => format!("err {:?}", e.kind()),
        };
        self.note(format!(
            "pump {:?} conn={} gen={} {what}",
            reader.source, reader.conn, reader.generation
        ));
        let on = crate::bridge::reader_took(
            read,
            &reader.mailbox,
            reader.source,
            reader.generation,
            &reader.closer,
        );
        if on {
            lock(&self.readers).push(reader);
        }
        true
    }

    /// Forget every reader of process `pid` (it died: so did its threads).
    fn drop_readers(&self, pid: u64) {
        lock(&self.readers).retain(|r| r.pid != pid);
    }
}

/// ONE SIMULATED BRIDGE PROCESS: what its doors ([`crate::fx::Fx::sim`]) answer
/// from, and whether it is still alive.
#[derive(Debug)]
pub(crate) struct Process {
    pub(crate) core: Arc<Core>,
    pub(crate) pid: u64,
    alive: AtomicBool,
    /// Inside a read that must not wait ([`Process::trying`]).
    trying: AtomicBool,
}

impl Process {
    pub(crate) fn new(core: Arc<Core>, pid: u64) -> Arc<Self> {
        Arc::new(Self {
            core,
            pid,
            alive: AtomicBool::new(true),
            trying: AtomicBool::new(false),
        })
    }

    /// RUN `op` AS A READ THAT MUST NOT WAIT ([`crate::fx::Fx::unwaited`]):
    /// while it runs, this process's bus reads answer "not yet" for an answer
    /// the broker still holds ([`bus`]'s held acks) instead of releasing it.
    /// Set back however `op` ends, an unwind included.
    pub(crate) fn trying<T>(&self, op: impl FnOnce() -> T) -> T {
        struct Unset<'a>(&'a AtomicBool);
        impl Drop for Unset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        self.trying.store(true, Ordering::SeqCst);
        let _unset = Unset(&self.trying);
        op()
    }

    /// Whether this process is inside a read that must not wait.
    pub(crate) fn is_trying(&self) -> bool {
        self.trying.load(Ordering::SeqCst)
    }

    /// Whether the world switched `rule` off.
    pub(crate) fn mutant(&self, rule: world::Mutant) -> bool {
        lock(&self.core.mutants).contains(&rule)
    }

    /// Whether this process still runs.
    pub(crate) fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// END THIS PROCESS as a `kill -9` would: dead from now on, its readers
    /// gone, and every broker connection it held ended (their wills fire).
    pub(crate) fn kill(&self) {
        if self.alive.swap(false, Ordering::SeqCst) {
            self.core.drop_readers(self.pid);
            self.core.broker.process_died(self.pid);
        }
    }

    pub(crate) fn now(&self) -> Instant {
        self.core.now()
    }

    pub(crate) fn wall_ms(&self) -> u64 {
        self.core.wall_ms()
    }

    /// The loop parked `wait` with nothing queued: the virtual time passes.
    /// First, what the broker still holds unanswered for this process is
    /// noted for the oracle's idle law ([`Core::idle_held`]): a sender's
    /// answer in a batch's flush while the loop goes to sleep is one nothing
    /// will read before the idle backstop's grace.
    pub(crate) fn idle(&self, wait: Duration) {
        let held = self.core.broker.held_for(self.pid);
        if !held.is_empty() {
            lock(&self.core.idle_held).extend(held);
        }
        self.core.advance(wait);
    }

    /// One draw of the bridge's own entropy.
    pub(crate) fn draw(&self) -> u64 {
        lock(&self.core.draws).next_u64()
    }

    /// One broker connection, for this process.
    pub(crate) fn dial(self: &Arc<Self>) -> std::io::Result<(Conn, BusCloser)> {
        self.step("dial");
        bus::dial(self)
    }

    /// A subscription handed to its reader: queued for the scheduler's pump.
    pub(crate) fn reader(
        &self,
        sub: astream_broker::AnySubscription,
        closer: Arc<BusCloser>,
        source: Source,
        mailbox: Arc<Mailbox>,
    ) {
        let conn = match &*closer {
            BusCloser::Sim(c) => c.conn(),
            BusCloser::Os(_) => unreachable!("a simulated process dials simulated connections"),
        };
        let generation = mailbox.broker_generation(source);
        self.core
            .note(format!("reader {source:?} conn={conn} gen={generation}"));
        lock(&self.core.readers).push(Reader {
            pid: self.pid,
            conn,
            source,
            generation,
            sub,
            closer,
            mailbox,
        });
    }

    /// ONE I/O STEP of this process, named `at`: where an armed kill lands.
    /// Nothing for a process already dead (its unwinding destructors still do
    /// I/O, and each is refused where it is made) or a thread already
    /// unwinding.
    pub(crate) fn step(&self, at: &str) {
        if !self.alive() || std::thread::panicking() {
            return;
        }
        let die = {
            let mut plan = lock(&self.core.kill);
            match plan.armed {
                Some((pid, left)) if pid == self.pid => {
                    if left <= 1 {
                        plan.armed = None;
                        true
                    } else {
                        plan.armed = Some((pid, left - 1));
                        false
                    }
                }
                _ => false,
            }
        };
        if die {
            self.core.note(format!("kill pid={} at {at}", self.pid));
            self.kill();
            std::panic::resume_unwind(Box::new(SimKill));
        }
    }

    /// The state dir is about to make `w`: refused for a dead process, a kill
    /// may land before it, and the device may fail it with nothing made
    /// ([`device`]'s write errors).
    pub(crate) fn disk_before(&self, w: Write<'_>) -> std::io::Result<()> {
        if !self.alive() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "simulated: the process is dead",
            ));
        }
        self.step("disk-before");
        if let Some(e) = self.core.device.fails_before(w) {
            let what = self.core.device.describe(w);
            self.core.note(format!(
                "write error {} before {what}",
                device::errno_name(&e)
            ));
            return Err(e);
        }
        Ok(())
    }

    /// The state dir made `w`: the device journals what the media may hold
    /// of it — and may fail its last step after its effect ([`device`]'s
    /// write errors), which the dir returns — and a kill may land after it.
    pub(crate) fn disk_after(&self, w: Write<'_>) -> std::io::Result<()> {
        let made = self.core.device.made(w);
        if let Err(e) = &made {
            let what = self.core.device.describe(w);
            self.core.note(format!(
                "write error {} after {what}",
                device::errno_name(e)
            ));
        }
        self.step("disk-after");
        made
    }
}

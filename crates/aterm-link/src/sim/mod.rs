// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DETERMINISTIC FABRIC SIMULATION, stage 1.

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

/// Lock a mutex of the simulation, through a poison.
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The payload a simulated kill unwinds the bridge with ([`Process::step`]).
#[derive(Debug)]
pub(crate) struct SimKill;

/// The seed's stream.
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

    /// An independent stream for one purpose, so adding a draw to one purpose does not shift every other's.
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
    /// What the broker held unanswered for a process when it PARKED with nothing queued ([`Process::idle`]).
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

    /// The readers whose next read has something to answer.
    pub(crate) fn readable(&self) -> Vec<(u64, Source)> {
        lock(&self.readers)
            .iter()
            .filter(|r| self.broker.readable(r.conn))
            .map(|r| (r.conn, r.source))
            .collect()
    }

    /// RUN ONE READER ONCE.
    pub(crate) fn pump(&self, conn: u64) -> bool {
        let reader = {
            let mut readers = lock(&self.readers);
            let Some(i) = readers.iter().position(|r| r.conn == conn) else {
                return false;
            };
            readers.remove(i)
        };
        let mut reader = reader;
        let read =
            crate::bridge::next_took(&mut reader.sub, &reader.closer, std::time::Duration::ZERO);
        let what = match &read {
            Ok(Some(crate::bridge::Took::Record((off, subject, _)))) => format!("@{off} {subject}"),
            Ok(Some(crate::bridge::Took::Gap(g))) => format!("gap {}..{}", g.from, g.to),
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

/// ONE SIMULATED BRIDGE PROCESS.
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

    /// RUN `op` AS A READ THAT MUST NOT WAIT ([`crate::fx::Fx::unwaited`]).
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

    /// END THIS PROCESS as a `kill -9` would.
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

    /// The loop parked `wait` with nothing queued.
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

    /// A subscription handed to its reader.
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

    /// ONE I/O STEP of this process, named `at`.
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

    /// The state dir is about to make `w`.
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

    /// The state dir made `w`.
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

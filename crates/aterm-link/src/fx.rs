// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bridge effect seam (`Fx`) for clocks, mailboxes, transport, and simulation.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mailbox::{Item, Mailbox, Source};
use crate::transport::{self, Closer, Conn};

/// The bridge's doors.
#[derive(Clone, Debug)]
pub(crate) struct Fx {
    /// The rules a unit test switched off on the live world, for a negative control the
    /// seeded worlds do not reach ([`Fx::os_without`]); the simulated world reads its own.
    #[cfg(test)]
    off: Vec<crate::sim::world::Mutant>,
    /// The clock a unit test holds for the live world ([`Fx::on_a_held_clock`]); the simulated
    /// world keeps its own.
    #[cfg(test)]
    held: Option<Arc<HeldClock>>,
    world: World,
}

/// A CLOCK A UNIT TEST HOLDS (critic item E04d): the live world's monotonic clock, moved only
/// by the test ([`Fx::advance`]) and by the waits the bridge itself waits out (a park that took
/// nothing, a sleep), never by the wall clock between them. So a grace the loop reads off it
/// (`bridge::IDLE_ACK_GRACE`) cannot lapse while a loaded machine runs the test's steps
/// slowly, and lapses the moment the test moves the clock past it, with no wait.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct HeldClock(std::sync::Mutex<Instant>);

#[cfg(test)]
impl HeldClock {
    /// The held instant.
    pub(crate) fn now(&self) -> Instant {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Move the held instant on by `by`.
    pub(crate) fn advance(&self, by: Duration) {
        let mut at = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *at += by;
    }
}

/// One step of a session's refill, as the bridge took it (`Bridge::refill`, `refill_together`):
/// the simulated world's refill law holds each finished refill to the log
/// (`sim::Process::refill_tap`).
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum RefillTap<'a> {
    /// The refill began from the watermark the bridge read for the session (`seen/<sid>`).
    Began {
        sid: &'a str,
        /// Its lane's `Fetch` filter, `/f/<F>/in/<node>/<sid>/>`.
        lane: &'a str,
        watermark: Option<u64>,
    },
    /// One record of its lane was offered to aterm (`owed`: aterm owes it, and the
    /// session's refill stops there).
    Offered { sid: &'a str, off: u64, owed: bool },
    /// Its lane's records below `floor` were given up to retention, said and told.
    Floor { sid: &'a str, floor: u64 },
    /// It read its lane to `head`.
    Ended { sid: &'a str, head: u64 },
    /// The session left before its refill ended.
    Dropped { sid: &'a str },
    /// One read of the node's whole inbox began for `sessions` sessions together.
    Together { sessions: usize },
    /// Its start lay below the node inbox's `oldest` (another lane's floor), so it read its own
    /// lane up to there before the shared read.
    BelowTheOldest { sid: &'a str, oldest: u64 },
}

/// WHAT A RESERVATION'S PUBLISH AT ITS PIN CAME TO where its words or astream's dedup answer
/// asked for a verdict (`Bridge::publish_reserved`, critic item F23): the simulated world counts
/// the answers its worlds meet, so a law of `Bridge::unverified_publication`'s is judged in
/// worlds that reach it (`sim::Core::reservations`).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ReservationTap {
    /// The read back found a record of one of the pin's identities at the answered offset: the
    /// attempt's own, or, for a keyed re-post, its key's winner's (`PublishPin::original`),
    /// which is every verified answer the keyed world counts.
    Verified,
    /// Another writer's record of the pin's face at the answered offset.
    Collision,
    /// `Bridge::unverified_publication`'s case `n` (1 to 5).
    Unverifiable(u8),
    /// The attach's replay settled a keyed re-post's entry at its key's winner, read back
    /// (`Bridge::keyed_winner_at`), not published again.
    ReplayedAtTheWinner,
}

/// Which world answers the doors.
#[derive(Clone, Debug)]
enum World {
    /// The live one: what every call site read before the door.
    Os,
    /// One process of a deterministic simulation (`crate::sim`).
    #[cfg(test)]
    Sim(Arc<crate::sim::Process>),
}

impl Fx {
    /// The live world: the shipped bridge's door.
    pub(crate) const fn os() -> Self {
        Self {
            world: World::Os,
            #[cfg(test)]
            off: Vec::new(),
            #[cfg(test)]
            held: None,
        }
    }

    /// One process of a simulated world.
    #[cfg(test)]
    pub(crate) fn sim(process: Arc<crate::sim::Process>) -> Self {
        Self {
            world: World::Sim(process),
            off: Vec::new(),
            held: None,
        }
    }

    /// The live world with `rule` switched off.
    #[cfg(test)]
    pub(crate) fn os_without(rule: crate::sim::world::Mutant) -> Self {
        Self {
            world: World::Os,
            off: vec![rule],
            held: None,
        }
    }

    /// The live world with each of `rules` switched off: one negative control whose rule
    /// another rule would mask.
    #[cfg(test)]
    pub(crate) fn os_without_each(rules: &[crate::sim::world::Mutant]) -> Self {
        Self {
            world: World::Os,
            off: rules.to_vec(),
            held: None,
        }
    }

    /// This live world on a clock the test holds ([`HeldClock`]), starting at the wall clock's
    /// now: for a rig whose mailbox only the test fills, so a park finds queued whatever it is
    /// to take. Every clone shares the one clock.
    #[cfg(test)]
    pub(crate) fn on_a_held_clock(self) -> Self {
        assert!(
            matches!(self.world, World::Os),
            "a simulated world keeps its own clock"
        );
        Self {
            held: Some(Arc::new(HeldClock(std::sync::Mutex::new(Instant::now())))),
            ..self
        }
    }

    /// The clock the test holds ([`Fx::on_a_held_clock`]), to share with a fake peer that
    /// spends the bridge's time.
    #[cfg(test)]
    pub(crate) fn held_clock(&self) -> Option<Arc<HeldClock>> {
        self.held.clone()
    }

    /// Move the clock the test holds on by `by` ([`HeldClock::advance`]).
    ///
    /// # Panics
    /// When this door's clock is not held ([`Fx::on_a_held_clock`]).
    #[cfg(test)]
    pub(crate) fn advance(&self, by: Duration) {
        self.held
            .as_ref()
            .expect("only a held clock is moved by a test")
            .advance(by);
    }

    /// Whether the simulated world (or a unit test, [`Fx::os_without`]) switched `rule` off.
    #[cfg(test)]
    pub(crate) fn mutant(&self, rule: crate::sim::world::Mutant) -> bool {
        match &self.world {
            World::Os => self.off.contains(&rule),
            World::Sim(p) => p.mutant(rule),
        }
    }

    /// What a session's refill did, for the simulated world's refill law (B09).
    #[cfg(test)]
    pub(crate) fn refill_tap(&self, tap: RefillTap<'_>) {
        if let World::Sim(p) = &self.world {
            p.refill_tap(tap);
        }
    }

    /// What a reservation's publish at its pin came to, for the simulated world's count (F23).
    #[cfg(test)]
    pub(crate) fn reservation_tap(&self, tap: ReservationTap) {
        if let World::Sim(p) = &self.world {
            p.reservation_tap(tap);
        }
    }

    /// The monotonic clock.
    pub(crate) fn now(&self) -> Instant {
        match &self.world {
            World::Os => {
                #[cfg(test)]
                if let Some(clock) = &self.held {
                    return clock.now();
                }
                Instant::now()
            }
            #[cfg(test)]
            World::Sim(p) => p.now(),
        }
    }

    /// The wall clock in milliseconds since the Unix epoch ([`crate::now_ms`]).
    pub(crate) fn wall_ms(&self) -> u64 {
        match &self.world {
            World::Os => crate::now_ms(),
            #[cfg(test)]
            World::Sim(p) => p.wall_ms(),
        }
    }

    /// THE PARK: take one input, waiting at most `wait` for it, the inbox face's wait judged by
    /// this world's clock ([`Mailbox::take`]). On a clock a test holds ([`Fx::on_a_held_clock`])
    /// the live park waits as ever, and one that took nothing moves the clock on by its wait.
    pub(crate) fn take(&self, mailbox: &Mailbox, wait: Duration) -> Option<Item> {
        match &self.world {
            World::Os => {
                #[cfg(test)]
                if let Some(clock) = &self.held {
                    let item = mailbox.take(wait, clock.now());
                    if item.is_none() {
                        clock.advance(wait);
                    }
                    return item;
                }
                mailbox.take(wait, self.now())
            }
            #[cfg(test)]
            World::Sim(p) => {
                let item = mailbox.take(Duration::ZERO, p.now());
                if item.is_none() {
                    p.idle(wait);
                }
                item
            }
        }
    }

    /// A PARK WITH NO INPUT TO TAKE. On a clock a test holds ([`Fx::on_a_held_clock`]) the live
    /// sleep runs as ever (what it waits for, a broker's rate or a marker, keeps the wall
    /// clock), and the clock then moves on by it.
    pub(crate) fn sleep(&self, wait: Duration) {
        match &self.world {
            World::Os => {
                std::thread::sleep(wait);
                #[cfg(test)]
                if let Some(clock) = &self.held {
                    clock.advance(wait);
                }
            }
            #[cfg(test)]
            World::Sim(p) => p.idle(wait),
        }
    }

    /// The reconnect back-off's jitter draw ([`transport::Backoff::failed_with`]).
    pub(crate) fn jitter(&self) -> Option<u64> {
        match &self.world {
            World::Os => transport::random_u64(),
            #[cfg(test)]
            World::Sim(p) => Some(p.draw()),
        }
    }

    /// A fresh alias key for the bus mirror's edges ([`crate::mirror::mint_alias_key`]).
    pub(crate) fn alias_key(&self) -> Option<[u8; 16]> {
        match &self.world {
            World::Os => crate::mirror::mint_alias_key(),
            #[cfg(test)]
            World::Sim(p) => {
                let mut key = [0u8; 16];
                key[..8].copy_from_slice(&p.draw().to_le_bytes());
                key[8..].copy_from_slice(&p.draw().to_le_bytes());
                Some(key)
            }
        }
    }

    /// ONE BROKER CONNECTION to `endpoint` ([`transport::connect`]).
    pub(crate) fn dial(&self, endpoint: &str, deadline: Duration) -> io::Result<(Conn, BusCloser)> {
        match &self.world {
            World::Os => {
                transport::connect(endpoint, deadline).map(|(c, closer)| (c, BusCloser::Os(closer)))
            }
            #[cfg(test)]
            World::Sim(p) => p.dial(),
        }
    }

    /// A READ THAT MUST NOT WAIT ([`crate::pipeline::Link::try_recv`]).
    pub(crate) fn unwaited<T>(&self, op: impl FnOnce() -> T) -> T {
        match &self.world {
            World::Os => op(),
            #[cfg(test)]
            World::Sim(p) => p.trying(op),
        }
    }

    /// HAND AN OPEN SUBSCRIPTION TO ITS READER ([`crate::bridge::spawn_reader`]).
    pub(crate) fn start_reader(
        &self,
        sub: astream_broker::AnySubscription,
        closer: Arc<BusCloser>,
        deadline: Duration,
        source: Source,
        mailbox: Arc<Mailbox>,
    ) -> io::Result<()> {
        match &self.world {
            World::Os => crate::bridge::spawn_reader(sub, closer, deadline, source, mailbox),
            #[cfg(test)]
            World::Sim(p) => {
                let _ = deadline;
                p.reader(sub, closer, source, mailbox);
                Ok(())
            }
        }
    }

    /// ONE TRIP ON THE VERB LANE, `op` being the trip itself: one request, or a pipelined batch
    /// of them ([`crate::ctl::Ctl::pipeline`], `Bridge::send_rows`). The simulation's kill
    /// points are before and after the whole trip: none falls between two lines of a batch.
    pub(crate) fn lane<T>(&self, op: impl FnOnce() -> T) -> T {
        match &self.world {
            World::Os => op(),
            #[cfg(test)]
            World::Sim(p) => {
                p.step("lane-before");
                let out = op();
                p.step("lane-after");
                out
            }
        }
    }

    /// Whether the presence view may be opened: always live.
    pub(crate) const fn presence_view(&self) -> bool {
        match &self.world {
            World::Os => true,
            #[cfg(test)]
            World::Sim(_) => false,
        }
    }

    /// The state dir's door this process writes through.
    pub(crate) fn disk(&self) -> Disk {
        match &self.world {
            World::Os => Disk::Os,
            #[cfg(test)]
            World::Sim(p) => Disk::Sim(Arc::clone(p)),
        }
    }
}

/// What ends one broker connection from outside the thread that reads it ([`Closer`]).
#[derive(Debug)]
pub(crate) enum BusCloser {
    /// A duplicate of the live socket ([`Closer`]).
    Os(Closer),
    /// A simulated connection.
    #[cfg(test)]
    Sim(crate::sim::bus::SimCloser),
}

impl BusCloser {
    /// End the connection in both directions ([`Closer::close`]).
    pub(crate) fn close(&self) {
        match self {
            BusCloser::Os(c) => c.close(),
            #[cfg(test)]
            BusCloser::Sim(c) => c.close(),
        }
    }

    /// Bound or lift how long a read on the connection parks ([`Closer::set_read_timeout`]).
    /// # Errors
    /// The live `setsockopt`'s.
    pub(crate) fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match self {
            BusCloser::Os(c) => c.set_read_timeout(d),
            #[cfg(test)]
            BusCloser::Sim(_) => {
                let _ = d;
                Ok(())
            }
        }
    }

    /// The live closer inside.
    #[cfg(test)]
    pub(crate) fn into_os(self) -> Closer {
        match self {
            BusCloser::Os(c) => c,
            BusCloser::Sim(_) => unreachable!("a live dial answers a live closer"),
        }
    }
}

/// THE STATE DIR'S DOOR ([`crate::state::StateDir`]).
#[derive(Clone, Debug, Default)]
pub(crate) enum Disk {
    /// The filesystem, as the dir always wrote it.
    #[default]
    Os,
    /// One simulated process's writes to its world's device.
    #[cfg(test)]
    Sim(Arc<crate::sim::Process>),
}

/// One write the state dir is about to make or has made, as the device journals it ([`Disk`]).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "what a write was is read by the simulated device alone; the live door takes \
                  no note of it"
    )
)]
pub(crate) enum Write<'a> {
    /// `path` atomically replaced by `bytes` (temp + rename) under `mode`.
    Replace {
        path: &'a std::path::Path,
        bytes: &'a [u8],
        flush: Option<aterm_durable::Flush>,
        mode: u32,
    },
    /// `bytes` appended to `path` in one `write`, never flushed.
    Append {
        path: &'a std::path::Path,
        bytes: &'a [u8],
    },
    /// `path` unlinked, never flushed.
    Remove { path: &'a std::path::Path },
    /// Flush `path` ([`aterm_durable::Flush::Durable`], [`aterm_durable::Flush::Ordered`]).
    Flush {
        path: &'a std::path::Path,
        flush: aterm_durable::Flush,
    },
    /// Flush dir `path` ([`aterm_durable::Flush::Durable`], [`aterm_durable::Flush::Ordered`]).
    SyncDir {
        path: &'a std::path::Path,
        flush: aterm_durable::Flush,
    },
}

impl Disk {
    /// About to make `w`.
    /// # Errors
    /// The simulated refusal of a write by a process that is gone, or a simulated write error.
    pub(crate) fn before(&self, w: Write<'_>) -> io::Result<()> {
        match self {
            Disk::Os => {
                let _ = w;
                Ok(())
            }
            #[cfg(test)]
            Disk::Sim(p) => p.disk_before(w),
        }
    }

    /// `w` was made, and returned `Ok`.
    /// # Errors
    /// The simulated failure of a write's last step, after its effect.
    pub(crate) fn after(&self, w: Write<'_>) -> io::Result<()> {
        match self {
            Disk::Os => {
                let _ = w;
                Ok(())
            }
            #[cfg(test)]
            Disk::Sim(p) => p.disk_after(w),
        }
    }
}

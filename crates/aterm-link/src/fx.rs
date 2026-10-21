// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Fx`: THE BRIDGE'S DOORS — the one place its loop reads a clock, parks on
//! its mailbox, dials the broker, hands a subscription to a reader, talks on
//! its verb lane and draws a random number, and the one place a deterministic
//! simulation stands in for all of them (`crate::sim`, test builds).
//!
//! The pattern is `aterm-agent`'s `supervise::fx`: every value the loop
//! uses for its operational clocks and backend dispatch crosses a door, so
//! a world other than the live one can answer those operations. Constructor
//! filesystem/metadata work and optional diagnostic I/O remain real. There
//! the other world is a harness tape; here it is
//! a seeded simulation of the whole fabric around one REAL bridge — a broker
//! that may lose exactly what astream's power-loss law lets it lose, an aterm
//! endpoint, a state dir on a device that can lose power, and a scheduler that
//! kills the bridge at any of its I/O steps — so an interleaving bug is found
//! by a seed and replayed by the same seed, instead of one end-to-end scenario
//! at a time (the last three bridge fixes, 538cd43c45, b6e072f744 and
//! 62a83beae9, were interleaving bugs).
//!
//! **What crosses a door.**
//! * THE CLOCKS ([`Fx::now`], [`Fx::wall_ms`]): the monotonic clock every
//!   deadline, back-off, round trip and duty runs on, and the wall clock a
//!   body's `t=` and an ask's deadline are stamped with. Before this door they
//!   were read in about seventy places across `bridge.rs` and `bridge_mirror.rs`.
//! * THE PARK ([`Fx::take`]): the loop's one blocking wait for an input. Live,
//!   it is [`Mailbox::take`]; simulated, nothing blocks — the virtual clock
//!   moves by the wait and the scheduler decides what arrived. A park with no
//!   input to take ([`Fx::sleep`], a dev seam's poll) moves the clock the same
//!   way.
//! * THE BUS ([`Fx::dial`], [`Fx::start_reader`]): every broker connection is
//!   dialled here, and every subscription is handed here to whatever reads it.
//!   Live, that is [`transport::connect`] and a reader thread per face;
//!   simulated, an in-memory broker speaking astream's own wire
//!   (`astream_broker::proto`) through astream's own client, and a pump the
//!   scheduler drives. The closer that ends a connection from outside its
//!   reader is the door's too ([`BusCloser`]).
//! * THE VERB LANE ([`Fx::lane`]): every request to aterm already goes through
//!   one funnel (`Bridge::ctl_request`); the door wraps it, so a simulated
//!   kill can land on either side of a request the endpoint handled.
//! * THE STATE DIR'S WRITES ([`Disk`]): every replace, append and unlink a
//!   [`crate::state::StateDir`] makes. Live, the filesystem calls it always
//!   made; simulated, the same calls on a scratch directory, journalled by a
//!   device that knows what a power cut may keep of each (aterm-durable's
//!   contract: a durable write survives, an atomic-only one may not).
//! * THE ENTROPY ([`Fx::jitter`], [`Fx::alias_key`]): the reconnect
//!   back-off's draw, the one random number the loop's own course depends on,
//!   and the bus mirror's alias key, which its bodies carry.
//!
//! The clocks and the parks are held to their door by a census guard
//! (`aterm-census`'s `bridge-fx`): a shipped `Instant::now`, `SystemTime::now`,
//! `.elapsed()`, `now_ms()` (the wall clock as the bridge read it before the
//! door: [`crate::now_ms`]), `unix_millis(`, `thread::sleep(` or
//! `mailbox.take(` in `bridge.rs`, `bridge_mirror.rs` or `pipeline.rs` is a
//! finding. A read under another name escapes it: the guard is lexical.
//!
//! **Live answers with the corresponding OS operation.** [`Fx::os`] uses:
//! `Instant::now()`,
//! [`crate::now_ms`], [`Mailbox::take`], [`transport::connect`], a reader
//! thread ([`crate::bridge`]'s `spawn_reader`), the CSPRNG draw
//! ([`transport::Backoff::failed`]'s). No lock, no allocation, no indirection a
//! release build can see beyond one `match` on a two-armed enum whose second
//! arm does not exist outside a test build.
//!
//! **Where the claim ends.** The door covers the bridge's LOOP: the run loop,
//! every handler it calls, the bus mirror's half. It does not cover the
//! presence view, an accelerator that dials and reads on a thread of its own
//! ([`crate::presence_view`]): the simulation keeps it shut
//! ([`Fx::presence_view`]), and the bridge then routes a bare address by its
//! walk, as it does whenever the view is down. Nor does it cover the CLI verbs
//! (`aterm fabric`, the doctor), which run once and read the real world.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mailbox::{Item, Mailbox, Source};
use crate::transport::{self, Closer, Conn};

/// The bridge's doors. Cheap to clone (an `Arc` inside at most): the bridge
/// holds one, and its state dir's [`Disk`] comes from it.
#[derive(Clone, Debug)]
pub(crate) struct Fx {
    world: World,
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
        Self { world: World::Os }
    }

    /// One process of a simulated world.
    #[cfg(test)]
    pub(crate) fn sim(process: Arc<crate::sim::Process>) -> Self {
        Self {
            world: World::Sim(process),
        }
    }

    /// Whether the simulated world switched `rule` off for this process: one
    /// of the simulation's NEGATIVE CONTROLS (`crate::sim::world::Mutant`).
    /// Never in the live world.
    #[cfg(test)]
    pub(crate) fn mutant(&self, rule: crate::sim::world::Mutant) -> bool {
        match &self.world {
            World::Os => false,
            World::Sim(p) => p.mutant(rule),
        }
    }

    /// The monotonic clock: every deadline, back-off and round trip.
    pub(crate) fn now(&self) -> Instant {
        match &self.world {
            World::Os => Instant::now(),
            #[cfg(test)]
            World::Sim(p) => p.now(),
        }
    }

    /// The wall clock in milliseconds since the Unix epoch: a body's `t=`, an
    /// ask's absolute deadline ([`crate::now_ms`]).
    pub(crate) fn wall_ms(&self) -> u64 {
        match &self.world {
            World::Os => crate::now_ms(),
            #[cfg(test)]
            World::Sim(p) => p.wall_ms(),
        }
    }

    /// THE PARK: take one input, waiting at most `wait` for it. Live, the
    /// mailbox's own wait. Simulated, nothing blocks: what is queued is taken,
    /// and with nothing queued the virtual clock moves by the wait (the
    /// scheduler's inputs arrive between the loop's passes).
    pub(crate) fn take(&self, mailbox: &Mailbox, wait: Duration) -> Option<Item> {
        match &self.world {
            World::Os => mailbox.take(wait),
            #[cfg(test)]
            World::Sim(p) => {
                let item = mailbox.take(Duration::ZERO);
                if item.is_none() {
                    p.idle(wait);
                }
                item
            }
        }
    }

    /// A PARK WITH NO INPUT TO TAKE: `wait` passes and nothing is read. Live,
    /// `std::thread::sleep`. Simulated, the virtual clock moves by `wait`. The
    /// one caller is a dev seam's poll ([`crate::bridge`]'s fault parks, which
    /// a released build never arms and a simulated world never sets); it goes
    /// through the door so no real sleep is left in the bridge's files
    /// (`aterm-census`'s `bridge-fx`).
    pub(crate) fn sleep(&self, wait: Duration) {
        match &self.world {
            World::Os => std::thread::sleep(wait),
            #[cfg(test)]
            World::Sim(p) => p.idle(wait),
        }
    }

    /// The reconnect back-off's jitter draw ([`transport::Backoff::failed_with`]):
    /// the OS CSPRNG live, the seed's stream simulated. `None` when it cannot
    /// be read, and the back-off then waits its whole span.
    pub(crate) fn jitter(&self) -> Option<u64> {
        match &self.world {
            World::Os => transport::random_u64(),
            #[cfg(test)]
            World::Sim(p) => Some(p.draw()),
        }
    }

    /// A fresh alias key for the bus mirror's edges ([`crate::mirror::mint_alias_key`]
    /// live; the seed's draws simulated): the one draw the mirror makes.
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

    /// ONE BROKER CONNECTION to `endpoint`, under `deadline`, and the closer
    /// that ends it from another thread ([`transport::connect`] live).
    pub(crate) fn dial(&self, endpoint: &str, deadline: Duration) -> io::Result<(Conn, BusCloser)> {
        match &self.world {
            World::Os => {
                transport::connect(endpoint, deadline).map(|(c, closer)| (c, BusCloser::Os(closer)))
            }
            #[cfg(test)]
            World::Sim(p) => p.dial(),
        }
    }

    /// HAND AN OPEN SUBSCRIPTION TO ITS READER: live, a thread that pumps it
    /// into the mailbox stamped with the face's generation (`spawn_reader`);
    /// simulated, the scheduler's pump, which reads it when the seed says the
    /// reader ran. `Err` only when a reader thread could not be spawned.
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

    /// ONE REQUEST ON THE VERB LANE, `op` being the request itself. Live it is
    /// `op`. Simulated, the scheduler may kill the process on either side of
    /// it: before the line is written, or after the endpoint answered and
    /// before the bridge read on.
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

    /// Whether the presence view may be opened: always live; never simulated,
    /// where the view's own dial and reader thread are outside the doors and
    /// the bridge routes by its walk instead (the module doc's "Where the claim
    /// ends").
    pub(crate) fn presence_view(&self) -> bool {
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

/// What ends one broker connection from outside the thread that reads it, and
/// bounds that thread's reads: astream's [`Closer`] live, the simulated
/// broker's handle on its connection simulated.
#[derive(Debug)]
pub(crate) enum BusCloser {
    /// A duplicate of the live socket ([`Closer`]).
    Os(Closer),
    /// A simulated connection.
    #[cfg(test)]
    Sim(crate::sim::bus::SimCloser),
}

impl BusCloser {
    /// End the connection in both directions; idempotent ([`Closer::close`]).
    pub(crate) fn close(&self) {
        match self {
            BusCloser::Os(c) => c.close(),
            #[cfg(test)]
            BusCloser::Sim(c) => c.close(),
        }
    }

    /// Bound (or, `None`, lift) how long a read on the connection parks
    /// ([`Closer::set_read_timeout`]). A simulated read never parks.
    ///
    /// # Errors
    ///
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

    /// The live closer inside, for a test that hands a live connection to code
    /// that takes astream's own (the presence view's reader).
    #[cfg(test)]
    pub(crate) fn into_os(self) -> Closer {
        match self {
            BusCloser::Os(c) => c,
            BusCloser::Sim(_) => unreachable!("a live dial answers a live closer"),
        }
    }
}

/// THE STATE DIR'S DOOR: what a [`crate::state::StateDir`] writes through.
/// Every replace, append and unlink the dir makes is announced here before
/// and after it is made ([`Disk::before`], [`Disk::after`]); live that is
/// nothing at all, simulated it is where a kill may land and what the device
/// journals for its power cuts (`crate::sim::device`).
#[derive(Clone, Debug, Default)]
pub(crate) enum Disk {
    /// The filesystem, as the dir always wrote it.
    #[default]
    Os,
    /// One simulated process's writes to its world's device.
    #[cfg(test)]
    Sim(Arc<crate::sim::Process>),
}

/// One write the state dir is about to make or has made, as the device
/// journals it ([`Disk`]).
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
    /// `path` atomically replaced by `bytes` (temp + rename) under `mode`,
    /// flushed (file and directory) when `durable`.
    Replace {
        path: &'a std::path::Path,
        bytes: &'a [u8],
        durable: bool,
        mode: u32,
    },
    /// `bytes` appended to `path` in one `write`, never flushed.
    Append {
        path: &'a std::path::Path,
        bytes: &'a [u8],
    },
    /// `path` unlinked, never flushed.
    Remove { path: &'a std::path::Path },
    /// The directory `path` flushed: every entry change in it (a rename, an
    /// unlink) is on the media, and no file's unflushed data with it.
    SyncDir { path: &'a std::path::Path },
}

impl Disk {
    /// About to make `w`. Live, `Ok`. Simulated: a dead process's write is
    /// refused (it made none), and a kill may land here, before the write.
    ///
    /// # Errors
    ///
    /// The simulated refusal of a write by a process that is gone.
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

    /// `w` was made, and returned `Ok`. Live, nothing. Simulated, the device
    /// journals it, and a kill may land here, after the write.
    pub(crate) fn after(&self, w: Write<'_>) {
        match self {
            Disk::Os => {
                let _ = w;
            }
            #[cfg(test)]
            Disk::Sim(p) => p.disk_after(w),
        }
    }
}

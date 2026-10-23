// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Fx`: THE BRIDGE'S DOORS — the one place its loop reads a clock, parks on its mailbox, dials the broker, hands a subscription to a reader, talks on its verb lane and draws a random number, and the one place a deterministic simulation stands in for all of them (`crate::sim`, test builds).

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mailbox::{Item, Mailbox, Source};
use crate::transport::{self, Closer, Conn};

/// The bridge's doors.
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

    /// Whether the simulated world switched `rule` off for this process: one of the simulation's NEGATIVE CONTROLS (`crate::sim::world::Mutant`).
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

    /// The wall clock in milliseconds since the Unix epoch: a body's `t=`, an ask's absolute deadline ([`crate::now_ms`]).
    pub(crate) fn wall_ms(&self) -> u64 {
        match &self.world {
            World::Os => crate::now_ms(),
            #[cfg(test)]
            World::Sim(p) => p.wall_ms(),
        }
    }

    /// THE PARK: take one input, waiting at most `wait` for it.
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

    /// A PARK WITH NO INPUT TO TAKE: `wait` passes and nothing is read.
    pub(crate) fn sleep(&self, wait: Duration) {
        match &self.world {
            World::Os => std::thread::sleep(wait),
            #[cfg(test)]
            World::Sim(p) => p.idle(wait),
        }
    }

    /// The reconnect back-off's jitter draw ([`transport::Backoff::failed_with`]): the OS CSPRNG live, the seed's stream simulated.
    pub(crate) fn jitter(&self) -> Option<u64> {
        match &self.world {
            World::Os => transport::random_u64(),
            #[cfg(test)]
            World::Sim(p) => Some(p.draw()),
        }
    }

    /// A fresh alias key for the bus mirror's edges ([`crate::mirror::mint_alias_key`] live; the seed's draws simulated): the one draw the mirror makes.
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

    /// ONE BROKER CONNECTION to `endpoint`, under `deadline`, and the closer that ends it from another thread ([`transport::connect`] live).
    pub(crate) fn dial(&self, endpoint: &str, deadline: Duration) -> io::Result<(Conn, BusCloser)> {
        match &self.world {
            World::Os => {
                transport::connect(endpoint, deadline).map(|(c, closer)| (c, BusCloser::Os(closer)))
            }
            #[cfg(test)]
            World::Sim(p) => p.dial(),
        }
    }

    /// A READ THAT MUST NOT WAIT, `op` being the read ([`crate::pipeline::Link::try_recv`], aterm-0).
    pub(crate) fn unwaited<T>(&self, op: impl FnOnce() -> T) -> T {
        match &self.world {
            World::Os => op(),
            #[cfg(test)]
            World::Sim(p) => p.trying(op),
        }
    }

    /// HAND AN OPEN SUBSCRIPTION TO ITS READER: live, a thread that pumps it into the mailbox stamped with the face's generation (`spawn_reader`); simulated, the scheduler's pump, which reads it when the seed says the reader ran.
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

    /// ONE REQUEST ON THE VERB LANE, `op` being the request itself.
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

    /// Whether the presence view may be opened: always live; never simulated, where the view's own dial and reader thread are outside the doors and the bridge routes by its walk instead (the module doc's "Where the claim ends").
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

/// What ends one broker connection from outside the thread that reads it, and bounds that thread's reads: astream's [`Closer`] live, the simulated broker's handle on its connection simulated.
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

    /// Bound (or, `None`, lift) how long a read on the connection parks ([`Closer::set_read_timeout`]).
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

    /// The live closer inside, for a test that hands a live connection to code that takes astream's own (the presence view's reader).
    #[cfg(test)]
    pub(crate) fn into_os(self) -> Closer {
        match self {
            BusCloser::Os(c) => c,
            BusCloser::Sim(_) => unreachable!("a live dial answers a live closer"),
        }
    }
}

/// THE STATE DIR'S DOOR: what a [`crate::state::StateDir`] writes through.
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
    /// The file at `path` flushed where it is, its bytes as they stand (a clean stop's, `StateDir::end_run_clean`): on the media on return ([`aterm_durable::Flush::Durable`]) or ahead of every later write on its device ([`aterm_durable::Flush::Ordered`]).
    Flush {
        path: &'a std::path::Path,
        flush: aterm_durable::Flush,
    },
    /// The directory `path` flushed: every entry change in it (a rename, an unlink) is on the media — on return ([`aterm_durable::Flush::Durable`], with everything flushed before it on the device), or ahead of every later write on the device ([`aterm_durable::Flush::Ordered`]) — and no file's unflushed data with it.
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

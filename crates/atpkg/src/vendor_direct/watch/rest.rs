// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A package host's one wait (design §1.6): its rest ends at the earliest deadline the host's
//! loop names, or at once when something it waits for rings. A file it watches changed
//! (`aterm.toml`, the bump file, a held move's records); the host's wake source heard a sleep's
//! wake or a step of the wall clock; work it started rang its end (a head round, an index probe,
//! a seat or hosts waiter); or it is stopping.
//!
//! A ring is a hint, never authority. A wait takes what rang before the host reads anything, so
//! a change made during the read rings again and is not lost, and the host then reads the state
//! itself. The bell is a condition variable ([`ParkBell`]): nothing here can fail to be made but
//! the file watch, and a host whose file watch did not start says so ([`Rest::watching`]) and
//! keeps the bounded look it had before its rest heard files.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aterm_uds::file_watch::FileWatch;
use aterm_uds::park_bell::ParkBell;

/// What rang a host's rest since its last wait took what rang.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Told {
    /// A file the rest watches changed, or was made or removed.
    pub inputs: bool,
    /// The host's wake source heard a sleep's wake or a step of the wall clock
    /// ([`Rest::wall_ringer`]). The host judges the wake itself, off its clocks, once per wait.
    pub woke: bool,
    /// Work the host started rang ([`Rest::ringer`]): a head round's moved head or its end, an
    /// index probe's near answer or its end, a seat or hosts waiter's hand-over.
    pub rung: bool,
    /// The host is stopping ([`Rest::stopper`]).
    pub stop: bool,
}

impl Told {
    const fn any(self) -> bool {
        self.inputs || self.woke || self.rung || self.stop
    }
}

/// One ring a worker, a waiter or a wake source makes on a host's rest. Cloned into each
/// worker; ringing a rest nobody waits on is heard by its next wait.
#[derive(Clone)]
pub struct Ring(Arc<dyn Fn() + Send + Sync>);

impl Ring {
    /// A ring that runs `ring`.
    #[must_use]
    pub fn new(ring: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(ring))
    }

    /// Ring the rest.
    pub fn ring(&self) {
        (self.0)();
    }
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ring")
    }
}

/// A package host's rest: the bell its waits park on, and the file watch that rings it.
pub struct Rest {
    bell: Arc<ParkBell<Told>>,
    /// The file watch whose changes ring [`Told::inputs`]; dropping the rest ends it.
    _files: Option<FileWatch>,
    /// Whether a file this host waits for rings the rest ([`Self::watching`]).
    watching: bool,
    /// Whether the host's wake source rings the rest at a sleep's wake ([`Self::hears_sleep`]).
    hears_sleep: bool,
}

impl std::fmt::Debug for Rest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rest")
            .field("watching", &self.watching)
            .field("hears_sleep", &self.hears_sleep)
            .finish_non_exhaustive()
    }
}

impl Rest {
    /// A rest whose file watch, on a thread named `name`, rings it at each change of `files`.
    /// The watch's first look at each file rings it once. Where the watch cannot start the rest
    /// still rings for work and stops, and [`Self::watching`] answers false.
    #[must_use]
    pub fn start(name: &str, files: impl IntoIterator<Item = PathBuf>) -> Self {
        let bell = Arc::new(ParkBell::new(Told::default()));
        let changed = Arc::clone(&bell);
        let files = FileWatch::platform(
            name,
            Box::new(move |_key| changed.ring(|told| told.inputs = true)),
        )
        .ok()
        .inspect(|watch| {
            for (key, path) in (0_u64..).zip(files) {
                watch.watch(key, path);
            }
        });
        Self {
            bell,
            watching: files.is_some(),
            _files: files,
            hears_sleep: false,
        }
    }

    /// A rest no file rings and no wake source rings: the host keeps its bounded looks.
    #[must_use]
    pub fn inert() -> Self {
        Self {
            bell: Arc::new(ParkBell::new(Told::default())),
            _files: None,
            watching: false,
            hears_sleep: false,
        }
    }

    /// A rest a test rings by hand ([`Self::tell`]): it counts as watching its files and hearing
    /// each sleep, since the test tells it of each.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn by_hand() -> Self {
        Self {
            watching: true,
            hears_sleep: true,
            ..Self::inert()
        }
    }

    /// The host's wake source rings [`Self::wall_ringer`] at each sleep's wake.
    #[must_use]
    pub const fn hearing_sleeps(mut self) -> Self {
        self.hears_sleep = true;
        self
    }

    /// Whether a change of the files this host waits for rings the rest: false where the file
    /// watch did not start, so the host looks at them on its bounded look instead.
    #[must_use]
    pub const fn watching(&self) -> bool {
        self.watching
    }

    /// Whether the host's wake source rings this rest at a sleep's wake: false for a host with
    /// none (a terminal session), which keeps its bounded look while a wall-clock deadline is
    /// live so it notices a sleep as soon as it did before.
    #[must_use]
    pub const fn hears_sleep(&self) -> bool {
        self.hears_sleep
    }

    /// The ring work this host started makes when it ends or has news ([`Told::rung`]).
    #[must_use]
    pub fn ringer(&self) -> Ring {
        let bell = Arc::clone(&self.bell);
        Ring::new(move || bell.ring(|told| told.rung = true))
    }

    /// The ring the host's wake source makes at a sleep's wake or a step of the wall clock
    /// ([`Told::woke`]).
    #[must_use]
    pub fn wall_ringer(&self) -> Ring {
        let bell = Arc::clone(&self.bell);
        Ring::new(move || bell.ring(|told| told.woke = true))
    }

    /// The ring that stops the host ([`Told::stop`]).
    #[must_use]
    pub fn stopper(&self) -> Ring {
        let bell = Arc::clone(&self.bell);
        Ring::new(move || bell.ring(|told| told.stop = true))
    }

    /// Wait at most `within` (`None`: until a ring) for a ring, and take what rang: a ring made
    /// before the wait is heard at once. `None` once `within` passed unrung.
    pub fn wait(&self, within: Option<Duration>) -> Option<Told> {
        self.bell.wait(within.unwrap_or(Duration::MAX), |told| {
            let rang = std::mem::take(told);
            rang.any().then_some(rang)
        })
    }

    /// Take what rang without waiting.
    pub fn take(&self) -> Option<Told> {
        self.wait(Some(Duration::ZERO))
    }

    /// Ring the rest as `told` says, as its sources would.
    #[cfg(any(test, debug_assertions))]
    pub fn tell(&self, told: Told) {
        self.bell.ring(|now| {
            now.inputs |= told.inputs;
            now.woke |= told.woke;
            now.rung |= told.rung;
            now.stop |= told.stop;
        });
    }

    /// Wait at most `within` until `what` holds of what rang, leaving it for the host's own wait:
    /// whether it did.
    #[cfg(any(test, debug_assertions))]
    pub fn heard(&self, within: Duration, what: impl Fn(&Told) -> bool) -> bool {
        self.bell
            .wait(within, |told| what(told).then_some(()))
            .is_some()
    }

    /// How many times a wait on this rest parked ([`ParkBell::parks`]).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn parks(&self) -> usize {
        self.bell.parks()
    }
}

#[cfg(test)]
#[path = "rest_tests.rs"]
mod tests;

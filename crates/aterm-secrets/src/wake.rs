// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One process's "network moved" signal (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.5.2). The
//! kernel's route socket ([`hear_routes`]) and the standing `_alab-vault._tcp` browse
//! ([`crate::discover::Watch`]) each ring a [`Bell`], and the server's advert's also rings at
//! this Mac's wake from sleep ([`aterm_uds::power`]). A consumer (the server's advert, the
//! subscriber's error nap) naps on that bell, and the nap says which source woke it. Nothing
//! carries the signal between processes: each process hears its own sources.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What ended a [`Bell::nap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// The nap ran its whole length.
    Timer,
    /// This Mac's addresses or links changed ([`crate::network::RouteChanges`]).
    Route,
    /// The standing browse heard a vault server it had not heard ([`crate::discover::Watch`]).
    Browse,
    /// The local LAN broker began answering its verified hello.
    Broker,
    /// The subscriber's local readiness listener ended and needs upkeep.
    Listener,
    /// The server stored a new version, or records that grew its held log, which its advert
    /// names (`seq` and `head`, `rec`).
    Put,
    /// This Mac woke from a sleep ([`aterm_uds::power::Power::Woke`]), before which the server
    /// ended its registrations.
    Resumed,
    /// The consumer is ending.
    Stop,
}

impl Wake {
    /// The words a log line gives this wake, after "woken by".
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Wake::Timer => "its timer",
            Wake::Route => "a route change",
            Wake::Browse => "a vault server the browse heard",
            Wake::Broker => "the LAN broker becoming ready",
            Wake::Listener => "the readiness listener ending",
            Wake::Put => "a new version or records stored",
            Wake::Resumed => "this Mac waking from sleep",
            Wake::Stop => "its end",
        }
    }
}

/// A wake-up that any source rings and one consumer naps on. A ring that comes while nobody
/// naps is kept for the next nap. Distinct causes are retained in arrival order;
/// repeated rings from the same source coalesce until the consumer takes it.
#[derive(Debug, Default)]
pub struct Bell {
    rung: Mutex<VecDeque<Wake>>,
    cond: Condvar,
    /// The write end of each [`hear`] thread's pipe: dropped with the bell, which leaves each
    /// thread's [`Closing`] at its end.
    closing: Mutex<Vec<std::io::PipeWriter>>,
}

impl Bell {
    /// A bell nobody rang.
    #[must_use]
    pub const fn new() -> Bell {
        Bell {
            rung: Mutex::new(VecDeque::new()),
            cond: Condvar::new(),
            closing: Mutex::new(Vec::new()),
        }
    }

    /// Ring: the nap under way, or else the next one, ends with `why`.
    pub fn ring(&self, why: Wake) {
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        if !rung.contains(&why) {
            rung.push_back(why);
        }
        self.cond.notify_all();
    }

    /// Wait at most `most` for a ring: the one that came, or [`Wake::Timer`].
    pub fn nap(&self, most: Duration) -> Wake {
        const MAX_NAP: Duration = Duration::from_secs(365 * 24 * 60 * 60);
        let deadline = Instant::now() + most.min(MAX_NAP);
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(why) = rung.pop_front() {
                return why;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Wake::Timer;
            }
            rung = self
                .cond
                .wait_timeout(rung, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Forget rings that no nap took: facts the next pass will read itself.
    /// Returns the first discarded cause, when any was pending.
    pub fn clear(&self) -> Option<Wake> {
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        let first = rung.pop_front();
        rung.clear();
        first
    }
}

/// What a waiter given to [`hear`] heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    /// A change came.
    Change,
    /// The [`Closing`] it waited beside reached its end: the bell is gone.
    Closing,
    /// Neither, within the time it was given.
    Nothing,
    /// The source is gone for good and will hear nothing more, so its thread ends. The route
    /// socket never says so ([`route_waiter`]): a failure of its poll or read may pass.
    Failed,
}

/// The read end of the pipe whose write end a [`Bell`] holds for one [`hear`] thread: it is
/// readable, at its end, once the bell is dropped. A waiter polls it beside its own source.
#[derive(Debug)]
pub struct Closing(
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "off Unix no waiter polls it: route changes are heard on macOS and Linux"
        )
    )]
    std::io::PipeReader,
);

#[cfg(unix)]
impl Closing {
    /// The descriptor to poll: readable once the bell is gone.
    #[must_use]
    pub fn fd(&self) -> std::os::fd::BorrowedFd<'_> {
        use std::os::fd::AsFd as _;
        self.0.as_fd()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Closing {
    /// Wait at most `most` for the bell's end, polling nothing else: [`Heard::Closing`] once it
    /// came, else [`Heard::Nothing`]. A poll of the pipe that fails sleeps out `most`.
    fn pause(&self, most: Duration) -> Heard {
        let deadline = Instant::now().checked_add(most);
        match aterm_uds::ready::any(&[self.fd()], deadline) {
            Ok(ready) if !ready.is_empty() => Heard::Closing,
            Ok(_) => Heard::Nothing,
            Err(_) => {
                std::thread::sleep(deadline.map_or(most, |deadline| {
                    deadline.saturating_duration_since(Instant::now())
                }));
                Heard::Nothing
            }
        }
    }
}

/// How long a route waiter whose socket failed waits beside its bell's [`Closing`] before it
/// waits on the socket again ([`route_waiter`]): the step every wait took before, now taken only
/// after a failure.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const FAILED_PAUSE: Duration = Duration::from_secs(5);

/// The kernel's route socket as a waiter for [`hear`] ([`route_waiter`]). `None` where the
/// kernel gives this process no route socket.
#[must_use]
pub fn route_changes() -> Option<impl FnMut(Duration, &Closing) -> Heard + Send + 'static> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        crate::network::RouteChanges::open().ok().map(route_waiter)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None::<fn(Duration, &Closing) -> Heard>
    }
}

/// `changes` as a waiter for [`hear`]: it waits at most the time it is given for a change of
/// this Mac's addresses or links or for the bell's [`Closing`]
/// ([`crate::network::RouteChanges::wait_beside`]), and says which came. Where the socket or
/// poll(2) failed it waits [`FAILED_PAUSE`] beside the bell's [`Closing`] alone and says nothing
/// came, so the next wait asks the socket again: a failure that passes (poll(2)'s `EAGAIN` or
/// `ENOMEM`, a read that failed) costs at most that pause, the step every wait took before, and
/// a socket that stays failed, which polls ready again at once, wakes once a pause.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn route_waiter(
    changes: crate::network::RouteChanges,
) -> impl FnMut(Duration, &Closing) -> Heard + Send + 'static {
    route_waiter_pausing(changes, FAILED_PAUSE)
}

/// [`route_waiter`], pausing `pause` after a failure.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn route_waiter_pausing(
    changes: crate::network::RouteChanges,
    pause: Duration,
) -> impl FnMut(Duration, &Closing) -> Heard + Send + 'static {
    use aterm_uds::network::RouteWake;
    move |most: Duration, closing: &Closing| match changes.wait_beside(most, Some(closing.fd())) {
        RouteWake::Change => Heard::Change,
        RouteWake::Also => Heard::Closing,
        RouteWake::Timeout => Heard::Nothing,
        RouteWake::Failed => closing.pause(pause),
    }
}

/// Ring `bell` with [`Wake::Route`] at each change of this Mac's addresses or links
/// ([`route_changes`]), from a thread of its own that ends once the bell is dropped. Whether it
/// hears them: where the kernel gives no route socket, a nap runs to its timer.
pub fn hear_routes(bell: &Arc<Bell>) -> bool {
    route_changes().is_some_and(|changes| hear(bell, changes, Wake::Route))
}

/// Ring `bell` with `why` each time `changed` says a change came. `changed` is given no time
/// limit ([`Duration::MAX`]) and the bell's [`Closing`] to wait beside its source. It runs on a
/// thread of its own, which ends once the bell is dropped (its [`Closing`] reaches its end, or
/// the next change finds the bell gone) or its source is gone for good ([`Heard::Failed`]), so
/// it wakes for nothing else. Whether the thread started.
pub fn hear(
    bell: &Arc<Bell>,
    mut changed: impl FnMut(Duration, &Closing) -> Heard + Send + 'static,
    why: Wake,
) -> bool {
    let Ok((closing, open)) = aterm_uds::pipe() else {
        return false;
    };
    let closing = Closing(closing);
    let weak = Arc::downgrade(bell);
    let started = std::thread::Builder::new()
        .name("vault-hear".to_owned())
        .spawn(move || {
            loop {
                match changed(Duration::MAX, &closing) {
                    Heard::Change => match weak.upgrade() {
                        Some(bell) => bell.ring(why),
                        None => return,
                    },
                    Heard::Nothing => {}
                    Heard::Closing | Heard::Failed => return,
                }
            }
        })
        .is_ok();
    if started {
        bell.closing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(open);
    }
    started
}

#[cfg(test)]
#[path = "wake_tests.rs"]
mod tests;

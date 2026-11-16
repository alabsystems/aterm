// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared updater wake signaling and connection-recovery classification.
//!
//! A checker's wait between cycles is one deadline on a [`WakeSignal`]: a post ends it (a sleep's
//! wake or a step of the wall clock this process hears, [`hear_wakes_from`], or a settled network
//! move its host relays), a nudge ends a retired waiter's ([`WakeSignal::nudge`]), and a wait no
//! post ended reads the machine's sleep off the wall clock's run past its own monotonic run
//! ([`classify`]). Where nothing posts each of this process's sleeps (and always in a virtual
//! machine's guest, which refuses to arm) the wait is cut into legs of at most [`BLIND_LEG`], so
//! the clocks are read that often.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// Wall-clock run past a leg's own monotonic run that says the machine slept during it.
pub(crate) const SLEEP_GAP: Duration = Duration::from_secs(90);

/// The longest leg of a wait while nothing posts each of this process's sleeps: a host that never
/// asked (a macOS terminal session), one whose hearer is starting, could not start or stopped,
/// and a virtual machine's guest, which never arms. A sleep nothing posted is read off the clocks
/// within this much awake time, as every wait read it before one-leg waits, so a wake still
/// reaches a check within 8 s with the 3 s settle; a suspended guest's resume, which nothing
/// announces, was measured at 6.0 s with it (`docs/DESIGN-warm-successor-2026-09-29.md`,
/// "A wake").
pub(crate) const BLIND_LEG: Duration = Duration::from_secs(5);

/// Whether an update-check error `message` indicates an unreachable network.
pub(crate) fn is_network_unreachable(message: &str) -> bool {
    // Resolving the proxy is as necessary to reaching the channel as resolving its host.
    const UNREACHABLE: [u32; 4] = [5, 6, 7, 28];
    if !message.contains("curl") {
        return false;
    }
    let code_after = |marker: &str| {
        message.match_indices(marker).any(|(at, _)| {
            let digits: String = message[at + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits
                .parse::<u32>()
                .is_ok_and(|code| UNREACHABLE.contains(&code))
        })
    };
    code_after("curl: (") || code_after("exit status: ") || code_after("(exit ")
}

/// Outcome of a cadence sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Waited {
    /// Full delay elapsed normally.
    Elapsed,
    /// The caller no longer owns this wait.
    Cancelled,
    /// A wake signal, network change, or system sleep gap was detected; the
    /// duration is any observed wall-clock excess, zero when there was none.
    Woke(Duration),
}

/// Whether the wall clock's run `observed` over a leg exceeds the leg's own monotonic run `ran`
/// by at least [`SLEEP_GAP`]. A leg late on both clocks (a scheduler delay) is no sleep, and a
/// wall clock set back is never one.
fn is_wake_gap(ran: Duration, observed: Duration) -> bool {
    observed.saturating_sub(ran) >= SLEEP_GAP
}

/// What one leg's return says: a post, or a sleep read off the clocks ([`is_wake_gap`]), is a
/// wake carrying the wall clock's run past the monotonic one (zero for a wall clock set back,
/// which the caller reads as no run); otherwise the leg elapsed.
fn classify(posted: bool, mono_run: Duration, wall_run: Duration) -> Waited {
    if posted || is_wake_gap(mono_run, wall_run) {
        Waited::Woke(wall_run.saturating_sub(mono_run))
    } else {
        Waited::Elapsed
    }
}

/// Nobody was asked to hear this process's sleeps.
const UNHEARD: u8 = 0;
/// A hearer is starting ([`hear_wakes_from`]) and has not answered yet.
const STARTING: u8 = 1;
/// A hearer posts each sleep's wake: a wait takes one leg.
const ARMED: u8 = 2;
/// The hearer cannot hear a sleep, could not start or stopped: a wait reads its clocks every
/// blind leg.
const BLIND: u8 = 3;

/// Cross-thread wake signal: the posts every waiter answers, and who hears this process's sleeps.
pub(crate) struct WakeSignal {
    posts: Mutex<u64>,
    posted: Condvar,
    /// [`UNHEARD`], [`STARTING`], [`ARMED`] or [`BLIND`].
    hearer: AtomicU8,
    /// Whether this process runs in a virtual machine's guest, asked once ([`Self::in_guest`]).
    guest: fn() -> bool,
    in_guest: OnceLock<bool>,
    /// Whether the blind waits were said in aterm.log ([`Self::blind_leg`]).
    said_blind: AtomicBool,
    /// Every leg a waiter entered, in order: a test supersedes only once a waiter is inside its
    /// wait.
    #[cfg(test)]
    legs: Mutex<Vec<Duration>>,
}

impl WakeSignal {
    pub(crate) const fn new() -> Self {
        Self::probing(hypervisor_guest)
    }

    /// A signal that asks `guest` whether this process runs in a virtual machine's guest.
    const fn probing(guest: fn() -> bool) -> Self {
        Self {
            posts: Mutex::new(0),
            posted: Condvar::new(),
            hearer: AtomicU8::new(UNHEARD),
            guest,
            in_guest: OnceLock::new(),
            said_blind: AtomicBool::new(false),
            #[cfg(test)]
            legs: Mutex::new(Vec::new()),
        }
    }

    /// Notify all waiters that the system woke.
    pub(crate) fn post(&self) {
        let mut posts = self.posts.lock().unwrap_or_else(PoisonError::into_inner);
        *posts = posts.wrapping_add(1);
        self.posted.notify_all();
    }

    /// Total wake notifications posted so far.
    pub(crate) fn posts(&self) -> u64 {
        *self.posts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Have every waiter read its predicates again, posting nothing. The posts lock is taken
    /// first: a waiter holds it from its predicate's read until it waits, so the notify cannot
    /// fall between the two.
    pub(crate) fn nudge(&self) {
        drop(self.posts.lock().unwrap_or_else(PoisonError::into_inner));
        self.posted.notify_all();
    }

    /// A hearer of this process's sleeps is starting: a blind leg meanwhile is not said.
    #[cfg(unix)]
    fn hearing(&self) {
        let _ =
            self.hearer
                .compare_exchange(UNHEARD, STARTING, Ordering::AcqRel, Ordering::Acquire);
    }

    /// A hearer that posts each sleep's wake answered, so each wait takes one leg; refused
    /// (`false`) in a virtual machine's guest, whose resume nothing announces.
    pub(crate) fn arm(&self) -> bool {
        if self.in_guest() {
            return false;
        }
        self.hearer.store(ARMED, Ordering::SeqCst);
        true
    }

    /// Nothing hears this process's sleeps any more: each wait reads its clocks every blind leg,
    /// and a waiter inside a long leg is nudged onto one at once.
    pub(crate) fn disarm(&self) {
        self.hearer.store(BLIND, Ordering::SeqCst);
        self.nudge();
    }

    /// Whether a hearer posts each of this process's sleeps.
    pub(crate) fn hears_sleep(&self) -> bool {
        self.hearer.load(Ordering::SeqCst) == ARMED
    }

    fn in_guest(&self) -> bool {
        *self.in_guest.get_or_init(self.guest)
    }

    /// The longest leg while nothing posts a sleep, [`BLIND_LEG`], said once in aterm.log with
    /// why (not while a hearer is starting, which may yet arm the signal).
    fn blind_leg(&self) -> Duration {
        let why = if self.in_guest() {
            "this process runs in a virtual machine's guest, whose resume nothing announces"
        } else {
            "nothing announces this process's sleeps"
        };
        if self.hearer.load(Ordering::SeqCst) != STARTING
            && !self.said_blind.swap(true, Ordering::AcqRel)
        {
            aterm_log::info!(
                "aterm-update: {why}, so the update check reads its clocks every {} s while it waits",
                BLIND_LEG.as_secs()
            );
        }
        BLIND_LEG
    }

    /// One leg of a wait: at most `leg` for a post past the `seen`-th, while `keep` holds and the
    /// hearing stays as `armed` read it (a disarm ends a long leg); whether a post came.
    fn leg(&self, seen: u64, leg: Duration, keep: &impl Fn() -> bool, armed: bool) -> bool {
        let posts = self.posts.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        self.legs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(leg);
        let (posts, _) = self
            .posted
            .wait_timeout_while(posts, leg, |posts| {
                *posts == seen && keep() && self.hears_sleep() == armed
            })
            .unwrap_or_else(PoisonError::into_inner);
        *posts != seen
    }

    /// Every leg a waiter on this signal entered, in order.
    #[cfg(test)]
    pub(crate) fn legs(&self) -> Vec<Duration> {
        self.legs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Whether this process runs in a virtual machine's guest (macOS's `kern.hv_vmm_present`). A
/// test build answers no, so no test's decision reads the machine it runs on.
fn hypervisor_guest() -> bool {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        aterm_sysprobe::hv_vmm_present() == Some(true)
    }
    #[cfg(any(not(target_os = "macos"), test))]
    {
        false
    }
}

/// Process-wide system wake and network-change signal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) static SYSTEM_WOKE: WakeSignal = WakeSignal::new();

/// Sleep for `total`, returning early on a post to `signal` past the `seen`-th, a sleep read off
/// the clocks, or lost ownership. Cancellation takes precedence over a wake.
pub(crate) fn sleep_watching_for_wake(
    signal: &WakeSignal,
    seen: u64,
    total: Duration,
    keep_waiting: impl Fn() -> bool,
) -> Waited {
    wait_reading(
        signal,
        seen,
        total,
        keep_waiting,
        Instant::now,
        SystemTime::now,
    )
}

/// [`sleep_watching_for_wake`] reading the monotonic clock `mono` and the wall clock `wall`: the
/// budget runs on `mono`, and each leg is [`classify`]'d by each clock's run over it, the wall
/// clock read first before the leg and last after it (a wall clock set back over the leg reads as
/// no run).
fn wait_reading(
    signal: &WakeSignal,
    seen: u64,
    total: Duration,
    keep_waiting: impl Fn() -> bool,
    mono: impl Fn() -> Instant,
    wall: impl Fn() -> SystemTime,
) -> Waited {
    let started = mono();
    let armed = std::cell::Cell::new(false);
    wait_legs(
        total,
        || mono().saturating_duration_since(started),
        &keep_waiting,
        || {
            armed.set(signal.hears_sleep());
            (!armed.get()).then(|| signal.blind_leg())
        },
        |leg| {
            let wall_from = wall();
            let mono_from = mono();
            let posted = signal.leg(seen, leg, &keep_waiting, armed.get());
            let mono_run = mono().saturating_duration_since(mono_from);
            let wall_run = wall().duration_since(wall_from).unwrap_or(Duration::ZERO);
            classify(posted, mono_run, wall_run)
        },
    )
}

/// Wait out `total` on the monotonic clock `elapsed`, in legs of what remains, each capped by
/// `longest` where it names a cap; ownership (`keep`) is read before and after each leg.
/// Scheduling delays consume the same budget as the legs, and a zero leg is never taken.
fn wait_legs(
    total: Duration,
    mut elapsed: impl FnMut() -> Duration,
    mut keep: impl FnMut() -> bool,
    mut longest: impl FnMut() -> Option<Duration>,
    mut wait: impl FnMut(Duration) -> Waited,
) -> Waited {
    loop {
        if !keep() {
            return Waited::Cancelled;
        }
        let remaining = total.saturating_sub(elapsed());
        if remaining.is_zero() {
            return Waited::Elapsed;
        }
        let leg = longest().map_or(remaining, |cap| remaining.min(cap));
        let outcome = wait(leg);
        if !keep() {
            return Waited::Cancelled;
        }
        if matches!(outcome, Waited::Woke(_)) {
            return outcome;
        }
    }
}

/// Hear this process's sleeps and wall-clock steps for the waits on `signal`, on a thread `spawn`
/// starts: `start` makes the watch there (the caller pays only the spawn), `signal` is armed once
/// the watch hears a sleep, and each sleep's wake and each step is posted ([`answer`]). Whether
/// the thread started; where it did not, `start` never ran and the waits stay blind.
#[cfg(unix)]
pub(crate) fn hear_wakes_from(
    signal: &'static WakeSignal,
    start: impl FnOnce() -> Option<std::sync::Arc<aterm_uds::wall_watch::WallWatch>> + Send + 'static,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
) -> bool {
    signal.hearing();
    let started = spawn(Box::new(move || {
        let Some(watch) = start() else {
            signal.disarm();
            return;
        };
        if !(watch.hears_sleep() && signal.arm()) {
            signal.disarm();
        }
        relay(signal, || hear(&watch));
    }));
    match started {
        Ok(()) => true,
        Err(error) => {
            signal.disarm();
            aterm_log::warn!(
                "aterm-update: the thread that hears this process's sleeps did not start ({error})"
            );
            false
        }
    }
}

/// The thread [`hear_wakes_from`] hears on.
#[cfg(unix)]
pub(crate) fn wall_thread(body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-update-wall".into())
        .spawn(body)
        .map(drop)
}

/// The next of `watch`'s moves, waiting with no timeout until one is announced.
#[cfg(unix)]
fn hear(watch: &aterm_uds::wall_watch::WallWatch) -> std::io::Result<aterm_uds::wall_watch::Moved> {
    aterm_uds::ready::any(&watch.fds(), None)?;
    Ok(watch.take())
}

/// Post `moved` on `signal` where it announces a sleep or a step: a wake after a will-sleep (it
/// measured the sleep, `slept`), or a set of the wall clock (on Linux a resume too). A wake with
/// no will-sleep before it is an idle sleep that did not come (`kIOMessageSystemWillNotSleep`),
/// and posts nothing.
#[cfg(unix)]
fn answer(signal: &WakeSignal, moved: aterm_uds::wall_watch::Moved) {
    if moved.stepped || moved.slept.is_some() {
        signal.post();
    }
}

/// [`answer`] each move `next` hears until it fails; then `signal` goes blind, a waiter inside a
/// long leg at once, said once, so a failing source never spins.
#[cfg(unix)]
fn relay(
    signal: &WakeSignal,
    mut next: impl FnMut() -> std::io::Result<aterm_uds::wall_watch::Moved>,
) {
    loop {
        match next() {
            Ok(moved) => answer(signal, moved),
            Err(error) => {
                aterm_log::warn!(
                    "aterm-update: the update check stopped hearing this process's sleeps \
                     ({error}); it reads its clocks while it waits instead"
                );
                signal.disarm();
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "check_wake_tests.rs"]
mod tests;

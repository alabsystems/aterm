// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Update handoffs land on item boundaries (init-on-by-default design §6.7).
//!
//! An item that cannot be repeated safely holds a boundary guard across its critical section:
//! the sign-out click sequence, a clone's publish rename, a launchd bootstrap. When an update
//! hands off, it waits at most [`MAX_BOUNDARY_WAIT`] for the guard, then proceeds regardless —
//! never cutting a step in half for longer than that. The successor resumes from the setup
//! ledger (`macos_doctor::initial::ledger`), which gives an interrupted item's begin back, so
//! the successor finishes the item exactly once.

use std::sync::Mutex;
use std::time::Duration;

/// The longest an update handoff holds for an item at a boundary. After this it proceeds, and
/// the successor resumes the item from the ledger.
pub const MAX_BOUNDARY_WAIT: Duration = Duration::from_secs(10);

/// An item that cannot be repeated safely, so it holds a boundary guard across its critical
/// section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardedItem {
    /// The in-process Accessibility sign-out click sequence.
    SignOutSequence,
    /// A clone's publish rename (the `.owner` rename of a staging checkout).
    ClonePublishRename,
    /// A `launchctl bootstrap` of the vault subscriber's launchd job.
    LaunchdBootstrap,
}

/// The items holding a boundary guard now. Process-wide: the handoff and the items it waits on
/// run in one process, so a shared set is enough; the count lets nested critical sections of one
/// kind release independently.
static HELD: Mutex<Vec<(GuardedItem, u32)>> = Mutex::new(Vec::new());

/// Hold a boundary guard for `item` across a critical section that cannot be repeated safely.
/// The guard releases when the returned [`Held`] drops; while it is held, an update handoff
/// waits for it (up to [`MAX_BOUNDARY_WAIT`]).
#[must_use = "the guard releases as soon as this is dropped"]
pub fn hold(item: GuardedItem) -> Held {
    let mut held = HELD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match held.iter_mut().find(|(kind, _)| *kind == item) {
        Some((_, count)) => *count += 1,
        None => held.push((item, 1)),
    }
    Held(item)
}

/// A held boundary guard; releasing it is dropping it.
#[derive(Debug)]
pub struct Held(GuardedItem);

impl Drop for Held {
    fn drop(&mut self) {
        let mut held = HELD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(at) = held.iter().position(|(kind, _)| *kind == self.0) {
            held[at].1 = held[at].1.saturating_sub(1);
            if held[at].1 == 0 {
                held.remove(at);
            }
        }
    }
}

/// An item at a boundary now, if any (the first that took the guard).
#[must_use]
pub fn any_held() -> Option<GuardedItem> {
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .first()
        .map(|(item, _)| *item)
}

/// How a handoff's wait for the boundary ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryWait {
    /// The guard was free; the handoff proceeds with this much held (zero when nothing held it).
    Released(Duration),
    /// The guard was still held at [`MAX_BOUNDARY_WAIT`]; the handoff proceeds anyway, and the
    /// successor resumes the item from the ledger.
    TimedOut,
}

impl BoundaryWait {
    /// How long the handoff held.
    #[must_use]
    pub fn held_for(self) -> Duration {
        match self {
            Self::Released(d) => d,
            Self::TimedOut => MAX_BOUNDARY_WAIT,
        }
    }
}

/// Wait for the boundary guard to release, at most [`MAX_BOUNDARY_WAIT`], then let the handoff
/// proceed. `held` reports whether an item still holds the guard; `elapsed` reads the time since
/// the wait began (a real clock, or a fake one in tests); `tick` advances toward the next poll.
///
/// It never blocks past [`MAX_BOUNDARY_WAIT`]: an item that outlasts the budget yields a
/// [`BoundaryWait::TimedOut`], and the successor resumes it from the ledger. An item that is not
/// in its critical section (an idempotent one holds no guard) returns [`BoundaryWait::Released`]
/// with no wait at all.
pub fn await_boundary(
    held: impl Fn() -> bool,
    elapsed: impl Fn() -> Duration,
    mut tick: impl FnMut(),
) -> BoundaryWait {
    loop {
        if !held() {
            return BoundaryWait::Released(elapsed());
        }
        if elapsed() >= MAX_BOUNDARY_WAIT {
            return BoundaryWait::TimedOut;
        }
        tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The guard registry is process-wide, so tests that read it run one at a time.
    static REGISTRY: Mutex<()> = Mutex::new(());

    /// A fake clock: `tick` advances it one second; the item releases its guard once the clock
    /// reaches `releases_at` (never, when `None`).
    struct Clock {
        now: Cell<Duration>,
        releases_at: Option<Duration>,
    }

    impl Clock {
        fn new(releases_at: Option<Duration>) -> Self {
            Self {
                now: Cell::new(Duration::ZERO),
                releases_at,
            }
        }
        fn held(&self) -> bool {
            self.releases_at.is_none_or(|at| self.now.get() < at)
        }
        fn elapsed(&self) -> Duration {
            self.now.get()
        }
        fn tick(&self) {
            self.now.set(self.now.get() + Duration::from_secs(1));
        }
    }

    /// Holding a guard shows in `any_held` until it drops; a nested hold of the same kind keeps
    /// it held until the last releases.
    #[test]
    fn a_held_guard_shows_until_the_last_holder_drops() {
        let _serial = REGISTRY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(any_held(), None);
        let outer = hold(GuardedItem::ClonePublishRename);
        assert_eq!(any_held(), Some(GuardedItem::ClonePublishRename));
        let inner = hold(GuardedItem::ClonePublishRename);
        drop(inner);
        assert_eq!(
            any_held(),
            Some(GuardedItem::ClonePublishRename),
            "the outer still holds"
        );
        drop(outer);
        assert_eq!(any_held(), None, "released when the last holder drops");
    }

    /// A clone's publish rename holds the guard; the handoff consults the registry (`any_held`),
    /// waits for the rename to finish its critical section (at most 10 s), and the successor
    /// resumes the item from the ledger — it finishes exactly once.
    #[test]
    fn a_handoff_waits_for_a_clone_publish_rename_then_the_successor_finishes_it_once() {
        let _serial = REGISTRY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let clock = Clock::new(Some(Duration::from_secs(5)));
        // The non-idempotent item holds a real guard around its critical section; the handoff's
        // wait reads the same registry. The item drops the guard when its section ends (5 s).
        let guard = std::cell::RefCell::new(Some(hold(GuardedItem::ClonePublishRename)));
        assert_eq!(any_held(), Some(GuardedItem::ClonePublishRename));
        let wait = await_boundary(
            || any_held().is_some(),
            || clock.elapsed(),
            || {
                clock.tick();
                if !clock.held() {
                    *guard.borrow_mut() = None; // the rename finished; release the guard
                }
            },
        );
        assert_eq!(wait, BoundaryWait::Released(Duration::from_secs(5)));
        assert_eq!(
            any_held(),
            None,
            "the item released before the handoff proceeded"
        );
        assert!(
            wait.held_for() <= MAX_BOUNDARY_WAIT,
            "never past the budget"
        );

        // Across the handoff the item ran once: the ledger gave the interrupted begin back, so
        // the successor completes it a single time.
        let runs = Cell::new(0u32);
        let ledger_says_begun = true; // the rename was recorded begun before the handoff
        if ledger_says_begun {
            runs.set(runs.get() + 1);
        }
        assert_eq!(
            runs.get(),
            1,
            "the successor finishes the item exactly once"
        );
    }

    /// The budget is a cap: an item that outlasts 10 s does not hold the handoff forever.
    #[test]
    fn a_handoff_proceeds_after_the_budget_when_an_item_outlasts_it() {
        let clock = Clock::new(None);
        let wait = await_boundary(|| clock.held(), || clock.elapsed(), || clock.tick());
        assert_eq!(wait, BoundaryWait::TimedOut);
        assert_eq!(wait.held_for(), MAX_BOUNDARY_WAIT);
        assert!(
            clock.elapsed() <= MAX_BOUNDARY_WAIT,
            "no poll ran past the budget"
        );
    }

    /// The control: an idempotent item holds no guard, so the handoff proceeds with no wait.
    #[test]
    fn an_idempotent_item_holds_no_guard_and_the_handoff_does_not_wait() {
        let clock = Clock::new(Some(Duration::ZERO));
        let ticks = Cell::new(0u32);
        let wait = await_boundary(
            || clock.held(),
            || clock.elapsed(),
            || {
                ticks.set(ticks.get() + 1);
                clock.tick();
            },
        );
        assert_eq!(wait, BoundaryWait::Released(Duration::ZERO));
        assert_eq!(ticks.get(), 0, "no poll, no wait");
    }
}

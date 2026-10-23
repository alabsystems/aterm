// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reference-counted permit capacities ([`Slots`] and [`UnitBudget`]) released on drop.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// A capacity of `cap` holders.
#[derive(Debug)]
pub(crate) struct Slots {
    used: AtomicUsize,
    cap: usize,
}

/// One holder of a [`Slots`], given back on drop.
#[derive(Debug)]
pub(crate) struct Slot(Arc<Slots>);

impl Slots {
    pub(crate) const fn new(cap: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            cap,
        }
    }

    /// One slot below the cap, or none.
    pub(crate) fn try_take(self: &Arc<Self>) -> Option<Slot> {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.cap).then_some(n + 1)
            })
            .ok()?;
        Some(Slot(Arc::clone(self)))
    }

    /// Acquires a counted slot regardless of the cap.
    pub(crate) fn take_uncapped(self: &Arc<Self>) -> Slot {
        self.used.fetch_add(1, Ordering::AcqRel);
        Slot(Arc::clone(self))
    }

    /// The holders now.
    #[cfg(any(test, unix))]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let previous = self.0.used.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "a capacity slot was given back twice");
    }
}

/// Event reported to a [`UnitBudget`] observer outside its lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BudgetEvent {
    Acquire,
    RefuseAtCap,
    Release,
    ReconcileGrow,
    ReconcileShrink,
    RefuseReconcile,
}

/// What a [`UnitBudget`] holds: the permits live and the units they charge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UnitUsage {
    pub(crate) live: usize,
    pub(crate) units: usize,
}

/// At most `live` permits charging at most `units` units together.
#[derive(Debug)]
pub(crate) struct UnitBudget {
    usage: Mutex<UnitUsage>,
    live: usize,
    units: usize,
    observe: fn(BudgetEvent),
}

/// Permit of a [`UnitBudget`] and its current unit charge, both released on drop.
#[derive(Debug)]
pub(crate) struct UnitPermit {
    budget: Arc<UnitBudget>,
    units: usize,
}

impl UnitBudget {
    pub(crate) const fn new(live: usize, units: usize, observe: fn(BudgetEvent)) -> Self {
        Self {
            usage: Mutex::new(UnitUsage { live: 0, units: 0 }),
            live,
            units,
            observe,
        }
    }

    /// Acquires a permit charging `units` (`> 0`) if both caps hold after the charge.
    pub(crate) fn try_take(self: &Arc<Self>, units: usize) -> Option<UnitPermit> {
        let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
        let admitted = usage.live < self.live
            && units > 0
            && units <= self.units
            && usage.units <= self.units - units;
        if admitted {
            usage.live += 1;
            usage.units += units;
        }
        drop(usage);
        if admitted {
            (self.observe)(BudgetEvent::Acquire);
            Some(UnitPermit {
                budget: Arc::clone(self),
                units,
            })
        } else {
            (self.observe)(BudgetEvent::RefuseAtCap);
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn usage(&self) -> UnitUsage {
        *self.usage.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl UnitPermit {
    /// Atomically replaces this permit's charge with `units`, keeping the old charge on refusal.
    pub(crate) fn reconcile(&mut self, units: usize) -> bool {
        let budget = &self.budget;
        if units == 0 || units > budget.units {
            (budget.observe)(BudgetEvent::RefuseReconcile);
            return false;
        }
        let mut usage = budget.usage.lock().unwrap_or_else(PoisonError::into_inner);
        debug_assert!(
            usage.units >= self.units,
            "a permit charges more than its budget holds"
        );
        let others = usage.units.saturating_sub(self.units);
        if others > budget.units - units {
            drop(usage);
            (budget.observe)(BudgetEvent::RefuseReconcile);
            return false;
        }
        usage.units = others + units;
        drop(usage);
        let previous = std::mem::replace(&mut self.units, units);
        match units.cmp(&previous) {
            std::cmp::Ordering::Greater => (budget.observe)(BudgetEvent::ReconcileGrow),
            std::cmp::Ordering::Less => (budget.observe)(BudgetEvent::ReconcileShrink),
            std::cmp::Ordering::Equal => {}
        }
        true
    }
}

impl Drop for UnitPermit {
    fn drop(&mut self) {
        let mut usage = self
            .budget
            .usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        debug_assert!(usage.live > 0, "a budget permit was given back twice");
        debug_assert!(
            usage.units >= self.units,
            "a permit gave back more units than it held"
        );
        usage.live = usage.live.saturating_sub(1);
        usage.units = usage.units.saturating_sub(self.units);
        drop(usage);
        (self.budget.observe)(BudgetEvent::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_refuse_at_the_cap_and_take_back_what_drops() {
        let slots = Arc::new(Slots::new(2));
        let first = slots.try_take().expect("below the cap");
        let second = slots.try_take().expect("below the cap");
        assert!(slots.try_take().is_none(), "the cap refuses a third");
        assert_eq!(slots.used(), 2, "a refusal takes nothing");
        let over = slots.take_uncapped();
        assert_eq!(slots.used(), 3, "an uncapped slot is still counted");
        drop((first, over));
        assert_eq!(slots.used(), 1);
        let again = slots.try_take().expect("a dropped slot is free again");
        drop((second, again));
        assert_eq!(slots.used(), 0);
    }
}

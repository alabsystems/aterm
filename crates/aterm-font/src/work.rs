// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// Cumulative expansion budget for a single outline query.
pub(crate) struct OutlineWork(Option<u32>);

impl Default for OutlineWork {
    #[inline]
    fn default() -> Self {
        Self(Some(65_536))
    }
}

impl OutlineWork {
    #[inline]
    pub(crate) fn take(&mut self) -> Option<()> {
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| s.outline_steps += 1);
        self.0 = self.0?.checked_sub(1);
        self.0.map(|_| ())
    }

    #[inline]
    pub(crate) const fn exhausted(&self) -> bool {
        self.0.is_none()
    }
}

/// Per-table `GSUB` traversal budget shared across shaper and autohinter lookups.
pub(crate) struct LayoutWork {
    left: core::cell::Cell<u64>,
    refused: core::cell::Cell<bool>,
}

impl LayoutWork {
    /// Budget for `traversals` traversals of a table of `len` bytes.
    #[inline]
    pub(crate) const fn for_table(len: usize, traversals: u64) -> Self {
        let one = (len as u64).saturating_mul(8);
        let one = if one < 16_384 { 16_384 } else { one };
        Self {
            left: core::cell::Cell::new(one.saturating_mul(traversals)),
            refused: core::cell::Cell::new(false),
        }
    }

    /// Spend `units`, returning `None` and marking the table refused when exhausted.
    #[inline]
    pub(crate) fn spend(&self, units: u64) -> Option<()> {
        match self.left.get().checked_sub(units) {
            Some(left) if !self.refused.get() => {
                self.left.set(left);
                Some(())
            }
            _ => {
                self.refused.set(true);
                None
            }
        }
    }

    /// Whether a walk ran past the budget.
    #[cfg(any(feature = "hint", debug_assertions))]
    #[inline]
    pub(crate) fn refused(&self) -> bool {
        self.refused.get()
    }
}

/// Per-thread outline and `COLR` paint-table step counts since the last [`take_spent`].
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// Outline steps requested, including any refused by the budget.
    pub outline_steps: u64,
    /// `COLR` paint tables visited, including any refused by the budget.
    pub paint_tables: u64,
}

#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
std::thread_local! {
    static SPENT: std::cell::Cell<Spent> = const {
        std::cell::Cell::new(Spent {
            outline_steps: 0,
            paint_tables: 0,
        })
    };
}

#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[inline]
pub(crate) fn note(charge: impl FnOnce(&mut Spent)) {
    SPENT.with(|cell| {
        let mut spent = cell.get();
        charge(&mut spent);
        cell.set(spent);
    });
}

/// Return and reset this thread's [`Spent`] counters.
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[must_use]
pub fn take_spent() -> Spent {
    SPENT.with(|cell| cell.replace(Spent::default()))
}

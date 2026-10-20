// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// One outline query's cumulative expansion budget. Nesting caps bound the
/// stack, but an acyclic DAG can visit its children exponentially often.
/// Charge every glyph entry, TrueType point and CFF instruction across all
/// siblings/subroutines, rather than granting each recursive call a new
/// allowance. Partial sink output is possible before refusal, as for any
/// malformed outline.
pub(crate) struct OutlineWork(Option<u32>);

impl Default for OutlineWork {
    fn default() -> Self {
        Self(Some(65_536))
    }
}

impl OutlineWork {
    pub(crate) fn take(&mut self) -> Option<()> {
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| s.outline_steps += 1);
        self.0 = self.0?.checked_sub(1);
        self.0.map(|_| ())
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.0.is_none()
    }
}

/// What this thread's outline queries and `COLR` paint walks have spent
/// since the last [`take_spent`]: how close a real face comes to the two
/// work budgets, the 65,536 outline steps of [`OutlineWork`] and the 2^16
/// paint tables of a `COLR` walk (TF.5 of
/// docs/AUDIT-third-party-2026-10-15.md). The font oracle recorder's
/// `budgets` mode reads it per glyph. Debug builds only: a release build
/// counts nothing and has no such item, and neither has the Trust lane's
/// build (`trust_verify`), which proves the budgets' code as it ships.
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// Outline steps asked for, every one a budget refused included, so a
    /// refused query reads more than 65,536.
    pub outline_steps: u64,
    /// `COLR` paint tables a walk asked to visit, every one the budget
    /// refused included, so a refused walk reads more than 2^16.
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
pub(crate) fn note(charge: impl FnOnce(&mut Spent)) {
    SPENT.with(|cell| {
        let mut spent = cell.get();
        charge(&mut spent);
        cell.set(spent);
    });
}

/// This thread's [`Spent`] since the last call, which starts it again at
/// zero. Debug builds only.
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[must_use]
pub fn take_spent() -> Spent {
    SPENT.with(|cell| cell.replace(Spent::default()))
}

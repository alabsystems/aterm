// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A table's cells, kept from one read to the next so a read applies only the operations that
//! arrived since (LT-B3).
//!
//! [`crate::documents::apply`] gives a cell's candidates as a function of the set of operations
//! applied, provided each is applied after every operation it observes: a later write drops the
//! candidates it observed, and a stop is sticky whichever order it meets a concurrent write in.
//! So the cells of a set `B` are the cells of a subset `A` with `B ∖ A` applied in causal order,
//! exactly when no operation of `B ∖ A` is observed by one of `A`. [`materialize`] reuses the
//! previous cells only where that is certain without asking the graph:
//!
//! - every operation the previous read applied is applied now (a fork, a request conflict, a
//!   duplicate judged anew, or a withdrawal removes one, and then nothing is reused);
//! - every operation applied now and not then was not retained then, so none of the previous
//!   read's operations can name it (an operation names only what was retained when it was
//!   read); one retained then but not applied (an opaque author's, a withheld one now
//!   released) rebuilds;
//! - the skipped set (operations past their writer's quota) and the quarantined writers are the
//!   same.
//!
//! Anything else rebuilds in full, so a reuse never changes an answer, only its cost.
//!
//! Each run of reuses is a lineage, and each reuse that applied an operation is a step of it
//! ([`Mark`]). A step keeps which cells its operations wrote, so a reader that made something of
//! the cells at one step (the materializer's projection of a table's rows, package TK-09) asks
//! which cells changed since ([`Marked::changed_since`]) and remakes only those: an operation's
//! apply changes the cells its changes name and no other ([`crate::documents::apply`]). A
//! rebuild starts a new lineage, and a step no longer kept ([`JOURNAL_STEPS`],
//! [`JOURNAL_CELLS`]) answers nothing, so such a reader makes everything again.

use crate::{database::Change, documents::Cells};
use astream_wire::sync::{Actor, Digest};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// The cells one step wrote: each `(collection, cell key)`.
pub(crate) type Written = BTreeSet<(String, String)>;

/// How many steps of a lineage keep what they wrote.
pub(crate) const JOURNAL_STEPS: usize = 64;

/// How many cells the steps kept may name between them: past it the oldest are dropped.
pub(crate) const JOURNAL_CELLS: usize = 1 << 16;

/// The lineages made in this process, so no two share a number.
static LINEAGES: AtomicU64 = AtomicU64::new(1);

/// Where a read's cells stand: the lineage of reuses they were made in, and its step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mark {
    lineage: u64,
    step: u64,
}

/// A read's [`Mark`] with the cells each step of its lineage kept up to it.
#[cfg_attr(
    all(test, not(unix)),
    expect(
        dead_code,
        reason = "only the unix cells cache (file, att) reads a mark"
    )
)]
#[derive(Clone, Debug)]
pub(crate) struct Marked {
    mark: Mark,
    journal: VecDeque<(u64, Arc<Written>)>,
}

#[cfg_attr(
    all(test, not(unix)),
    expect(
        dead_code,
        reason = "only the unix cells cache (file, att) reads a mark"
    )
)]
impl Marked {
    /// Where these cells stand.
    pub(crate) const fn mark(&self) -> Mark {
        self.mark
    }

    /// The cells that may differ between the cells at `earlier` and these: every cell a step
    /// after `earlier` wrote, up to this one. `None` where `earlier` is of another lineage or
    /// a later step, or a step between is no longer kept: then nothing is known of what
    /// changed.
    pub(crate) fn changed_since(&self, earlier: Mark) -> Option<Written> {
        if earlier.lineage != self.mark.lineage || earlier.step > self.mark.step {
            return None;
        }
        let mut changed = Written::new();
        let mut next = earlier.step + 1;
        for (step, written) in &self.journal {
            if *step < next {
                continue;
            }
            if *step != next {
                return None;
            }
            changed.extend(written.iter().cloned());
            next += 1;
        }
        (next == self.mark.step + 1).then_some(changed)
    }
}

/// What one read of a table's cells applied, and the cells it made.
#[derive(Clone, Debug, Default)]
pub(crate) struct Materialized {
    /// Every operation the log retained, applied or not.
    retained: BTreeSet<Digest>,
    /// The operations applied.
    applied: BTreeSet<Digest>,
    /// The operations left out (past their writer's quota).
    skip: BTreeSet<Digest>,
    /// The writers the projection quarantined.
    quarantined: BTreeSet<Actor>,
    /// The cells.
    pub(crate) cells: Cells,
    /// Where they stand.
    mark: Mark,
    /// What each kept step of the lineage wrote, oldest first.
    journal: VecDeque<(u64, Arc<Written>)>,
    /// Whether these cells were extended from a previous read's rather than rebuilt.
    #[cfg(test)]
    pub(crate) extended: bool,
}

/// The facts of one read [`materialize`] needs.
pub(crate) struct Read<'a> {
    /// Every operation the log retains.
    pub(crate) retained: BTreeSet<Digest>,
    /// The operations to apply, in causal order (each after every operation it observes).
    pub(crate) ordered: &'a [Digest],
    /// Operations left out of the cells.
    pub(crate) skip: &'a BTreeSet<Digest>,
    /// The writers the projection quarantined.
    pub(crate) quarantined: &'a BTreeSet<Actor>,
}

impl Materialized {
    /// Where these cells stand, with what each kept step wrote.
    #[cfg_attr(
        all(test, not(unix)),
        expect(
            dead_code,
            reason = "only the unix cells cache (file, att) reads a mark"
        )
    )]
    pub(crate) fn marked(&self) -> Marked {
        Marked {
            mark: self.mark,
            journal: self.journal.clone(),
        }
    }
}

/// The cells of `read`: `previous`'s with only the new operations applied where the module's
/// rule allows it, else rebuilt from every operation of `read.ordered`. `changes` gives an
/// operation's changes, `None` for one that writes no cell (a merge).
pub(crate) fn materialize<'c>(
    previous: Option<Materialized>,
    read: Read<'_>,
    changes: impl Fn(&Digest) -> Option<&'c [Change]>,
    observes: &dyn Fn(&Digest, &Digest) -> bool,
) -> Materialized {
    let applied: BTreeSet<Digest> = read
        .ordered
        .iter()
        .filter(|id| !read.skip.contains(*id))
        .copied()
        .collect();
    let reusable = previous.filter(|previous| {
        previous.skip == *read.skip
            && previous.quarantined == *read.quarantined
            && previous.applied.is_subset(&applied)
            && applied
                .difference(&previous.applied)
                .all(|id| !previous.retained.contains(id))
    });
    #[cfg(test)]
    let extended = reusable.is_some();
    let reused = reusable.is_some();
    let (mut cells, before, mut mark, mut journal) = match reusable {
        Some(previous) => (
            previous.cells,
            previous.applied,
            previous.mark,
            previous.journal,
        ),
        None => (
            Cells::new(),
            BTreeSet::new(),
            Mark {
                lineage: LINEAGES.fetch_add(1, Ordering::Relaxed),
                step: 0,
            },
            VecDeque::new(),
        ),
    };
    let mut written = Written::new();
    for id in read.ordered {
        if read.skip.contains(id) || before.contains(id) {
            continue;
        }
        if let Some(changes) = changes(id) {
            crate::documents::apply(observes, &mut cells, *id, changes, true);
            written.extend(
                changes
                    .iter()
                    .map(|change| (change.collection.clone(), change.key.clone())),
            );
        }
    }
    // A reuse that applied something is the lineage's next step; a rebuild is a new lineage's
    // first, which no reader made anything of yet.
    if reused && !written.is_empty() {
        mark.step += 1;
        journal.push_back((mark.step, Arc::new(written)));
        let mut cells_kept: usize = journal.iter().map(|(_, written)| written.len()).sum();
        while journal.len() > JOURNAL_STEPS || cells_kept > JOURNAL_CELLS {
            let Some((_, dropped)) = journal.pop_front() else {
                break;
            };
            cells_kept -= dropped.len();
        }
    }
    Materialized {
        retained: read.retained,
        applied,
        skip: read.skip.clone(),
        quarantined: read.quarantined.clone(),
        cells,
        mark,
        journal,
        #[cfg(test)]
        extended,
    }
}

#[cfg(test)]
#[path = "materialized_tests.rs"]
mod tests;

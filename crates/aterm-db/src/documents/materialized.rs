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

use crate::{database::Change, documents::Cells};
use astream_wire::sync::{Actor, Digest};
use std::collections::BTreeSet;

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
    let (mut cells, before) = match reusable {
        Some(previous) => (previous.cells, previous.applied),
        None => (Cells::new(), BTreeSet::new()),
    };
    for id in read.ordered {
        if read.skip.contains(id) || before.contains(id) {
            continue;
        }
        if let Some(changes) = changes(id) {
            crate::documents::apply(observes, &mut cells, *id, changes, true);
        }
    }
    Materialized {
        retained: read.retained,
        applied,
        skip: read.skip.clone(),
        quarantined: read.quarantined.clone(),
        cells,
        #[cfg(test)]
        extended,
    }
}

#[cfg(test)]
#[path = "materialized_tests.rs"]
mod tests;

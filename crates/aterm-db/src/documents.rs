// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared whole-value projection. Callers decide which authenticated operations
//! are eligible; this kernel supplies no membership or resource authority.

use crate::database::{Candidate, Cell, Change};
use astream_wire::sync::{Digest, Index};
use aterm_json::Value;
use std::collections::BTreeMap;

#[cfg(unix)]
pub(crate) mod application;
#[cfg(unix)]
pub(crate) mod log;

pub(crate) type Cells = BTreeMap<(String, String), Cell>;

/// A stop survives all later edits; a compensating absence remains a candidate
/// so that an unseen concurrent value cannot disappear when it arrives later.
pub(crate) fn apply(
    index: &Index,
    cells: &mut Cells,
    operation: Digest,
    changes: &[Change],
    reverting: bool,
) {
    for change in changes {
        let cell = cells
            .entry((change.collection.clone(), change.key.clone()))
            .or_default();
        if change.value.is_none() && !reverting {
            cell.stopped = true;
            cell.candidates.clear();
        } else if !cell.stopped {
            cell.candidates
                .retain(|old| !index.observes(&operation, &old.operation));
            cell.candidates.push(Candidate {
                operation,
                value: change.value.clone().unwrap_or(Value::Null),
            });
            cell.candidates.sort_by_key(|candidate| candidate.operation);
        }
    }
}

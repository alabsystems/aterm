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
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteA1",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteA2",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteB1",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "WriteB2",
        project = "aterm_db::typed::model_tests::cells_transition"
    )
)]
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
            if cell.candidates.len() > 1 {
                cell.candidates
                    .sort_unstable_by_key(|candidate| candidate.operation);
            }
        }
    }
}

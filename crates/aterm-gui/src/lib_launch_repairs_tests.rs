// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launch's repairs on the seed's lane ([`super::launch_repairs_run`]).

use std::sync::atomic::{AtomicUsize, Ordering};

use super::launch_repairs_run;

static MOVES: AtomicUsize = AtomicUsize::new(0);
static NONE: AtomicUsize = AtomicUsize::new(0);

fn moves_one() -> Vec<String> {
    MOVES.fetch_add(1, Ordering::SeqCst);
    vec![
        "moved 1 stale copy of aterm to the Trash (recoverable there): aterm-prev-1.app".to_owned(),
    ]
}

fn moves_none() -> Vec<String> {
    NONE.fetch_add(1, Ordering::SeqCst);
    Vec::new()
}

/// A LAUNCH RAISES ONE ENTRY FOR WHAT ITS REPAIRS MOVED: the repairs run once and what they moved
/// is one exceptions entry naming the move, which the digest admits; repairs that moved
/// nothing raise none. A laboratory's instance (a private state root) runs no repair at all, and a
/// process that set none has nothing to run.
#[test]
fn a_launch_raises_one_entry_for_what_its_repairs_moved() {
    let mut raised = Vec::new();
    launch_repairs_run(Some(moves_one), false, |entry| raised.push(entry));
    assert_eq!(MOVES.load(Ordering::SeqCst), 1);
    assert_eq!(raised.len(), 1);
    let message = raised[0]
        .message()
        .unwrap_or_else(|fault| panic!("the digest refuses the entry: {fault:?}"));
    assert!(
        message
            .detail
            .iter()
            .any(|line| line.contains("to the Trash (recoverable there)")),
        "{:?}",
        message.detail
    );

    launch_repairs_run(Some(moves_none), false, |entry| raised.push(entry));
    assert_eq!(NONE.load(Ordering::SeqCst), 1);
    assert_eq!(raised.len(), 1, "nothing moved, nothing raised");

    launch_repairs_run(Some(moves_one), true, |entry| raised.push(entry));
    launch_repairs_run(None, false, |entry| raised.push(entry));
    assert_eq!(
        MOVES.load(Ordering::SeqCst),
        1,
        "a laboratory's instance runs none"
    );
    assert_eq!(raised.len(), 1);
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! File-descriptor-relative directory walker and disk space accounting.

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]
#![warn(missing_docs)]

pub mod iopol;
mod list;
pub mod look;
pub mod refuse;
pub mod tree;
pub mod walk;

pub use iopol::{
    DiskIo, MaterializePolicy, PolicyOutcome, main_thread_never_materializes,
    set_thread_materialize_policy, set_walker_thread_policy, thread_disk_throttled,
    thread_materialize_policy,
};
pub use list::ListerChoice;
pub use look::{
    Marker, Worktree, build_dir_above, link_target, marker_at, markers_at, worktree_above,
};
pub use refuse::{Deny, LIBRARY_ALLOW, Refusal, canonical, refuse, refuse_look};
pub use tree::{DirId, DirRec, HUGE_NOTE, HUGE_WARN, HugeDir, Sums, Tree};
pub use walk::{
    LevelRow, LinkTotals, MAX_DEPTH, Policy, RootSpec, SLICE, SizeFull, Sliced, Step, Ticket, Walk,
    Walker, diskwalk_size_full, entry_count_of, list_one_level,
};

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
#[cfg(all(test, unix))]
#[path = "walk_tests.rs"]
mod walk_tests;

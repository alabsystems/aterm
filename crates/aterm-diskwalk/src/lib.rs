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
    Marker, Worktree, build_dir_above, directory_identity, link_target, marker_at, markers_at,
    worktree_above,
};
pub use refuse::{
    Deny, LIBRARY_ALLOW, Refusal, canonical, refuse, refuse_look, rel_within, within,
};
pub use tree::{DirId, DirRec, HUGE_NOTE, HUGE_WARN, HugeDir, Sums, Tree};
pub use walk::{
    LevelRow, LinkTotals, MAX_DEPTH, Policy, RootSpec, SLICE, SizeFull, Sliced, Step, Ticket, Walk,
    Walker, diskwalk_size_full, entry_count_of, list_one_level,
};

/// What this thread's walks and looks touched: every directory a walk or a look opened (from
/// `/`, one component at a time) and every name a look `stat`ed or read as a link, for a test
/// that proves a refused path is never opened or `stat`ed.
#[cfg(any(test, debug_assertions))]
pub mod touched {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    std::thread_local! {
        static LOG: RefCell<Option<Vec<PathBuf>>> = const { RefCell::new(None) };
    }

    /// Run `f` and return what it returned with every path this thread touched while it ran,
    /// in order.
    pub fn record<R>(f: impl FnOnce() -> R) -> (R, Vec<PathBuf>) {
        let outer = LOG.with(|log| log.replace(Some(Vec::new())));
        let out = f();
        let seen = LOG.with(|log| log.replace(outer)).unwrap_or_default();
        (out, seen)
    }

    /// Note `path` in the running [`record`], if any.
    #[cfg_attr(not(unix), expect(dead_code, reason = "off Unix nothing is touched"))]
    pub(crate) fn note(path: &Path) {
        LOG.with(|log| {
            if let Some(seen) = log.borrow_mut().as_mut() {
                seen.push(path.to_path_buf());
            }
        });
    }
}

// Both read Unix spellings (`/Users//a`, which Windows reads as relative).
#[cfg(all(test, unix))]
#[path = "lib_tests.rs"]
mod tests;
#[cfg(all(test, unix))]
#[path = "walk_tests.rs"]
mod walk_tests;
// Off Unix every walk and look is `Refusal::Unsupported`.
#[cfg(all(test, not(unix)))]
#[path = "off_unix_tests.rs"]
mod off_unix_tests;

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-diskwalk`: what disk space a folder uses, and every subtree
//! (`docs/DESIGN-system-awareness-2026-10-16.md` §10.5, phase P2d).
//!
//! A library beside `aterm-dirfd`, under its law: fd-relative, and it
//! re-resolves nothing. No timer and no thread: the host's walker thread
//! drives it with an injected clock.
//!
//! * [`refuse`]: the path test run before any syscall, on the logical and the
//!   canonical spelling. It denies every place whose `stat` could put a macOS
//!   privacy dialog on the owner's screen (`consent::tcc_prompting_roots`,
//!   the one list; other users' homes; `~/Library` but aterm's own folders),
//!   calls `/`, a home and a volume root too broad to walk, and never asks a
//!   remote, removable or automounted volume anything.
//! * [`Walk`]: one root's resumable cursor, advanced in ~4 ms slices
//!   ([`Walk::diskwalk_slice`]); [`Walker`] mints a generation per start so a
//!   superseded cursor never resumes (the BudgetedWalk model).
//! * [`Tree`]: one record per directory with its own and its subtree's
//!   [`Sums`] (allocated size shared fairly between hard links, logical size,
//!   what deleting surely frees, cloud placeholders), entry counts beside
//!   bytes, and the queries: [`Tree::biggest`], [`Tree::most_entries`],
//!   [`Tree::huge_dirs`] (the §10.8 entry-count hazard).
//! * [`diskwalk_size_full`]: a whole walk in one call, for a one-shot survey.
//! * [`list_one_level`]: names and entry counts of a too-broad root.
//! * [`look`]: one name's kind in one folder, a link's target, the git
//!   worktree above a folder: the host's root discovery, path-tested the same
//!   way.
//!
//! macOS lists with `getattrlistbulk`, Linux with `getdents64` + `statx`, and
//! any Unix can use `readdir` + `fstatat` ([`ListerChoice::Portable`]), which
//! is also the native arms' oracle in the tests. Windows has no walker yet
//! (design P8): every entry point answers [`Refusal::Unsupported`].

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]
#![warn(missing_docs)]

pub mod iopol;
mod list;
pub mod look;
pub mod refuse;
pub mod tree;
pub mod walk;

pub use iopol::{
    MaterializePolicy, PolicyOutcome, main_thread_never_materializes,
    set_thread_materialize_policy, set_walker_thread_policy, thread_materialize_policy,
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

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The paths a build stamp is of, one rule for every producer that probes a work tree for a
//! commit stamp's `-dirty`: aterm's and aterm-bench's build scripts, the lab's commit (read when
//! it runs) and its Windows build facts, `verify perf`'s pinned git facts and `ship dev-app`'s
//! pinned stamp, each of which mounts this file by `#[path]`. A release cut and its handoff fixtures pin
//! `ATERM_BUILD_GIT_COMMIT` to a commit they build, never `-dirty`, so they ask no work tree.
//!
//! A stamp is dirty when `git status --porcelain` over [`PATHSPEC`] prints anything: a tracked
//! file changed, staged or removed, or an untracked file, anywhere in the work tree but the
//! measured ledger `docs/measured`. No build reads the ledger (no `include!` names it and no
//! build script opens it), and the measurement lanes write their entries into it between the
//! runs of one set: counted, a set's first entry stamped the second run's build `-dirty`, and
//! that new stamp rebuilt the first run's source. A changed or new source file still marks the
//! stamp dirty. A build script that watches the tracked files skips the ledger's too, since no
//! edit there can change the stamp. Both pathspecs are anchored at the work tree's top, so a
//! build script, which runs in its package's folder, asks of the whole tree.

/// The pathspec (with its `--`) of the paths a build stamp is of, for `git status` and
/// `git ls-files`.
pub(crate) const PATHSPEC: [&str; 3] = ["--", ":(top)", ":(top,exclude)docs/measured"];

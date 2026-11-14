// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![allow(
    clippy::expect_used,
    dead_code,
    reason = "a scratch fixture: a step that fails leaves nothing to run, so it panics; another \
              crate's tests call one seam"
)]

use super::{Bootstrap, Paths, Selection, candidate, import, private_file, reserve, root};

/// Fence `paths` under a writer succession: the reservation alone, with its successor's
/// home reserved and holding no custody.
pub(crate) fn fence(paths: &Paths, bootstrap: &Bootstrap) {
    private_file::create_private_dir(&root(paths)).expect("the generation root");
    reserve(paths, &paths.home.join("config.toml"), bootstrap).expect("a reserved succession");
}

/// Fence `paths` under a writer succession whose candidate is never admitted, so its import
/// stops before any record moves, pass after pass (FL-14, ATERM-KI-1109-1).
pub(crate) fn fence_pending(paths: &Paths, bootstrap: &Bootstrap) {
    fence(paths, bootstrap);
    let mut selection = selection(paths);
    assert!(
        import(&mut selection).is_err(),
        "the candidate was never admitted"
    );
}

fn selection(paths: &Paths) -> Selection {
    Selection::read(paths)
        .expect("the selection reads")
        .expect("a selection")
}

/// Seat `admitted`, a Mac the stand-in lab enrolled with keys of its own, as the successor
/// of `paths`: its custody moves into the home the fence reserved, which is returned. No
/// actor or table claim of `paths` is copied.
pub(crate) fn seat(paths: &Paths, admitted: &Paths) -> Paths {
    let next = candidate(paths);
    for entry in std::fs::read_dir(&admitted.dir).expect("the admitted Mac's custody") {
        let entry = entry.expect("an entry");
        std::fs::rename(entry.path(), next.dir.join(entry.file_name())).expect("the entry moves");
    }
    next
}

/// Import the witnessed prefix `paths` holds into its seated successor and seal the
/// succession: its first completion, or its completion made again after an invalidation.
pub(crate) fn complete(paths: &Paths) {
    import(&mut selection(paths)).expect("the succession completes");
}

/// Invalidate the completion of `paths`' succession, as a later source receipt does: its
/// successor may have authored since, so no home of the lineage is known to hold the newest
/// records.
pub(crate) fn invalidate_completion(paths: &Paths) {
    let mut selection = selection(paths);
    selection.complete = Some([0; 32]);
    selection.save().expect("the selection saves");
}

/// One recovery pass as the runtime runs it between transport lifetimes
/// (`super::advance`), waiting the hang detector: it fences, signs the fenced home's
/// statement, admits its successor and imports the witnessed prefix (SF-A). This crate's
/// tests only: the pass it runs is itself test-only.
#[cfg(test)]
pub(crate) fn advance(paths: &Paths, config: &std::path::Path) -> crate::Result<Paths> {
    super::advance(paths, config)
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A pinned build number back to its commit. A pack's build is `git rev-list --count` of
//! the commit packed, strictly monotonic along the first-parent chain, so the chain is
//! binary-searched; a count that exists only inside a merge (crossed by the merge's jump)
//! falls back to a bounded scan of the one merge span that brackets it.

use std::path::Path;

use crate::api::Build;
use crate::error::{Error, Result};
use crate::git;

/// A bracketing merge span longer than this is refused rather than walked.
pub const SPAN_LIMIT: usize = 4000;

/// The commit in `clone` whose `rev-list --count` is `want`, searched on the first-parent
/// chain of the clone's origin head (`origin/HEAD`, else `origin/main`, else
/// `origin/master`).
///
/// # Errors
///
/// A refusal when the clone has no origin head, when `want` exceeds the head's own count
/// (a stale clone, or a pin from a branch the clone lacks), when the bracketing span is
/// longer than [`SPAN_LIMIT`], or when no commit of it has that count.
pub fn resolve_commit(clone: &Path, want: Build) -> Result<String> {
    let head = ["origin/HEAD", "origin/main", "origin/master"]
        .iter()
        .find_map(|r| git::run(clone, &["rev-parse", "--verify", "-q", r]).ok())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Error::refused(format!(
                "resolve: no origin head ref in {}",
                clone.display()
            ))
        })?;
    let total = git::rev_count(clone, &head)?;
    if want > total {
        return Err(Error::refused(format!(
            "resolve: pin {want} exceeds origin head's count {total} (stale clone, or a pin \
             from a branch this clone lacks)"
        )));
    }
    let chain: Vec<String> = git::run(clone, &["rev-list", "--first-parent", &head])?
        .lines()
        .map(str::to_string)
        .collect();
    // Newest first, so counts fall along the chain: every index below `lo` counts above
    // `want`, every index above `hi` below it.
    let (mut lo, mut hi) = (0usize, chain.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let c = git::rev_count(clone, &chain[mid])?;
        if c == want {
            return Ok(chain[mid].clone());
        }
        if c > want {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    // chain[lo - 1] counts above `want` and chain[lo] below it: the pinned commit sits
    // inside the merge that jumped the count across `want`.
    let (Some(newer), Some(older)) = (lo.checked_sub(1).and_then(|i| chain.get(i)), chain.get(lo))
    else {
        return Err(Error::refused(format!(
            "resolve: no first-parent bracket for count {want}"
        )));
    };
    let span = git::run(clone, &["rev-list", &format!("{older}..{newer}")])?;
    for (n, cand) in span.lines().enumerate() {
        if n >= SPAN_LIMIT {
            return Err(Error::refused(format!(
                "resolve: the bracketing merge span exceeds {SPAN_LIMIT} commits hunting \
                 count {want}"
            )));
        }
        if git::rev_count(clone, cand)? == want {
            return Ok(cand.to_string());
        }
    }
    Err(Error::refused(format!(
        "resolve: no commit between {older} and {newer} has rev-list count {want}"
    )))
}

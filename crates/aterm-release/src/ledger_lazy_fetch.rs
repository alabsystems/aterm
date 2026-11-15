// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a release read that must stay local is kept from fetching an object a partial clone
//! lacks: agh's one answer for the selected git, through atpkg's guard
//! ([`atpkg::git_lazy_fetch::guard`], asked of it run hermetically with its package's declared
//! environment, once per file in this process, and only a decided answer remembered), and the
//! release's refusal where git neither takes `--no-lazy-fetch` (git 2.45 and later) nor obeys
//! `GIT_NO_LAZY_FETCH` (the May 2024 security releases and distribution builds that carry their
//! fix).

use std::path::Path;

use agh::git::lazy_fetch::NoLazyFetch;
use atpkg::git_lazy_fetch::{self as shared, Unguarded};

use super::{Error, Result};

/// How the git at `program`, run with its package's declared `env`, keeps a release read from
/// fetching lazily ([`shared::guard`]).
///
/// # Errors
/// The git neither takes `--no-lazy-fetch` nor obeys `GIT_NO_LAZY_FETCH`
/// ([`Unguarded::Fetches`], named with the gits that do and how to get one), or it could not be
/// asked ([`Unguarded::Unasked`], which the next read asks again).
pub(super) fn guard(program: &Path, env: &[(String, String)]) -> Result<NoLazyFetch> {
    shared::guard(&shared::Hermetic::with_env(program, env)).map_err(|unguarded| match unguarded {
        Unguarded::Fetches(why) => Error::new(format!(
            "{why}\nfix: install or update managed Git with `aterm pkg install git` (or update \
             the OS Git when no managed Git is installed), then run the release again"
        )),
        Unguarded::Unasked(why) => Error::new(why),
    })
}

#[cfg(test)]
#[path = "ledger_lazy_fetch_tests.rs"]
mod tests;

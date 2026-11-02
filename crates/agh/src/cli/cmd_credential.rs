// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh auth git-credential get|store|erase` (design §6.9): the git credential helper the
//! isolated runner mounts.

use super::args::Invocation;
use super::{Ctx, Outcome};
use crate::error::{Error, Result};

/// `agh auth git-credential`: the git credential helper.
///
/// # Errors
///
/// could_not_run until WP7.
pub(crate) fn auth_git_credential(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("auth git-credential", "WP7"))
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The ref verbs (design §9.4): `ref create` over the API, and `ref delete` as a leased
//! push through the git runner.

use super::args::Invocation;
use super::{Ctx, Outcome};
use crate::error::{Error, Result};

/// `agh ref create`: a branch or tag ref at a sha.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn ref_create(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("ref create", "WP6"))
}

/// `agh ref delete`: a ref deleted where it still names the expected sha.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn ref_delete(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("ref delete", "WP6"))
}

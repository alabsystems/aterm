// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh asset delete` (design §9.4): one release asset removed, its name typed again.

use super::args::Invocation;
use super::{Ctx, Outcome};
use crate::error::{Error, Result};

/// `agh asset delete`: one asset deleted, its name typed again.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn asset_delete(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("asset delete", "WP6"))
}

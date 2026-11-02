// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The release write verbs (design §9.4): `release create`, `upload`, `publish`, `edit`,
//! `ensure` and `delete`, and the steps every write verb shares.

use super::args::Invocation;
use super::{Ctx, Outcome};
use crate::error::{Error, Result};

/// `agh release create`: a draft release.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn release_create(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("release create", "WP6"))
}

/// `agh release upload`: assets uploaded to a release.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn release_upload(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("release upload", "WP6"))
}

/// `agh release publish`: a draft published, its assets checked.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn release_publish(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("release publish", "WP6"))
}

/// `agh release edit`: a release's title, notes or prerelease mark changed.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn release_edit(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("release edit", "WP6"))
}

/// `agh release ensure`: a release, its assets and its publish, each step once.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn release_ensure(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("release ensure", "WP6"))
}

/// `agh release delete`: a release deleted, its tag typed again.
///
/// # Errors
///
/// could_not_run until WP6.
pub(crate) fn release_delete(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let _ = (ctx, inv);
    Err(Error::not_yet("release delete", "WP6"))
}

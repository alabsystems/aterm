// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `atpkg-publish hosts check` (§16.2): whether this builder's agh.toml is ready for
//! `--host agh-staging` or `--host agh`, read-only.

use crate::ctx::{Ctx, HostChoice};
use crate::error::{Error, Result};

/// One check's verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCheck {
    /// `config`, `staging_writer`, `public_writer`, `grants` or `drafts`.
    pub name: &'static str,
    /// Whether it holds.
    pub ok: bool,
    /// What was found, or what is missing.
    pub detail: String,
}

/// Every check, for the environment a lane unit runs with (`Dirs::resolve(home,
/// xdg_config_home, xdg_state_home)`), for `choice`. `keyless` judges the staging owner
/// alone, for a Linux lane installed `--keyless`, which never writes the public owner.
#[must_use]
pub fn check(
    ctx: &Ctx,
    dirs: &agh::config::Dirs,
    choice: HostChoice,
    keyless: bool,
    programs: &[String],
) -> Vec<HostCheck> {
    // WP12 writes the checks (plan §16.2); until then there are none.
    let _ = (ctx, dirs, choice, keyless, programs);
    Vec::new()
}

/// The CLI verb: `atpkg-publish hosts check [--host agh-staging|agh] [--unit-env] [--keyless]`.
/// It prints one JSON object, `{"checks":[…],"host":"…","ok":…}`, and exits 0 when every
/// check holds, else 1 (Refused).
///
/// # Errors
///
/// Usage for a bad flag.
pub fn run(ctx: &Ctx, args: &[String]) -> Result<()> {
    let _ = (ctx, args);
    Err(Error::cannot_run("not yet: hosts check lands in WP12"))
}

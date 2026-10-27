// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh auth import NAME --from-gh` (design §6.7): the token comes from `gh auth token`, run
//! as a child through `Fx::child`. agh never reads a keychain.

use crate::client::{Github, Meter};
use crate::config::Dirs;
use crate::error::{Error, Result};
use crate::identity::EnvSource;
use crate::identity::provision::{AddOptions, AddRequest, Added};

/// The absolute paths gh is looked for at, in order.
pub const GH_ABSOLUTE: [&str; 3] = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"];

/// The environment variables passed to gh: exactly these, when set.
pub const GH_ENV: [&str; 5] = ["HOME", "PATH", "USER", "LOGNAME", "XDG_CONFIG_HOME"];

/// The bound on the one gh call: 60 s, aterm-machines' budget for the same call.
pub const GH_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

/// What `auth import` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportRequest {
    /// `--user LOGIN`: the gh account. `None`: gh's active account.
    pub user: Option<String>,
    /// `auth add`'s request: name, tier, owners, write, drafts. Its `from` is ignored:
    /// the token comes from gh, as `AddFrom::Stdin`.
    pub add: AddRequest,
}

/// Run `gh auth token --hostname github.com [--user LOGIN]` through `gh.fx()`'s child door
/// and provision its answer as `auth add --stdin` would, through `add_with`:
/// `<config>/agh/tokens/NAME.token` (0600), the login bound with `GET /user`, agh.toml
/// edited. The answer is held in a `Secret` and never printed. The terminal check is
/// `cmd_auth`'s, as for `auth add`.
///
/// # Errors
///
/// - CouldNotRun, next `agh auth add NAME --file PATH --tier TIER`, when no gh exists at
///   `GH_ABSOLUTE`;
/// - Unauthenticated, next the same, when gh exits non-zero. Its stderr is never quoted,
///   because gh's stderr may hold a hint with an account name, and the reason is the exit
///   status;
/// - Invalid when gh's stdout is not one token line;
/// - the provisioning errors of [`super::provision::add_with`].
pub fn import_from_gh(
    gh: &Github,
    dirs: &Dirs,
    env: &dyn EnvSource,
    req: &ImportRequest,
    options: AddOptions,
) -> Result<Added> {
    let _ = (gh, dirs, env, req, options);
    Err(Error::not_yet("auth import --from-gh", "WP8"))
}

/// `import_from_gh`, with the one-profile client's meter.
///
/// # Errors
///
/// As [`import_from_gh`].
pub(crate) fn import_from_gh_metered(
    gh: &Github,
    dirs: &Dirs,
    env: &dyn EnvSource,
    req: &ImportRequest,
    options: AddOptions,
) -> Result<(Added, Meter)> {
    let _ = (gh, dirs, env, req, options);
    Err(Error::not_yet("auth import --from-gh", "WP8"))
}

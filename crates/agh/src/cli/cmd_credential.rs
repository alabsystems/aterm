// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! `agh auth git-credential get|store|erase` (design §6.9): the git credential helper the
//! isolated runner mounts.

use std::path::PathBuf;

use super::args::Invocation;
use super::{Answer, Ctx, Outcome, Receipt};
use crate::config::Dirs;
use crate::error::Result;
use crate::git::credential::{self, HelperIo, HelperRoute};
use crate::git::grant::LiveGrants;
use crate::identity::EnvSource;

/// The helper's directories: the runner's own `Dirs`, from `AGH_GIT_CONFIG_DIR`,
/// `AGH_GIT_STATE_DIR` and `AGH_GIT_HOME`, when all three are set and absolute (a runner's
/// git child has a private empty `HOME`, which names no agh directory); otherwise `fallback`,
/// the context's own (`Dirs::from_env()`), which is what a person's own git gets.
pub(super) fn helper_dirs(env: &dyn EnvSource, fallback: Option<&Dirs>) -> Option<Dirs> {
    let dir = |name: &str| {
        env.var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    match (
        dir(crate::git::CONFIG_DIR_VAR),
        dir(crate::git::STATE_DIR_VAR),
        dir(crate::git::HOME_VAR),
    ) {
        (Some(config), Some(state), Some(home)) => Some(Dirs {
            config,
            state,
            home,
        }),
        _ => fallback.cloned(),
    }
}

/// `agh auth git-credential`: the git credential helper. It prints `username=…` and
/// `password=…` or `quit=1` on stdout, and nothing else anywhere: no receipt, and nothing on
/// stderr, because git shows a helper's stderr to the person. An operation outside `get`,
/// `store` and `erase` is `quit=1`, not Usage, since git may add operations. A route that
/// cannot be built (`credential::helper_route`'s refusal) is `Router::empty()`, so the helper
/// quits.
///
/// # Errors
///
/// could_not_run when the answer could not be written to stdout, or the clock cannot be read.
pub(crate) fn auth_git_credential(ctx: &mut Ctx, inv: &Invocation) -> Result<Outcome> {
    let op = inv.positional(0).unwrap_or_default();
    let dirs = helper_dirs(&*ctx.env, ctx.dirs.as_ref());
    let known = matches!(op, "get" | "store" | "erase");
    match dirs {
        Some(dirs) if known => {
            let route =
                credential::helper_route(&*ctx.env, &dirs).unwrap_or_else(|_| HelperRoute::empty());
            let now = ctx.fx.wall_secs()?;
            let io = HelperIo {
                stdin: &mut *ctx.stdin,
                stdin_is_terminal: ctx.stdin_is_terminal,
                stdout: &mut *ctx.stdout,
            };
            credential::serve_with(
                op,
                io,
                &route.router,
                &LiveGrants::new(&dirs),
                &*ctx.env,
                now,
                &route.pinned,
            )?;
        }
        _ => credential::write_quit(&mut *ctx.stdout)?,
    }
    Ok(Outcome {
        answer: Answer::Nothing,
        receipt: Receipt::new(&inv.verb_name()),
    })
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fresh source-constructor custody supplements an admitted native restart.
//! It never changes the immutable Prepare recipe or authorizes native Execute.
use super::*;

pub(super) fn prepare(
    sid: &str,
    command: &RestartCommand,
    child: &Process,
) -> Result<Option<Arc<aterm_agent::harness::codex_source_registry::RestartReservation>>, ()> {
    let env = codex_account_env().map_err(|_| ())?;
    prepare_in(&env, sid, command, child)
}

/// The constructor reserves in the registry the dispatch serving this Attach lends
/// ([`crate::control::serving_source_registry`]), the one its own source dispatch uses, never a
/// process-wide registry only the app publishes.
fn prepare_in(
    env: &aterm_agent::harness::cli::Env,
    sid: &str,
    command: &RestartCommand,
    child: &Process,
) -> Result<Option<Arc<aterm_agent::harness::codex_source_registry::RestartReservation>>, ()> {
    let registry = crate::control::serving_source_registry().ok_or(())?;
    registry
        .reserve_native_restart(env, sid, command, child)
        .map(|reservation| reservation.map(Arc::new))
        .map_err(|_| ())
}

#[cfg(all(test, unix))]
#[path = "native_source_restart_tests.rs"]
mod tests;

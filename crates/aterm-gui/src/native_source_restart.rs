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
    let registry = crate::control::source_registry().ok_or(())?;
    let env = codex_account_env().map_err(|_| ())?;
    registry
        .reserve_native_restart(&env, sid, command, child)
        .map(|reservation| reservation.map(Arc::new))
        .map_err(|_| ())
}

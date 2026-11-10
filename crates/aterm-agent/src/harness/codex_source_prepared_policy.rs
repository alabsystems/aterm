// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read only the immutable policy captured by this process's accepted prepare.

use super::*;

impl Registry {
    pub fn prepared_policy(
        &self,
        peer: &Peer,
        operation: &str,
    ) -> Result<crate::harness::codex_assignment::PreparedPreview, Error> {
        if !hex(operation, 32) || !peer.current() {
            return Err(Error::Stale);
        }
        let preview = {
            let state = lock(&self.state)?;
            let cell = state
                .reservations
                .get(&actor_key(&peer.caller))
                .ok_or(Error::Stale)?;
            let held = lock(cell)?;
            if held.process != peer.process
                || held.target.caller() != &peer.caller
                || held.target.operation() != operation
                || !matches!(held.phase, seal::Phase::Draining)
                || held.scope()?.cancelled()
                || held.installed.is_some()
            {
                return Err(Error::Stale);
            }
            held.target
                .policy_preview()
                .cloned()
                .ok_or(Error::Unavailable)?
        };
        if !peer.current() {
            return Err(Error::Stale);
        }
        Ok(preview)
    }
}

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Retained actor admission for inherited work. This cannot admit an unrelated
//! process, choose a source, or turn a transport reconnect into a new use.
use super::*;
#[path = "codex_source_target_frontdoor.rs"]
mod target_frontdoor;

impl Registry {
    fn registered_work_peer(&self, caller: &slot::Caller) -> Result<Peer, Error> {
        let process = Process::observe(caller.pid).ok_or(Error::Unavailable)?;
        if process.birth != caller.birth {
            return Err(Error::Stale);
        }
        {
            let state = lock(&self.state)?;
            let actor = state.actors.get(&actor_key(caller)).ok_or(Error::Stale)?;
            // The original kernel/image/App admission owns this exact actor.
            // PID equality alone cannot recover it after exec or PID reuse.
            if actor.process != process {
                return Err(Error::Stale);
            }
        }
        Ok(Peer {
            caller: caller.clone(),
            process,
            launch: None,
        })
    }

    /// Same-actor derived constructor only. The active parent capability and
    /// App scope are revalidated by the existing registry transaction. Exact
    /// prior opens may reconcile after the parent use closes; no fresh open can.
    pub fn open_registered_derived(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        operation: &str,
        parent: UseIdentity<'_>,
    ) -> Result<MemberAllocation, Error> {
        let peer = self.registered_work_peer(caller)?;
        self.open_member_derived_from_cap(&peer, connection, operation, parent)
    }

    /// Recover one active use on its original authenticated work connection.
    /// This returns its existing source-family authority, never a new admission.
    pub fn registered_source_use(
        &self,
        caller: &slot::Caller,
        connection: &Connection,
        member: &str,
        nonce: UseNonce,
        capability: &str,
    ) -> Result<SourceUse, Error> {
        let peer = self.registered_work_peer(caller)?;
        self.continuing_use_context(&peer, connection, member, nonce, capability)
    }
}

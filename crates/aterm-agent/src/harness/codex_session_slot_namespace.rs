// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Opaque namespace continuation of a real admitted credential use. It cannot
//! be reconstructed from a signed Book, operation ID, or a pending boolean.
use super::*;
use crate::harness::{account_work, codex_source_gate::SourceWorkLease, codex_source_registry};
use std::sync::Arc;

pub(in crate::harness) struct RefreshNamespace {
    proof: codex_source_registry::UseNamespace,
}
impl std::fmt::Debug for RefreshNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshNamespace(redacted)")
    }
}
impl RefreshNamespace {
    pub(in crate::harness) fn for_use(
        env: &Env,
        caller: &Caller,
        context: &codex_source_registry::SourceUse,
    ) -> Result<Self, String> {
        let home = env.home.as_deref().ok_or("source home unavailable")?;
        let proof = context
            .namespace(caller, home)
            .map_err(|_| "source use namespace unavailable")?;
        proof
            .refresh_allowed()
            .map_err(|_| "source use has no manager refresh authority")?;
        Ok(Self { proof })
    }
    pub(in crate::harness) fn scope(&self) -> &account_work::Scope {
        self.proof.scope()
    }
    pub(super) fn expected(&self) -> &SourceHandle {
        self.proof.source()
    }
    /// Kernel custody only. This does not replace with_current's authority.
    pub(in crate::harness) fn source_lease(&self) -> Result<Option<Arc<SourceWorkLease>>, String> {
        self.proof
            .revalidated_from_gate()
            .map_err(|_| "source use namespace changed")?
            .source_lease()
    }
    pub(in crate::harness) fn with_current<R>(
        &self,
        env: &Env,
        caller: &Caller,
        binding: &owner::GrantBinding,
        assignment_generation: u64,
        source_authority: &codex_assignment::SourceAuthority,
        run: impl FnOnce() -> Result<R, String>,
    ) -> Result<R, String> {
        self.with_namespace(
            env,
            caller,
            binding,
            assignment_generation,
            source_authority,
            |_| run(),
        )
    }
    pub(super) fn with_namespace<R>(
        &self,
        env: &Env,
        caller: &Caller,
        binding: &owner::GrantBinding,
        assignment_generation: u64,
        source_authority: &codex_assignment::SourceAuthority,
        run: impl FnOnce(&codex_assignment::BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        let home = env.home.as_deref().ok_or("source home unavailable")?;
        let proof = self
            .proof
            .revalidated(home)
            .map_err(|_| "source use namespace changed")?;
        let expected = proof.source();
        if caller != proof.caller()
            || binding != expected.binding()
            || assignment_generation != expected.assignment_generation()
            || source_authority != expected.source_authority()
        {
            return Err("source namespace body changed".into());
        }
        let captured = expected.namespaces().ok_or("source namespace unknown")?;
        if !captured.current() {
            return Err("source namespace changed".into());
        }
        proof.with_namespace(env, |namespace| {
            let result = run(namespace)?;
            if !captured.current() {
                return Err("source namespace changed".into());
            }
            Ok(result)
        })
    }
}

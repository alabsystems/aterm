// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Source discovery has no provider-exchange authority. Keep its namespace
//! view separate from the capability accepted by the refresh engine.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum SourceReadNamespace<'a> {
    ActiveUse(&'a super::super::codex_source_registry::UseNamespace),
    RefreshingUse(&'a RefreshNamespace),
    CapturedPreload(&'a super::super::codex_source_registry::PreloadReadNamespace),
}
impl SourceReadNamespace<'_> {
    pub(super) fn with_namespace<R>(
        &self,
        env: &Env,
        caller: &Caller,
        binding: &owner::GrantBinding,
        generation: u64,
        authority: &codex_assignment::SourceAuthority,
        run: impl FnOnce(&codex_assignment::BookNamespace) -> Result<R, String>,
    ) -> Result<R, String> {
        match self {
            Self::CapturedPreload(proof) => {
                proof.with_namespace(env, caller, binding, generation, authority, run)
            }
            Self::RefreshingUse(proof) => {
                proof.with_namespace(env, caller, binding, generation, authority, run)
            }
            Self::ActiveUse(proof) => {
                let source = proof.source();
                if caller != proof.caller()
                    || binding != source.binding()
                    || generation != source.assignment_generation()
                    || authority != source.source_authority()
                {
                    return Err("source read namespace body changed".into());
                }
                proof.with_namespace(env, run)
            }
        }
    }
}

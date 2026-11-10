// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Sorted principal/actual-token exclusion, borrowed under an existing S→E
//! refresh work lease. This acquires neither source nor exchange recursively.
use super::*;
pub(in crate::harness) struct RefreshResourceTurn {
    namespace: super::super::codex_source_gate::SourceNamespace,
    resources: Vec<Held>,
}
impl RefreshResourceTurn {
    pub(in crate::harness) fn try_acquire(
        namespace: super::super::codex_source_gate::SourceNamespace,
        proof: &super::super::codex_grant_owner::PublicationIdentity,
    ) -> Result<Option<Self>, String> {
        let directory = namespace
            .directory()
            .map_err(str::to_owned)?
            .join("native-publication-resources");
        let mut resources = Vec::new();
        for name in proof.resources() {
            let Some(resource) = Held::open(&directory, name)?.try_exclusive()? else {
                return Ok(None);
            };
            resources.push(resource);
        }
        let turn = Self {
            namespace,
            resources,
        };
        turn.current()?;
        Ok(Some(turn))
    }
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        self.namespace.directory().map_err(str::to_owned)?;
        for held in &self.resources {
            held.current()?;
        }
        Ok(())
    }
}

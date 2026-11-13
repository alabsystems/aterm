// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel custody for recovery metadata only. This role cannot become a
//! PublicationTurn or authorize credential, binding, owner-witness or Slot writes.
use super::*;
use crate::harness::{codex_source_gate::SourceWorkLease, wake_deadline::WakeDeadline};

pub(super) struct RetirementTurn {
    source: SourceWorkLease,
    exchange: Held,
    resources: Vec<Held>,
}

impl RetirementTurn {
    fn try_acquire(home: &Path, names: &[String]) -> Result<Option<Self>, String> {
        // A publisher still needs S-exclusive. Ordinary source bodies/WS can
        // retain S-shared while unrelated recovery metadata is retired.
        let Some(source) = SourceWorkLease::try_for_home(home).map_err(str::to_owned)? else {
            return Ok(None);
        };
        let namespace = source.namespace().map_err(str::to_owned)?;
        let directory = namespace.directory().map_err(str::to_owned)?;
        let Some(exchange) = Held::open(directory, EXCHANGE)?.try_shared()? else {
            return Ok(None);
        };
        let directory = directory.join("native-publication-resources");
        let mut resources = Vec::new();
        // Names come from the authenticated historical principal/RT identity,
        // using the same ordering and namespace as publication and refresh.
        for name in names {
            let Some(resource) = Held::open(&directory, name)?.try_exclusive()? else {
                return Ok(None);
            };
            resources.push(resource);
        }
        let turn = Self {
            source,
            exchange,
            resources,
        };
        turn.current()?;
        Ok(Some(turn))
    }

    pub(super) fn acquire_within(
        home: &Path,
        names: &[String],
        deadline: WakeDeadline,
        current: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        loop {
            if deadline.passed() {
                return Err("native retirement deadline passed".into());
            }
            current()?;
            if let Some(turn) = Self::try_acquire(home, names)? {
                current()?;
                turn.current()?;
                if deadline.passed() {
                    return Err("native retirement deadline passed".into());
                }
                return Ok(turn);
            }
            // Every partial acquisition drops before waiting; no upgrade or
            // account/owner/AuthLock custody spans this contention wait.
            std::thread::sleep(
                deadline
                    .remaining()
                    .min(std::time::Duration::from_millis(20)),
            );
        }
    }

    pub(super) fn current(&self) -> Result<(), String> {
        self.source.current().map_err(str::to_owned)?;
        self.exchange.current()?;
        for resource in &self.resources {
            resource.current()?;
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "native_enrollment_retirement_gate_tests.rs"]
mod tests;

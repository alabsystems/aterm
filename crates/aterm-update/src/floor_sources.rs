// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Who raised an update floor.

use serde::{Deserialize, Serialize};

/// The contributions to one floor, by the machine whose signed manifest raised it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorSources {
    /// The part no revocation lowers: unattributed observations and records without provenance.
    #[serde(default)]
    pub base: u64,
    /// The highest value each machine asked for.
    #[serde(default)]
    pub machines: std::collections::BTreeMap<String, u64>,
}

impl FloorSources {
    /// Whether nothing is recorded, so an unattributed floor writes as a bare number.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base == 0 && self.machines.is_empty()
    }

    /// The floor these contributions make: the highest of them.
    #[must_use]
    pub fn value(&self) -> u64 {
        self.machines.values().copied().fold(self.base, u64::max)
    }

    /// Apply revocations and a new raise to a floor recorded as `recorded`, returning the new floor and sources.
    #[must_use]
    pub fn step(
        &self,
        recorded: u64,
        revoked: &[String],
        raise: u64,
        machine: Option<&str>,
    ) -> (u64, Self) {
        let is_revoked = |id: &str| revoked.iter().any(|r| r == id);
        let mut next = self.clone();
        let explained = next.value();
        if explained < recorded {
            next = Self {
                base: recorded,
                machines: std::collections::BTreeMap::new(),
            };
        } else if explained > recorded {
            next.base = next.base.min(recorded);
            for value in next.machines.values_mut() {
                *value = (*value).min(recorded);
            }
        }
        next.machines.retain(|id, _| !is_revoked(id));
        if raise != 0 {
            match machine {
                Some(id) if is_revoked(id) => {}
                Some(id) => {
                    let entry = next.machines.entry(id.to_string()).or_insert(0);
                    *entry = (*entry).max(raise);
                }
                None => next.base = next.base.max(raise),
            }
        }
        let value = next.value();
        if next.machines.is_empty() {
            next.base = 0;
        }
        (value, next)
    }
}

#[cfg(test)]
#[path = "floor_sources_tests.rs"]
mod tests;

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
mod tests {
    use super::*;

    fn revoked(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn a_revocation_takes_back_its_machines_part_and_nothing_else() {
        let none = FloorSources::default();
        let (v, s) = none.step(0, &[], 900, Some("m3"));
        let (v, s) = s.step(v, &[], 50, None);
        let (v, s) = s.step(v, &[], 9_999_999_999, Some("m11"));
        assert_eq!(v, 9_999_999_999);
        let (same, s) = s.step(v, &revoked(&["m20"]), 0, None);
        assert_eq!(same, 9_999_999_999);
        let (v, s) = s.step(same, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 900);
        let (v, s) = s.step(v, &revoked(&["m11"]), 10_000_000_000, Some("m11"));
        assert_eq!(v, 900);
        let (v, s) = s.step(v, &revoked(&["m11"]), 1_000, Some("m3"));
        assert_eq!(v, 1_000);
        let (v, _) = s.step(v, &revoked(&["m11"]), 10, Some("m3"));
        assert_eq!(v, 1_000);
    }

    #[test]
    fn an_unexplained_floor_is_unattributed_and_a_hand_edit_is_capped() {
        let mut attributed = FloorSources::default();
        attributed.machines.insert("m11".into(), 9_000);
        let (v, s) = attributed.step(9_500, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 9_500);
        assert!(s.is_empty(), "written as before provenance: {s:?}");
        let (v, _) = attributed.step(100, &[], 0, None);
        assert_eq!(v, 100);
        let (v, _) = attributed.step(100, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 0, "the capped part was m11's alone");
        let based = FloorSources {
            base: 9_000,
            ..FloorSources::default()
        };
        assert_eq!(based.step(100, &[], 0, None).0, 100);
        let (v, s) = FloorSources::default().step(100, &[], 0, Some("m3"));
        assert_eq!(v, 100);
        assert!(s.is_empty(), "{s:?}");
        let (v, _) = attributed.step(9_000, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 0);
    }
}

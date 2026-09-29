// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHO RAISED AN UPDATE FLOOR (round seven of the update audit, group H1).
//!
//! Both update lanes keep monotonic floors a signed manifest can raise: the macOS
//! `floor.toml` (`min_build`, `high_water`) and the Linux state's `min_build`. The
//! manifest is signed by a rostered MACHINE, and the roster exists for the day one of
//! those machines is stolen: a thief's release with `min_build = 9_999_999_999` raised
//! the floor on every client that saw it, and nothing lowered it after the owner revoked
//! the machine, so every later genuine release was held below it forever. A floor now
//! records who raised it, and a revocation takes back exactly that machine's part.
//!
//! One pure rule ([`FloorSources::step`]) for both lanes, compiled into every macOS and
//! Linux build and tested here on either.

use serde::{Deserialize, Serialize};

/// The contributions to ONE floor, by the machine whose signed manifest raised it —
/// what lets a revocation take back exactly what the revoked machine asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorSources {
    /// The part no revocation lowers: every unattributed observation (a channel with
    /// no roster tier, a caller that names no machine), and the whole value a record
    /// written without provenance holds.
    #[serde(default)]
    pub base: u64,
    /// The highest value each machine asked for.
    #[serde(default)]
    pub machines: std::collections::BTreeMap<String, u64>,
}

impl FloorSources {
    /// Nothing recorded — skipped when written, so a floor nobody attributed is
    /// written exactly as before provenance existed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.base == 0 && self.machines.is_empty()
    }

    /// The floor these contributions make: the highest of them.
    #[must_use]
    pub fn value(&self) -> u64 {
        self.machines.values().copied().fold(self.base, u64::max)
    }

    /// One step of a floor that is recorded as `recorded` with these sources: the
    /// machines in `revoked` lose their part, then `raise` is recorded for `machine`
    /// (unattributed when `None`) unless that machine is itself revoked — a revoked
    /// machine's word counts for nothing. Returns the new floor and its sources, in
    /// the one form they are written in.
    ///
    /// FIRST the sources are made to explain `recorded`. A number the sources do not
    /// reach was raised by a writer that recorded no provenance (a build that predates
    /// it rewrote the record), so all of it becomes unattributed — exactly the
    /// pre-provenance floor, never a lower one. A source above the number (a hand
    /// edit; nothing this code writes) is capped at it.
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
        // Sources that name no machine say nothing the number does not.
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

    /// A REVOCATION TAKES BACK WHAT ITS MACHINE RAISED, AND NOTHING ELSE.
    #[test]
    fn a_revocation_takes_back_its_machines_part_and_nothing_else() {
        let none = FloorSources::default();
        let (v, s) = none.step(0, &[], 900, Some("m3"));
        let (v, s) = s.step(v, &[], 50, None);
        let (v, s) = s.step(v, &[], 9_999_999_999, Some("m11"));
        assert_eq!(v, 9_999_999_999);
        // An unrelated revocation moves nothing.
        let (same, s) = s.step(v, &revoked(&["m20"]), 0, None);
        assert_eq!(same, 9_999_999_999);
        // m11's revocation: back to what m3 and the unattributed observation asked.
        let (v, s) = s.step(same, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 900);
        // m11's word never counts again.
        let (v, s) = s.step(v, &revoked(&["m11"]), 10_000_000_000, Some("m11"));
        assert_eq!(v, 900);
        // A trusted machine still raises it.
        let (v, _) = s.step(v, &revoked(&["m11"]), 1_000, Some("m3"));
        assert_eq!(v, 1_000);
    }

    /// A NUMBER WITH NO PROVENANCE IS NEVER LOWERED. What an older build wrote (it
    /// drops the sources when it rewrites the record) is unattributed: the
    /// pre-provenance floor, which no revocation can take below.
    #[test]
    fn an_unexplained_floor_is_unattributed_and_a_hand_edit_is_capped() {
        let mut attributed = FloorSources::default();
        attributed.machines.insert("m11".into(), 9_000);
        // The record says 9_500 but the sources explain only 9_000: an older writer
        // raised it, so all of it stands.
        let (v, s) = attributed.step(9_500, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 9_500);
        assert!(s.is_empty(), "written as before provenance: {s:?}");
        // The record says 100 but a source claims 9_000: capped at the record.
        let (v, _) = attributed.step(100, &[], 0, None);
        assert_eq!(v, 100);
        let (v, _) = attributed.step(100, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 0, "the capped part was m11's alone");
        // NEGATIVE CONTROL: explained exactly, the same revocation takes it all back.
        let (v, _) = attributed.step(9_000, &revoked(&["m11"]), 0, None);
        assert_eq!(v, 0);
    }
}

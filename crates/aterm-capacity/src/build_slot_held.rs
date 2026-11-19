// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only coexistence with the old slot pools. This takes no old admission
//! ticket, epoch or fence: old clients arriving after a look cannot see new claims.

use aterm_sysprobe::slots::Holder;

/// An old reservation, with a native owner only when PID, birth and possession
/// were verified. Unknown held files reserve the whole machine, never zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// Native owner, when verified by the platform slot reader.
    pub process: Option<Holder>,
    /// Reserved cores, bounded by this look's physical cores.
    pub cores: u32,
    // Width supported by this reservation's declaration. Zero means absent.
    // A verified group of unspecified width can back one executing descendant.
    declared_cores: u32,
}

impl Held {
    #[cfg(any(unix, test))]
    pub(crate) fn verified(owner: Holder, cores: u32) -> Self {
        Self {
            process: Some(owner),
            cores,
            declared_cores: cores,
        }
    }

    #[cfg(any(unix, test))]
    pub(crate) fn unmeasured(process: Option<Holder>, physical: u32) -> Self {
        Self {
            process,
            cores: physical.max(1),
            declared_cores: 0,
        }
    }
}

/// One bounded observation of both legacy pools.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldSlots {
    /// Distinct reservations, not one row per duplicate lease descriptor.
    pub holders: Vec<Held>,
}

impl HeldSlots {
    #[cfg(unix)]
    fn unknown(cores: u32) -> Self {
        Self {
            holders: vec![Held::unmeasured(None, cores)],
        }
    }

    /// Cores held outside this command's verified ancestry, and the inherited
    /// ceiling when it is nested under an old reservation. Unknown owners are
    /// never excluded; ALONE uses the complete `holders`, including ancestors.
    #[cfg(test)]
    pub(crate) fn budget(&self, lineage: &[Holder]) -> (u32, Option<u32>) {
        self.budget_with(lineage, &[])
    }

    /// The old reservation and its verified ledger children overlap; they are
    /// not independent grants. Siblings share that ceiling. Unknown or detached
    /// ledger owners receive no overlap credit.
    pub(crate) fn budget_with(
        &self,
        lineage: &[Holder],
        claimed: &[(Vec<Holder>, u32)],
    ) -> (u32, Option<u32>) {
        // Physical files and complete lease IDs were deduplicated by the
        // native reader. Several DISTINCT executing leases may still have the
        // same native owner (one Targo scheduler dispatches many units).
        // They form one summed ceiling, not several one-core ancestor ceilings.
        let mut groups: Vec<Held> = Vec::new();
        for row in &self.holders {
            if let Some(owner) = row.process
                && let Some(group) = groups.iter_mut().find(|group| group.process == Some(owner))
            {
                group.cores = group.cores.saturating_add(row.cores);
                group.declared_cores = group.declared_cores.saturating_add(row.declared_cores);
            } else {
                groups.push(row.clone());
            }
        }
        let mut assigned = vec![0u32; groups.len()];
        for (line, cores) in claimed {
            // Charge a ledger claim to just its nearest old owner, so two old
            // reservations never both subtract the same child from physical use.
            if let Some((index, _)) = groups
                .iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    Some((index, line.iter().position(|p| Some(*p) == row.process)?))
                })
                .min_by_key(|(_, distance)| *distance)
            {
                assigned[index] = assigned[index].saturating_add(*cores);
            }
        }
        let mut outside = 0u32;
        let mut inherited = None;
        for (index, held) in groups.iter().enumerate() {
            if held.process.is_some_and(|owner| lineage.contains(&owner)) {
                let used = claimed
                    .iter()
                    .filter(|(line, _)| held.process.is_some_and(|owner| line.contains(&owner)))
                    .map(|(_, cores)| *cores)
                    .fold(0u32, u32::saturating_add);
                // Old copies may own several descriptors for one undeclared job.
                // Positively identifying that job backs one core, never one per
                // descriptor or the entire conservative outside reservation.
                let room = held.declared_cores.max(1).saturating_sub(used);
                inherited = Some(inherited.map_or(room, |n: u32| n.min(room)));
            } else {
                outside = outside.saturating_add(held.cores.saturating_sub(assigned[index]));
            }
        }
        (outside, inherited)
    }
}

/// Uses the account database home, never an ambient HOME or caller-supplied pool.
#[must_use]
pub fn read(cores: u32) -> HeldSlots {
    #[cfg(unix)]
    {
        let Some(home) = aterm_uds::account::home().and_then(|p| std::fs::canonicalize(p).ok())
        else {
            return HeldSlots::unknown(cores);
        };
        native::read(&home.join(aterm_sysprobe::slots::SLOT_DIR), cores)
    }
    #[cfg(not(unix))]
    {
        let _ = cores;
        HeldSlots::default()
    }
}

/// The command's current native ancestry, in the SAME epoch-microsecond identity
/// domain as the slot observer (ordinary memory sweeps use a different clock).
#[must_use]
pub fn lineage(pid: u32) -> Vec<Holder> {
    #[cfg(target_os = "macos")]
    {
        let mut line = Vec::new();
        let mut at = pid;
        while line.len() < 128 {
            let Ok(Some(p)) = aterm_sysprobe::kproc::record_read(at) else {
                break;
            };
            if p.zombie || line.iter().any(|h: &Holder| h.pid == p.pid) {
                break;
            }
            line.push(Holder {
                pid: p.pid,
                birth_us: p
                    .start_s
                    .saturating_mul(1_000_000)
                    .saturating_add(u64::from(p.start_us)),
            });
            if p.ppid <= 1 || p.ppid == at {
                break;
            }
            at = p.ppid;
        }
        line
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        Vec::new()
    }
}

#[cfg(unix)]
#[path = "build_slot_held_unix.rs"]
mod native;

#[cfg(test)]
#[path = "build_slot_held_tests.rs"]
mod tests;

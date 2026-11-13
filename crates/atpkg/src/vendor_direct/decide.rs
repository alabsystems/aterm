// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure vendor-lane upgrade, hold, and yank rollback decision logic.

use super::policy::Policy;
use super::version::{BuildDate, Version};

/// A complete, recorded vendor build (the active one, or the retained previous one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InstalledView {
    /// Its version.
    pub version: Version,
    /// Its store build number (`version.build_id()` for a recorded vendor build).
    pub build: u64,
    /// The signed `buildDate` it was installed with (claude only).
    pub build_date: Option<BuildDate>,
}

/// The program's active build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Installed {
    /// No build is active.
    Nothing,
    /// A legacy ALab index build (no `.vendor` record).
    Legacy {
        /// The index build number.
        build: u64,
        /// The vendor version it carries, if known.
        version: Option<Version>,
    },
    /// A complete, recorded vendor build.
    Vendor(InstalledView),
}

impl Installed {
    /// The active version, when one is known.
    #[must_use]
    pub(crate) const fn version(self) -> Option<Version> {
        match self {
            Self::Nothing => None,
            Self::Legacy { version, .. } => version,
            Self::Vendor(view) => Some(view.version),
        }
    }
}

/// What the vendor's `latest` pointer names, with the digest document's `buildDate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeadView {
    /// The head version.
    pub version: Version,
    /// The signed `buildDate` (claude only).
    pub build_date: Option<BuildDate>,
}

/// Everything [`decide`] reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inputs<'a> {
    /// The program deciding.
    pub program: &'a str,
    /// The active build.
    pub installed: Installed,
    /// The newest retained complete vendor build below the installed one.
    pub previous: Option<InstalledView>,
    /// The head, or `None` when it could not be reached or verified.
    pub head: Option<HeadView>,
    /// The signed yanks (fresh index, else the latch).
    pub policy: &'a Policy,
    /// The compiled floor ([`super::VendorSpec::floor`]).
    pub floor: Version,
    /// The stamp's high-water ([`super::ProgramStamp::high_water`]).
    pub high_water: Option<Version>,
    /// The local hold (`atpkg pin`).
    pub held: bool,
    /// The compiled legacy ceiling ([`super::VendorSpec::legacy_ceiling`]).
    pub legacy_ceiling: u64,
    /// A person asked for this program by name.
    pub asked: bool,
}

/// Why a head is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallReason {
    /// Nothing is installed.
    Fresh,
    /// The head is newer than the installed build.
    Newer,
    /// The installed build is a legacy index build whose version could not be read.
    ReplacesLegacy,
    /// The installed build is yanked and the head is admissible.
    ReplacesYanked,
}

/// Why nothing moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeepReason {
    /// No head this pass (unreachable or unverifiable).
    NoHead,
    /// The head is the installed version.
    UpToDate,
    /// The head is older than the installed version.
    HeadOlder,
    /// The head version is yanked.
    HeadYanked,
    /// The head is below the compiled floor.
    BelowFloor,
    /// The head is below this machine's high-water.
    BelowHighWater,
    /// The head's major is more than one above the installed major.
    MajorJump,
    /// The head's signed `buildDate` is older than the installed build's.
    BuildDateRegressed,
    /// A local hold suppresses the upgrade.
    Held,
    /// The installed build is a legacy index build that is no legacy pin (above the ceiling,
    /// or any build of a program no index pinned), so its version may be above the floor; its
    /// version could not be read and no person asked.
    LegacyUnread,
}

/// What the vendor lane does for one program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Fetch, verify and activate the head.
    Install {
        /// The head version.
        version: Version,
        /// Why.
        reason: InstallReason,
    },
    /// Leave the installed build (or nothing) as it is.
    Keep(KeepReason),
    /// Re-activate this retained build.
    RollbackTo(u64),
    /// The installed build is yanked and nothing admissible exists.
    Tombstone,
}

/// Decide one program.
#[must_use]
pub(crate) fn decide(i: &Inputs<'_>) -> Decision {
    let yanked = |v: Version| i.policy.is_yanked(i.program, v);
    let current = i.installed.version();
    let installed_yanked = match i.installed {
        Installed::Nothing => false,
        Installed::Legacy { build, version } => {
            i.policy.is_legacy_yanked(i.program, build) || version.is_some_and(yanked)
        }
        Installed::Vendor(view) => yanked(view.version),
    };
    if installed_yanked {
        if let Some(head) = i.head
            && !yanked(head.version)
            && head.version >= i.floor
            && current.is_none_or(|c| !major_jump(c, head.version))
        {
            return Decision::Install {
                version: head.version,
                reason: InstallReason::ReplacesYanked,
            };
        }
        if let Some(prev) = i.previous
            && !yanked(prev.version)
            && prev.version >= i.floor
        {
            return Decision::RollbackTo(prev.build);
        }
        return Decision::Tombstone;
    }
    if i.held && i.installed != Installed::Nothing {
        return Decision::Keep(KeepReason::Held);
    }
    let Some(head) = i.head else {
        return Decision::Keep(KeepReason::NoHead);
    };
    if let Some(c) = current {
        if head.version == c {
            return Decision::Keep(KeepReason::UpToDate);
        }
        if head.version < c {
            return Decision::Keep(KeepReason::HeadOlder);
        }
    }
    if yanked(head.version) {
        return Decision::Keep(KeepReason::HeadYanked);
    }
    if head.version < i.floor {
        return Decision::Keep(KeepReason::BelowFloor);
    }
    if i.installed != Installed::Nothing
        && i.high_water
            .is_some_and(|hw| !yanked(hw) && head.version < hw)
    {
        return Decision::Keep(KeepReason::BelowHighWater);
    }
    if current.is_some_and(|c| major_jump(c, head.version)) {
        return Decision::Keep(KeepReason::MajorJump);
    }
    let reason = match i.installed {
        Installed::Nothing => InstallReason::Fresh,
        Installed::Legacy {
            build,
            version: None,
        } if !(1..=i.legacy_ceiling).contains(&build) && !i.asked => {
            return Decision::Keep(KeepReason::LegacyUnread);
        }
        Installed::Legacy { version: None, .. } => InstallReason::ReplacesLegacy,
        Installed::Legacy { .. } => InstallReason::Newer,
        Installed::Vendor(view) => {
            if let (Some(h), Some(c)) = (head.build_date, view.build_date)
                && h < c
            {
                return Decision::Keep(KeepReason::BuildDateRegressed);
            }
            InstallReason::Newer
        }
    };
    Decision::Install {
        version: head.version,
        reason,
    }
}

fn major_jump(installed: Version, head: Version) -> bool {
    u64::from(head.major()) > u64::from(installed.major()) + 1
}

#[cfg(test)]
#[path = "decide_tests.rs"]
mod tests;

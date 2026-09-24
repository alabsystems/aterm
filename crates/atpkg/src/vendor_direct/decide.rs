// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The vendor lane's decision (design §1.3), pure: what to do with one program given the
//! installed build, the vendor's head and the signed policy.
//!
//! Only a signed yank of the installed version can move a machine backwards; a head that
//! regresses, freezes or vanishes is kept at, never followed.

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
    /// A legacy ALab index build (no `.vendor` record): its store build number, and the
    /// vendor version its artifact spells, when it spells one.
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
    /// The newest retained complete vendor build below the installed one — the only
    /// rollback target a yank may move to.
    pub previous: Option<InstalledView>,
    /// The head, or `None` when it could not be reached or verified.
    pub head: Option<HeadView>,
    /// The signed yanks (fresh index, else the latch).
    pub policy: &'a Policy,
    /// The compiled floor ([`super::VendorSpec::floor`]).
    pub floor: Version,
    /// The stamp's high-water ([`super::ProgramStamp::high_water`]). It binds only while a
    /// build is installed; a fresh install is bounded by the floor alone. After a
    /// `ReplacesYanked` install or a `RollbackTo`, the lane records the new build with
    /// `ProgramStamp::record_yank_move`, which is what lowers it.
    pub high_water: Option<Version>,
    /// The local hold (`atpkg pin`).
    pub held: bool,
    /// The compiled legacy ceiling ([`super::VendorSpec::legacy_ceiling`]).
    pub legacy_ceiling: u64,
    /// A person asked for this program by name (`update <program>`, `install <program>`,
    /// `claude update`): an unread legacy build above the ceiling is replaced all the same.
    pub asked: bool,
}

/// Why a head is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallReason {
    /// Nothing is installed.
    Fresh,
    /// The head is newer than the installed build.
    Newer,
    /// The installed build is a legacy index build whose version could not be read. No
    /// downgrade when the build is at or below the ceiling: the head is at or above the
    /// compiled floor, which is at or above the version of every legacy pin up to it
    /// ([`super::VendorSpec::legacy_ceiling`]); above it, a person asked.
    ReplacesLegacy,
    /// The installed build is yanked and the head is admissible — possibly older.
    ReplacesYanked,
}

/// Why nothing moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeepReason {
    /// No head this pass (unreachable or unverifiable).
    NoHead,
    /// The head is the installed version.
    UpToDate,
    /// The head is older than the installed version: never an automatic downgrade.
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
    /// The installed build is a legacy index build above the ceiling — pinned after this
    /// code, so its version may be above the floor — whose version could not be read, and
    /// no person asked: kept until its version is read or a person asks.
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
    /// Re-activate this retained build: the installed build is yanked and the head is not
    /// admissible.
    RollbackTo(u64),
    /// The installed build is yanked and nothing admissible exists: mark unrunnable.
    Tombstone,
}

/// Decide one program (design §1.3).
///
/// * A yanked installed build (by version, or a legacy build by its index build number)
///   is the one sanctioned downgrade: the head if it is admissible (not yanked, at or
///   above the floor, major at most one above), else the previous build if it is (not
///   yanked, at or above the floor), else tombstone. A hold never keeps a yanked build.
/// * Otherwise a hold keeps any active build, legacy included.
/// * Otherwise install iff nothing is installed or the head is newer than the installed
///   version (a legacy build of unknown version is replaced when its build is at or below
///   the ceiling, or a person asked), and the head is not yanked, at or above the floor,
///   at most one major above, and — when both carry one — not older by `buildDate`; and,
///   while a build is installed, at or above the high-water.
///   A yanked high-water is no mark: the signed move off it went below it on purpose.
///   With nothing installed the mark does not bind: the floor alone refuses a replayed
///   head, and a vendor pulling back a release it shipped never leaves the program
///   uninstallable.
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
        // Above the ceiling the floor vouches for nothing: the build may carry a version
        // above it, so an unread one moves only when a person asks.
        Installed::Legacy {
            build,
            version: None,
        } if build > i.legacy_ceiling && !i.asked => {
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

/// Whether `head` is more than one major above `installed`.
fn major_jump(installed: Version, head: Version) -> bool {
    u64::from(head.major()) > u64::from(installed.major()) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn at(text: &str) -> InstalledView {
        InstalledView {
            version: v(text),
            build: v(text).build_id(),
            build_date: None,
        }
    }

    fn vendor(text: &str) -> Installed {
        Installed::Vendor(at(text))
    }

    fn legacy(build: u64, version: Option<&str>) -> Installed {
        Installed::Legacy {
            build,
            version: version.map(v),
        }
    }

    fn head(text: &str) -> Option<HeadView> {
        Some(HeadView {
            version: v(text),
            build_date: None,
        })
    }

    fn date(text: &str) -> Option<BuildDate> {
        Some(BuildDate::parse(text).unwrap())
    }

    /// Nothing installed, head 2.1.281, floor 2.1.280, no yanks, no hold.
    fn base(policy: &Policy) -> Inputs<'_> {
        Inputs {
            program: "claude",
            installed: Installed::Nothing,
            previous: None,
            head: head("2.1.281"),
            policy,
            floor: v("2.1.280"),
            high_water: None,
            held: false,
            legacy_ceiling: CEILING,
            asked: false,
        }
    }

    /// The newest legacy pin (build 2026092201 of claude and codex).
    const CEILING: u64 = 2_026_092_201;

    fn install(text: &str, reason: InstallReason) -> Decision {
        Decision::Install {
            version: v(text),
            reason,
        }
    }

    fn keep(reason: KeepReason) -> Decision {
        Decision::Keep(reason)
    }

    #[test]
    fn fresh_newer_equal_and_older() {
        let none = Policy::default();
        assert_eq!(
            decide(&base(&none)),
            install("2.1.281", InstallReason::Fresh)
        );
        let newer = Inputs {
            installed: vendor("2.1.280"),
            ..base(&none)
        };
        assert_eq!(decide(&newer), install("2.1.281", InstallReason::Newer));
        let equal = Inputs {
            installed: vendor("2.1.281"),
            ..base(&none)
        };
        assert_eq!(decide(&equal), keep(KeepReason::UpToDate));
        let older = Inputs {
            installed: vendor("2.1.290"),
            ..base(&none)
        };
        assert_eq!(decide(&older), keep(KeepReason::HeadOlder));
        let gone = Inputs {
            installed: vendor("2.1.280"),
            head: None,
            ..base(&none)
        };
        assert_eq!(decide(&gone), keep(KeepReason::NoHead));
        let nothing = Inputs {
            head: None,
            ..base(&none)
        };
        assert_eq!(decide(&nothing), keep(KeepReason::NoHead));
    }

    #[test]
    fn floor_and_high_water_refuse_a_regressed_head() {
        let none = Policy::default();
        let below_floor = Inputs {
            head: head("2.1.279"),
            ..base(&none)
        };
        assert_eq!(decide(&below_floor), keep(KeepReason::BelowFloor));
        let installed_below_floor = Inputs {
            installed: vendor("2.1.100"),
            head: head("2.1.200"),
            ..base(&none)
        };
        assert_eq!(decide(&installed_below_floor), keep(KeepReason::BelowFloor));
        let below_hw = Inputs {
            installed: vendor("2.1.280"),
            high_water: Some(v("2.1.285")),
            head: head("2.1.283"),
            ..base(&none)
        };
        assert_eq!(decide(&below_hw), keep(KeepReason::BelowHighWater));
        // Nothing installed: the floor alone bounds the head.
        let fresh_below_hw = Inputs {
            high_water: Some(v("2.1.285")),
            ..base(&none)
        };
        assert_eq!(
            decide(&fresh_below_hw),
            install("2.1.281", InstallReason::Fresh)
        );
        let at_hw = Inputs {
            installed: vendor("2.1.280"),
            high_water: Some(v("2.1.283")),
            head: head("2.1.283"),
            ..base(&none)
        };
        assert_eq!(decide(&at_hw), install("2.1.283", InstallReason::Newer));
    }

    #[test]
    fn a_yanked_head_is_skipped() {
        let policy = Policy::from_entries(["claude@2.1.281"]);
        let fresh = base(&policy);
        assert_eq!(decide(&fresh), keep(KeepReason::HeadYanked));
        let upgrade = Inputs {
            installed: vendor("2.1.280"),
            ..base(&policy)
        };
        assert_eq!(decide(&upgrade), keep(KeepReason::HeadYanked));
        // Another program's yank of the same version does not bind.
        let other = Policy::from_entries(["codex@2.1.281"]);
        assert_eq!(
            decide(&base(&other)),
            install("2.1.281", InstallReason::Fresh)
        );
    }

    #[test]
    fn a_yanked_installed_version_moves_to_the_head_even_when_lower() {
        let policy = Policy::from_entries(["claude@2.1.285"]);
        let lower = Inputs {
            installed: vendor("2.1.285"),
            high_water: Some(v("2.1.285")),
            head: head("2.1.281"),
            ..base(&policy)
        };
        assert_eq!(
            decide(&lower),
            install("2.1.281", InstallReason::ReplacesYanked)
        );
        let higher = Inputs {
            installed: vendor("2.1.285"),
            head: head("2.1.286"),
            ..base(&policy)
        };
        assert_eq!(
            decide(&higher),
            install("2.1.286", InstallReason::ReplacesYanked)
        );
        // A hold never keeps a yanked build.
        let held = Inputs {
            held: true,
            ..lower
        };
        assert_eq!(
            decide(&held),
            install("2.1.281", InstallReason::ReplacesYanked)
        );
    }

    #[test]
    fn a_yanked_installed_version_rolls_back_when_the_head_is_not_admissible() {
        let policy = Policy::from_entries(["claude@2.1.285", "claude@2.1.286"]);
        let head_yanked = Inputs {
            installed: vendor("2.1.285"),
            previous: Some(at("2.1.282")),
            head: head("2.1.286"),
            ..base(&policy)
        };
        assert_eq!(
            decide(&head_yanked),
            Decision::RollbackTo(v("2.1.282").build_id())
        );
        let head_absent = Inputs {
            head: None,
            ..head_yanked
        };
        assert_eq!(
            decide(&head_absent),
            Decision::RollbackTo(v("2.1.282").build_id())
        );
        let head_same = Inputs {
            head: head("2.1.285"),
            ..head_yanked
        };
        assert_eq!(
            decide(&head_same),
            Decision::RollbackTo(v("2.1.282").build_id())
        );
        let head_below_floor = Inputs {
            head: head("2.1.200"),
            ..head_yanked
        };
        assert_eq!(
            decide(&head_below_floor),
            Decision::RollbackTo(v("2.1.282").build_id())
        );
        let head_major_jump = Inputs {
            head: head("4.0.0"),
            ..head_yanked
        };
        assert_eq!(
            decide(&head_major_jump),
            Decision::RollbackTo(v("2.1.282").build_id())
        );
    }

    #[test]
    fn nothing_admissible_tombstones() {
        let policy = Policy::from_entries(["claude@2.1.285", "claude@2.1.282"]);
        let no_previous = Inputs {
            installed: vendor("2.1.285"),
            head: None,
            ..base(&policy)
        };
        assert_eq!(decide(&no_previous), Decision::Tombstone);
        let previous_yanked = Inputs {
            previous: Some(at("2.1.282")),
            ..no_previous
        };
        assert_eq!(decide(&previous_yanked), Decision::Tombstone);
        let previous_below_floor = Inputs {
            previous: Some(at("2.1.100")),
            ..no_previous
        };
        assert_eq!(decide(&previous_below_floor), Decision::Tombstone);
    }

    #[test]
    fn a_major_jump_past_one_is_refused() {
        let none = Policy::default();
        let installed = vendor("2.1.290");
        let plus_one = Inputs {
            installed,
            head: head("3.0.0"),
            ..base(&none)
        };
        assert_eq!(decide(&plus_one), install("3.0.0", InstallReason::Newer));
        let plus_two = Inputs {
            installed,
            head: head("4.0.0"),
            ..base(&none)
        };
        assert_eq!(decide(&plus_two), keep(KeepReason::MajorJump));
        // A fresh install has no major to jump from.
        let fresh = Inputs {
            head: head("9.0.0"),
            ..base(&none)
        };
        assert_eq!(decide(&fresh), install("9.0.0", InstallReason::Fresh));
    }

    #[test]
    fn a_build_date_regression_is_refused_only_when_both_carry_one() {
        let none = Policy::default();
        let installed = Installed::Vendor(InstalledView {
            build_date: date("2026-09-21T20:55:27Z"),
            ..at("2.1.280")
        });
        let regressed = Inputs {
            installed,
            head: Some(HeadView {
                version: v("2.1.281"),
                build_date: date("2026-09-20T00:00:00Z"),
            }),
            ..base(&none)
        };
        assert_eq!(decide(&regressed), keep(KeepReason::BuildDateRegressed));
        let same_instant = Inputs {
            head: Some(HeadView {
                version: v("2.1.281"),
                build_date: date("2026-09-21T20:55:27Z"),
            }),
            ..regressed
        };
        assert_eq!(
            decide(&same_instant),
            install("2.1.281", InstallReason::Newer)
        );
        let head_undated = Inputs {
            head: head("2.1.281"),
            ..regressed
        };
        assert_eq!(
            decide(&head_undated),
            install("2.1.281", InstallReason::Newer)
        );
    }

    #[test]
    fn a_hold_keeps_any_active_build_but_needs_one() {
        let none = Policy::default();
        for installed in [
            vendor("2.1.280"),
            legacy(2_026_091_901, Some("2.1.278")),
            legacy(2_026_091_901, None),
        ] {
            let held = Inputs {
                installed,
                held: true,
                ..base(&none)
            };
            assert_eq!(decide(&held), keep(KeepReason::Held), "{installed:?}");
        }
        // A pin cannot outlive its build (`cmd_pin` refuses a program not installed), and
        // with nothing active there is nothing to hold.
        let held_fresh = Inputs {
            held: true,
            ..base(&none)
        };
        assert_eq!(
            decide(&held_fresh),
            install("2.1.281", InstallReason::Fresh)
        );
    }

    /// A legacy build's version, when its artifact spells one, keeps every no-downgrade
    /// rule; one of unknown version is replaced, but never below the high-water.
    #[test]
    fn a_legacy_build_is_never_downgraded() {
        let none = Policy::default();
        let lv = legacy(2_026_091_901, Some("2.1.290"));
        for (h, want) in [
            ("2.1.285", keep(KeepReason::HeadOlder)),
            ("2.1.290", keep(KeepReason::UpToDate)),
            ("2.1.291", install("2.1.291", InstallReason::Newer)),
            ("3.0.0", install("3.0.0", InstallReason::Newer)),
            ("4.0.0", keep(KeepReason::MajorJump)),
        ] {
            let i = Inputs {
                installed: lv,
                head: head(h),
                ..base(&none)
            };
            assert_eq!(decide(&i), want, "{h}");
        }
        let unknown = Inputs {
            installed: legacy(2_026_091_901, None),
            head: head("2.1.285"),
            ..base(&none)
        };
        assert_eq!(
            decide(&unknown),
            install("2.1.285", InstallReason::ReplacesLegacy)
        );
        let unknown_below_mark = Inputs {
            high_water: Some(v("2.1.290")),
            ..unknown
        };
        assert_eq!(
            decide(&unknown_below_mark),
            keep(KeepReason::BelowHighWater)
        );
    }

    /// THE LEGACY CEILING: an unread legacy build at or below the newest legacy pin this
    /// code knows is replaced on every lane (its version is at or below the floor); one
    /// above it — pinned after this code, its version possibly above the floor — is kept
    /// until its version is read or a person asks, and a person's door is still bounded by
    /// the floor, the high-water and the yanks.
    #[test]
    fn an_unread_legacy_build_above_the_ceiling_moves_only_when_a_person_asks() {
        let none = Policy::default();
        let at_ceiling = Inputs {
            installed: legacy(CEILING, None),
            ..base(&none)
        };
        assert_eq!(
            decide(&at_ceiling),
            install("2.1.281", InstallReason::ReplacesLegacy)
        );
        let above = Inputs {
            installed: legacy(CEILING + 1, None),
            ..base(&none)
        };
        assert_eq!(decide(&above), keep(KeepReason::LegacyUnread));
        let asked = Inputs {
            asked: true,
            ..above
        };
        assert_eq!(
            decide(&asked),
            install("2.1.281", InstallReason::ReplacesLegacy)
        );
        // Its version read, it is an ordinary legacy build again.
        for (version, want) in [
            ("2.1.290", keep(KeepReason::HeadOlder)),
            ("2.1.280", install("2.1.281", InstallReason::Newer)),
        ] {
            let read = Inputs {
                installed: legacy(CEILING + 1, Some(version)),
                ..above
            };
            assert_eq!(decide(&read), want, "{version}");
        }
        // The person's door keeps every other bound.
        let below_floor = Inputs {
            head: head("2.1.279"),
            ..asked
        };
        assert_eq!(decide(&below_floor), keep(KeepReason::BelowFloor));
        let below_mark = Inputs {
            high_water: Some(v("2.1.290")),
            ..asked
        };
        assert_eq!(decide(&below_mark), keep(KeepReason::BelowHighWater));
        let head_yanked = Policy::from_entries(["claude@2.1.281"]);
        let yanked = Inputs {
            policy: &head_yanked,
            ..asked
        };
        assert_eq!(decide(&yanked), keep(KeepReason::HeadYanked));
        // A signed yank of the build itself moves it unattended, as below the ceiling.
        let build_yanked = Policy::from_entries(["claude@2026092202"]);
        let revoked = Inputs {
            policy: &build_yanked,
            ..above
        };
        assert_eq!(
            decide(&revoked),
            install("2.1.281", InstallReason::ReplacesYanked)
        );
    }

    #[test]
    fn a_yanked_legacy_build_moves_like_a_yanked_vendor_build() {
        let by_build = Policy::from_entries(["claude@2026091901"]);
        let by_version = Policy::from_entries(["claude@2.1.278"]);
        for policy in [&by_build, &by_version] {
            let moves = Inputs {
                installed: legacy(2_026_091_901, Some("2.1.278")),
                ..base(policy)
            };
            assert_eq!(
                decide(&moves),
                install("2.1.281", InstallReason::ReplacesYanked)
            );
            let held = Inputs {
                held: true,
                ..moves
            };
            assert_eq!(
                decide(&held),
                install("2.1.281", InstallReason::ReplacesYanked),
                "a hold never keeps a yanked build"
            );
            let stranded = Inputs {
                head: None,
                ..moves
            };
            assert_eq!(decide(&stranded), Decision::Tombstone);
        }
        // Another legacy build's yank does not bind, and never touches a vendor build.
        let other = Policy::from_entries(["claude@2026091801"]);
        let untouched = Inputs {
            installed: legacy(2_026_091_901, Some("2.1.278")),
            ..base(&other)
        };
        assert_eq!(decide(&untouched), install("2.1.281", InstallReason::Newer));
        let on_vendor = Inputs {
            installed: vendor("2.1.280"),
            head: None,
            ..base(&by_build)
        };
        assert_eq!(decide(&on_vendor), keep(KeepReason::NoHead));
    }

    /// A yank of the installed build prefers the admissible head over the retained
    /// previous build.
    #[test]
    fn a_yanked_installed_version_prefers_the_head_to_a_rollback() {
        let policy = Policy::from_entries(["claude@2.1.285"]);
        let i = Inputs {
            installed: vendor("2.1.285"),
            previous: Some(at("2.1.282")),
            head: head("2.1.286"),
            high_water: Some(v("2.1.285")),
            ..base(&policy)
        };
        assert_eq!(
            decide(&i),
            install("2.1.286", InstallReason::ReplacesYanked)
        );
    }

    /// The pass after a signed downgrade: a head between the new build and the yanked one
    /// installs, whether or not the stamp's mark was moved off the yanked version yet.
    #[test]
    fn a_signed_downgrade_does_not_strand_the_next_update() {
        let policy = Policy::from_entries(["codex@0.157.0"]);
        let codex = |installed, h, high_water| Inputs {
            program: "codex",
            installed,
            previous: None,
            head: head(h),
            policy: &policy,
            floor: v("0.156.0"),
            high_water,
            held: false,
            legacy_ceiling: CEILING,
            asked: false,
        };
        let first = codex(vendor("0.157.0"), "0.156.1", Some(v("0.157.0")));
        assert_eq!(
            decide(&first),
            install("0.156.1", InstallReason::ReplacesYanked)
        );
        for mark in [Some(v("0.157.0")), Some(v("0.156.1"))] {
            let second = codex(vendor("0.156.1"), "0.156.2", mark);
            assert_eq!(
                decide(&second),
                install("0.156.2", InstallReason::Newer),
                "{mark:?}"
            );
            let regressed = codex(vendor("0.156.1"), "0.156.0", mark);
            assert_eq!(decide(&regressed), keep(KeepReason::HeadOlder), "{mark:?}");
        }
        // The same after a rollback, and for an accidental major release pulled back.
        let rolled_back = codex(vendor("0.156.0"), "0.156.3", Some(v("0.157.0")));
        assert_eq!(
            decide(&rolled_back),
            install("0.156.3", InstallReason::Newer)
        );
        let accident = Policy::from_entries(["claude@3.0.0"]);
        let pulled = Inputs {
            installed: vendor("3.0.0"),
            head: head("2.1.290"),
            high_water: Some(v("3.0.0")),
            ..base(&accident)
        };
        assert_eq!(
            decide(&pulled),
            install("2.1.290", InstallReason::ReplacesYanked)
        );
        let next = Inputs {
            installed: vendor("2.1.290"),
            head: head("2.1.291"),
            ..pulled
        };
        assert_eq!(decide(&next), install("2.1.291", InstallReason::Newer));
        // A mark that is not yanked still binds — why the lane moves it with
        // `record_yank_move` after the move.
        let lifted = Inputs {
            policy: &Policy::default(),
            ..next
        };
        assert_eq!(decide(&lifted), keep(KeepReason::BelowHighWater));
    }
}

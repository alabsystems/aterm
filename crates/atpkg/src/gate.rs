// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The apply-time floor/yank gate (§7): the pure decision, per program, of what an apply does
//! given a channel's pin, floors and `yanked` list (`"trust@4790"`).
//!
//! It runs at apply, not only at stage, because a build can be revoked after it was staged.
//! Every floor is read through [`Channel::min_build_for`]: build counters are per program, so
//! one program's floor never tombstones another whose numbering is smaller. Fail-closed: when
//! even the pinned build is below its floor or yanked, the program is tombstoned rather than
//! left on a revoked build. [`crate::flow`] runs the transaction that consumes these decisions.

use crate::manifest::Channel;

/// What an apply must do for one program in a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDecision {
    /// Stage and activate the pinned build: a fresh install, or a force-upgrade off an
    /// older, floored or yanked installed build.
    Install,
    /// The installed build already equals the (valid) pinned build — no-op.
    UpToDate,
    /// Even the pinned build is below the program's floor or yanked: there is no safe build,
    /// so the program is marked unrunnable.
    Tombstone,
    /// The channel does not pin this program — it is not part of this channel's set.
    NotPinned,
}

/// Whether `build` of `program` is on the channel's `yanked` list (`"<program>@<build>"`
/// entries; a malformed one never matches).
#[must_use]
pub fn is_yanked(channel: &Channel, program: &str, build: u64) -> bool {
    channel.yanked.iter().any(|entry| {
        entry
            .split_once('@')
            .is_some_and(|(p, b)| p == program && b.parse::<u64>() == Ok(build))
    })
}

/// Whether the installed `build` of `program` may keep running: at/above that program's floor
/// and not yanked. A local pin may hold a program back only while this holds; it never keeps
/// a revoked build alive. `None` (not installed) is ok.
#[must_use]
pub fn current_build_ok(channel: &Channel, program: &str, installed: Option<u64>) -> bool {
    match installed {
        Some(cur) => cur >= channel.min_build_for(program) && !is_yanked(channel, program, cur),
        None => true,
    }
}

/// The apply action for `program` in `channel`, given the `installed` build. Pure.
#[must_use]
pub fn decide(channel: &Channel, program: &str, installed: Option<u64>) -> ApplyDecision {
    let Some(&pinned) = channel.pin.get(program) else {
        return ApplyDecision::NotPinned;
    };
    // Fail-closed: a pin below the floor or yanked leaves nothing safe to run.
    if pinned < channel.min_build_for(program) || is_yanked(channel, program, pinned) {
        return ApplyDecision::Tombstone;
    }
    match installed {
        Some(cur) if cur == pinned => ApplyDecision::UpToDate,
        // Fresh install, or a force-upgrade off an older, floored or yanked build.
        _ => ApplyDecision::Install,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(min_build: u64, pin: &[(&str, u64)], yanked: &[&str]) -> Channel {
        Channel {
            name: "stable".into(),
            channel_build: 1,
            min_build,
            min_build_by_program: Default::default(),
            yanked: yanked.iter().map(|s| (*s).to_string()).collect(),
            pin: pin.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            pin_by_target: Default::default(),
        }
    }

    /// [`channel`] plus per-program floors (`min_build_by_program`).
    fn channel_floors(
        min_build: u64,
        per_program: &[(&str, u64)],
        pin: &[(&str, u64)],
        yanked: &[&str],
    ) -> Channel {
        Channel {
            min_build_by_program: per_program
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
            ..channel(min_build, pin, yanked)
        }
    }

    #[test]
    fn is_yanked_matches_program_at_build() {
        let ch = channel(0, &[], &["trust@4790", "ay@17", "garbage", "ny@notanum"]);
        assert!(is_yanked(&ch, "trust", 4790));
        assert!(is_yanked(&ch, "ay", 17));
        assert!(!is_yanked(&ch, "trust", 4791));
        assert!(!is_yanked(&ch, "ay", 18));
        assert!(!is_yanked(&ch, "ny", 0)); // malformed "ny@notanum" never matches
    }

    #[test]
    fn fresh_install_and_force_upgrade_yield_install() {
        // Floor below the pin (18 ≥ 10), so the pin is valid.
        let ch = channel(10, &[("ay", 18)], &[]);
        assert_eq!(decide(&ch, "ay", None), ApplyDecision::Install); // fresh
        assert_eq!(decide(&ch, "ay", Some(17)), ApplyDecision::Install); // upgrade
        assert_eq!(decide(&ch, "ay", Some(18)), ApplyDecision::UpToDate); // already current
    }

    #[test]
    fn pin_below_floor_or_yanked_tombstones() {
        // The pin itself is below min_build → no safe build.
        let low = channel(120, &[("ay", 100)], &[]);
        assert_eq!(decide(&low, "ay", None), ApplyDecision::Tombstone);
        assert_eq!(decide(&low, "ay", Some(100)), ApplyDecision::Tombstone);
        // The pin itself is yanked → tombstone even if it equals the installed build.
        let yanked = channel(0, &[("trust", 4790)], &["trust@4790"]);
        assert_eq!(
            decide(&yanked, "trust", Some(4790)),
            ApplyDecision::Tombstone
        );
        assert_eq!(decide(&yanked, "trust", None), ApplyDecision::Tombstone);
    }

    #[test]
    fn current_build_ok_gates_a_local_pin_hold() {
        let yanked = channel(0, &[("trust", 4800)], &["trust@4790"]);
        assert!(
            !current_build_ok(&yanked, "trust", Some(4790)),
            "yanked current build"
        );
        assert!(
            current_build_ok(&yanked, "trust", Some(4800)),
            "valid current build"
        );
        // Below-floor current build: not ok.
        let floored = channel(100, &[("ay", 120)], &[]);
        assert!(!current_build_ok(&floored, "ay", Some(90)), "below floor");
        assert!(
            current_build_ok(&floored, "ay", Some(110)),
            "at/above floor"
        );
        // Not installed: no live build to hold.
        assert!(current_build_ok(&floored, "ay", None));
    }

    // REGRESSION (audit 2026-09-15): a floor is per PROGRAM, because a build number is only
    // comparable to another build number of the same program. Comparing every program's pin
    // against ONE channel-wide number made the documented yank floor unusable: the only value
    // that did not tombstone unrelated programs was 0.
    #[test]
    fn a_program_floor_never_tombstones_a_program_it_does_not_name() {
        // The toolchain channel's shape: independent counters per member, with `trust`
        // floored at 7000 and re-pinned above it.
        let ch = channel_floors(
            0,
            &[("trust", 7000)],
            &[("trust", 7100), ("nn", 108), ("ty", 3007)],
            &[],
        );
        // trust: the floored installed build is force-upgraded and a local pin may not hold it.
        assert_eq!(decide(&ch, "trust", Some(6808)), ApplyDecision::Install);
        assert!(!current_build_ok(&ch, "trust", Some(6808)));
        assert_eq!(decide(&ch, "trust", Some(7100)), ApplyDecision::UpToDate);
        // Every other member is untouched: a smaller counter is not below a floor that was
        // never theirs.
        assert_eq!(decide(&ch, "nn", Some(108)), ApplyDecision::UpToDate);
        assert_eq!(decide(&ch, "ty", Some(3007)), ApplyDecision::UpToDate);
        assert_eq!(decide(&ch, "nn", None), ApplyDecision::Install);
        assert!(current_build_ok(&ch, "nn", Some(108)));
        assert!(current_build_ok(&ch, "ty", Some(3007)));
    }

    #[test]
    fn a_program_below_its_own_floor_tombstones_alone() {
        // `trust`'s pin is below `trust`'s floor: no safe build for trust …
        let ch = channel_floors(0, &[("trust", 7000)], &[("trust", 6808), ("nn", 108)], &[]);
        assert_eq!(decide(&ch, "trust", Some(6808)), ApplyDecision::Tombstone);
        assert_eq!(decide(&ch, "trust", None), ApplyDecision::Tombstone);
        // … and `nn` still installs in the same pass.
        assert_eq!(decide(&ch, "nn", None), ApplyDecision::Install);
        assert_eq!(decide(&ch, "nn", Some(108)), ApplyDecision::UpToDate);
    }

    #[test]
    fn a_program_entry_can_only_raise_the_channel_wide_floor() {
        // Max, never override: an entry below the channel-wide floor leaves that floor in
        // force, so a client that knows the key is never looser than one that does not.
        let ch = channel_floors(120, &[("ay", 50)], &[("ay", 130), ("ny", 130)], &[]);
        assert_eq!(ch.min_build_for("ay"), 120);
        assert!(!current_build_ok(&ch, "ay", Some(100)));
        assert_eq!(decide(&ch, "ay", Some(100)), ApplyDecision::Install);
        // A program with no entry keeps the channel-wide floor too.
        assert_eq!(ch.min_build_for("ny"), 120);
        assert!(!current_build_ok(&ch, "ny", Some(119)));
    }

    #[test]
    fn unpinned_program_is_not_part_of_the_channel() {
        let ch = channel(0, &[("ay", 18)], &[]);
        assert_eq!(decide(&ch, "dotfiles", None), ApplyDecision::NotPinned);
    }
}

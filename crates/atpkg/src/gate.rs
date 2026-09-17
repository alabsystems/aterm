// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The apply-time floor/yank gate (§7) — the pure decision, per program, of what an apply
//! must do given a channel's pin, `min_build`, and `yanked` list.
//!
//! Enforced **at apply** (not just at stage), because a build can be revoked *after* it
//! was staged: `min_build` is a force-upgrade floor and `yanked` is per-program
//! revocation (`"trust@4790"`). Every floor comparison reads
//! [`Channel::min_build_for`] — the program's OWN floor — never the channel-wide number
//! directly: build counters are per program (`nn = 108` and `trust-mc = 20065` are both
//! current), so a floor meant for one program must not tombstone another whose numbering
//! is simply smaller. The gate is fail-closed — if even the channel's *pinned*
//! build is below the floor or on the yank-list, there is no safe build to run, so the
//! program is **tombstoned** (marked unrunnable) rather than silently left on a revoked
//! build. The transactional stage→verify→flip that consumes these decisions is the rest
//! of Phase 4; this module is the decidable core, kept pure so the matrix is unit-tested.

use crate::manifest::Channel;

/// What an apply must do for one program in a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDecision {
    /// Stage + activate the channel's pinned build (a fresh install or a force-upgrade
    /// from an older/floored/yanked installed build to the valid pin).
    Install,
    /// The installed build already equals the (valid) pinned build — no-op.
    UpToDate,
    /// Even the channel's pinned build is below the program's floor or on the yank-list:
    /// there is no safe build, so the program is marked unrunnable. Never run a revoked
    /// build.
    Tombstone,
    /// The channel does not pin this program — it is not part of this channel's set.
    NotPinned,
}

/// Whether `build` of `program` is on the channel's `yanked` deny-list (entries are
/// `"<program>@<build>"`). A malformed entry is ignored (it can't match a real build).
#[must_use]
pub fn is_yanked(channel: &Channel, program: &str, build: u64) -> bool {
    channel.yanked.iter().any(|entry| {
        entry
            .split_once('@')
            .is_some_and(|(p, b)| p == program && b.parse::<u64>() == Ok(build))
    })
}

/// Whether the currently-installed `build` of `program` is itself still acceptable to keep
/// running: at/above THAT PROGRAM's floor ([`Channel::min_build_for`]) AND not yanked. This
/// is the guard a LOCAL PIN must pass before it may suppress an upgrade — a pin can freeze a
/// program on its current build only while that build is still gate-valid, never keep a
/// revoked/below-floor build alive (that is exactly what `decide` force-upgrades OFF of,
/// returning `Install` not `Tombstone`). `None` (not installed) is trivially valid — there is
/// no live build to hold.
#[must_use]
pub fn current_build_ok(channel: &Channel, program: &str, installed: Option<u64>) -> bool {
    match installed {
        Some(cur) => cur >= channel.min_build_for(program) && !is_yanked(channel, program, cur),
        None => true,
    }
}

/// Decide the apply action for `program` in `channel`, given the currently-`installed`
/// build (if any). See [`ApplyDecision`]. Pure — no I/O.
#[must_use]
pub fn decide(channel: &Channel, program: &str, installed: Option<u64>) -> ApplyDecision {
    let Some(&pinned) = channel.pin.get(program) else {
        return ApplyDecision::NotPinned;
    };
    // Fail-closed: if even the PIN is below the floor or yanked, nothing is safe to run.
    if pinned < channel.min_build_for(program) || is_yanked(channel, program, pinned) {
        return ApplyDecision::Tombstone;
    }
    match installed {
        Some(cur) if cur == pinned => ApplyDecision::UpToDate,
        // Fresh install, OR a force-upgrade from an older / floored / yanked installed
        // build to the valid pin.
        _ => ApplyDecision::Install,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn channel(min_build: u64, pin: &[(&str, u64)], yanked: &[&str]) -> Channel {
        Channel {
            name: "stable".into(),
            channel_build: 1,
            min_build,
            min_build_by_program: Default::default(),
            yanked: yanked.iter().map(|s| (*s).to_string()).collect(),
            pin: pin.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            pin_by_target: Default::default(),
            meta: BTreeMap::new(),
        }
    }

    /// [`channel`] plus PER-PROGRAM floors (`min_build_by_program`).
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
        // The exact guard a local pin must pass before it may suppress an upgrade: the
        // currently-installed build must itself be at/above the floor AND not yanked.
        // Yanked current build → NOT ok (a pin must never keep it running).
        let yanked = channel(0, &[("trust", 4800)], &["trust@4790"]);
        assert!(
            !current_build_ok(&yanked, "trust", Some(4790)),
            "yanked current build"
        );
        assert!(
            current_build_ok(&yanked, "trust", Some(4800)),
            "valid current build"
        );
        // Below-floor current build → NOT ok.
        let floored = channel(100, &[("ay", 120)], &[]);
        assert!(!current_build_ok(&floored, "ay", Some(90)), "below floor");
        assert!(
            current_build_ok(&floored, "ay", Some(110)),
            "at/above floor"
        );
        // Not installed → trivially ok (no live build to hold).
        assert!(current_build_ok(&floored, "ay", None));
    }

    #[test]
    fn yanking_an_installed_build_forces_upgrade_to_a_valid_pin() {
        // The channel re-pinned to a NEWER, non-yanked build; the old one is yanked.
        let ch = channel(0, &[("trust", 4800)], &["trust@4790"]);
        // Installed the now-yanked 4790 → force-upgrade to the valid pin 4800.
        assert_eq!(decide(&ch, "trust", Some(4790)), ApplyDecision::Install);
        assert_eq!(decide(&ch, "trust", Some(4800)), ApplyDecision::UpToDate);
    }

    // REGRESSION (audit 2026-09-15): a floor is per PROGRAM, because a build number is only
    // comparable to another build number of the same program. Comparing every program's pin
    // against ONE channel-wide number made the documented yank floor unusable: the only value
    // that did not tombstone unrelated programs was 0.
    #[test]
    fn a_program_floor_never_tombstones_a_program_it_does_not_name() {
        // The live toolchain channel's shape: independent counters per member (measured
        // 2026-09-15 — nn = 108, ty = 3007, trust = 6808). The owner floors `trust` below
        // 7000 to revoke a bad toolchain build and re-pins it above the floor.
        let ch = channel_floors(
            0,
            &[("trust", 7000)],
            &[("trust", 7100), ("nn", 108), ("ty", 3007)],
            &[],
        );
        // trust: the floor bites exactly as a floor should — the revoked installed build is
        // force-upgraded to the valid pin, and a local pin may not hold it.
        assert_eq!(decide(&ch, "trust", Some(6808)), ApplyDecision::Install);
        assert!(!current_build_ok(&ch, "trust", Some(6808)));
        assert_eq!(decide(&ch, "trust", Some(7100)), ApplyDecision::UpToDate);
        // Every other member is untouched. A counter three orders of magnitude smaller is
        // NOT "below the floor": that floor was never theirs.
        assert_eq!(decide(&ch, "nn", Some(108)), ApplyDecision::UpToDate);
        assert_eq!(decide(&ch, "ty", Some(3007)), ApplyDecision::UpToDate);
        assert_eq!(decide(&ch, "nn", None), ApplyDecision::Install);
        assert!(current_build_ok(&ch, "nn", Some(108)));
        assert!(current_build_ok(&ch, "ty", Some(3007)));
    }

    #[test]
    fn a_program_below_its_own_floor_tombstones_alone() {
        // `trust`'s own pin is below `trust`'s floor → no safe build for trust …
        let ch = channel_floors(0, &[("trust", 7000)], &[("trust", 6808), ("nn", 108)], &[]);
        assert_eq!(decide(&ch, "trust", Some(6808)), ApplyDecision::Tombstone);
        assert_eq!(decide(&ch, "trust", None), ApplyDecision::Tombstone);
        // … and `nn` still installs in the same pass, shims intact.
        assert_eq!(decide(&ch, "nn", None), ApplyDecision::Install);
        assert_eq!(decide(&ch, "nn", Some(108)), ApplyDecision::UpToDate);
    }

    #[test]
    fn a_program_entry_can_only_raise_the_channel_wide_floor() {
        // MAX, never override: an entry BELOW the channel-wide floor leaves the channel
        // floor in force, so a client that knows the key is never LOOSER than one that does
        // not (the single-counter app channel keeps behaving exactly as before).
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

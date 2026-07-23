// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The apply-time floor/yank gate (§7) — the pure decision, per program, of what an apply
//! must do given a channel's pin, `min_build`, and `yanked` list.
//!
//! Enforced **at apply** (not just at stage), because a build can be revoked *after* it
//! was staged: `min_build` is a force-upgrade floor and `yanked` is per-program
//! revocation (`"trust@4790"`). The gate is fail-closed — if even the channel's *pinned*
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
    /// Even the channel's pinned build is below `min_build` or on the yank-list: there is
    /// no safe build, so the program is marked unrunnable. Never run a revoked build.
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
/// running: at/above the channel floor AND not yanked. This is the guard a LOCAL PIN must
/// pass before it may suppress an upgrade — a pin can freeze a program on its current build
/// only while that build is still gate-valid, never keep a revoked/below-floor build alive
/// (that is exactly what `decide` force-upgrades OFF of, returning `Install` not `Tombstone`).
/// `None` (not installed) is trivially valid — there is no live build to hold.
#[must_use]
pub fn current_build_ok(channel: &Channel, program: &str, installed: Option<u64>) -> bool {
    match installed {
        Some(cur) => cur >= channel.min_build && !is_yanked(channel, program, cur),
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
    if pinned < channel.min_build || is_yanked(channel, program, pinned) {
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
            yanked: yanked.iter().map(|s| (*s).to_string()).collect(),
            pin: pin.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            meta: BTreeMap::new(),
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

    #[test]
    fn unpinned_program_is_not_part_of_the_channel() {
        let ch = channel(0, &[("ay", 18)], &[]);
        assert_eq!(decide(&ch, "dotfiles", None), ApplyDecision::NotPinned);
    }
}

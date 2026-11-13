// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Apply-time floor and yank decisions per program in a channel.

use crate::manifest::Channel;

/// What an apply must do for one program in a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDecision {
    /// Stage and activate the pinned build.
    Install,
    /// The installed build already equals the valid pinned build.
    UpToDate,
    /// The pinned build is below the program's floor or yanked.
    Tombstone,
    /// The channel does not pin this program.
    NotPinned,
}

/// Whether `build` of `program` is on the channel's `yanked` list (`program@<canonical-build>`).
#[must_use]
pub fn is_yanked(channel: &Channel, program: &str, build: u64) -> bool {
    channel.yanked.iter().any(|entry| {
        entry
            .split_once('@')
            .is_some_and(|(p, b)| p == program && crate::store::parse_build_name(b) == Some(build))
    })
}

/// Whether the `installed` build of `program` is at or above its floor and not yanked (`None` is
/// ok).
#[must_use]
pub fn current_build_ok(channel: &Channel, program: &str, installed: Option<u64>) -> bool {
    match installed {
        Some(cur) => cur >= channel.min_build_for(program) && !is_yanked(channel, program, cur),
        None => true,
    }
}

/// The apply action for `program` in `channel` given `installed`.
#[must_use]
pub fn decide(channel: &Channel, program: &str, installed: Option<u64>) -> ApplyDecision {
    let Some(&pinned) = channel.pin.get(program) else {
        return ApplyDecision::NotPinned;
    };
    if pinned < channel.min_build_for(program) || is_yanked(channel, program, pinned) {
        return ApplyDecision::Tombstone;
    }
    match installed {
        Some(cur) if cur == pinned => ApplyDecision::UpToDate,
        _ => ApplyDecision::Install,
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;

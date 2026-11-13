// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Supported harness lanes (relaunch on exit, live upgrade, restart in place) per [`Program`].
use aterm_phase::Program;

/// Harness lanes implemented for a [`Program`] ([`lanes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes {
    /// Relaunch on exit in the same tab and conversation ([`super::relaunch::on_exit`]).
    pub relaunch: bool,
    /// Live upgrade of an idle agent onto a newer installed build ([`super::upgrade_drive`]).
    pub upgrade: bool,
    /// In-place restart at an idle point ([`super::relaunch::restart_here`]).
    pub restart_in_place: bool,
}

impl Lanes {
    /// Empty lane set (`Program::Generic`).
    pub const NONE: Lanes = Lanes {
        relaunch: false,
        upgrade: false,
        restart_in_place: false,
    };
}

/// Returns the [`Lanes`] supported by `program`.
#[must_use]
pub const fn lanes(program: Program) -> Lanes {
    match program {
        Program::Claude => Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: true,
        },
        Program::Codex => Lanes {
            relaunch: true,
            upgrade: false,
            restart_in_place: false,
        },
        Program::Muse | Program::Agy => Lanes {
            relaunch: true,
            upgrade: true,
            restart_in_place: false,
        },
        Program::Grok | Program::Kimi | Program::Generic => Lanes::NONE,
    }
}

/// Returns whether the background sweep carries an in-flight relaunch for `program`.
#[must_use]
pub const fn sweep_carries_relaunch(program: Program) -> bool {
    match program {
        Program::Claude | Program::Codex => true,
        Program::Muse | Program::Agy | Program::Grok | Program::Kimi | Program::Generic => false,
    }
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;

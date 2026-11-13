// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Lanes, lanes, sweep_carries_relaunch};
use aterm_phase::Program;

const NONE: Lanes = Lanes::NONE;

/// Muse Code and agy are relaunched on exit and upgraded live, never
/// restarted in place; their hosts carry the relaunch.
#[test]
fn muse_code_and_agy_are_relaunched_on_exit_and_upgraded() {
    for p in [Program::Muse, Program::Agy] {
        assert_eq!(
            lanes(p),
            Lanes {
                relaunch: true,
                upgrade: true,
                restart_in_place: false,
            },
            "{p:?}"
        );
        assert_ne!(lanes(p), lanes(Program::Claude), "{p:?}");
        assert!(!sweep_carries_relaunch(p), "{p:?}: its host carries it");
    }
    assert!(sweep_carries_relaunch(Program::Codex));
}

/// Every agent has a lane: no `upgrade::Agent` is left with all three
/// false, and the one program with none is the one no reader hosts.
#[test]
fn every_agent_has_a_lane_and_only_generic_has_none() {
    use super::super::upgrade::Agent;
    let unlaned: Vec<Agent> = Agent::ALL
        .into_iter()
        .filter(|agent| lanes(agent.program()) == NONE)
        .collect();
    assert_eq!(unlaned, []);
    assert_eq!(lanes(Program::Generic), NONE, "the negative control");
}

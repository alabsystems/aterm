// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_session_held_off_for_its_claim_grace_is_not_the_hosts() {
    let hosting = HarnessHosting {
        supervising: true,
        agents: crate::harness_host::Admission::of(&Default::default()),
        held_off: vec!["s-adopted".to_string()],
        relaunching: Default::default(),
        autopilot: aterm_agent::supervise::Autopilot::Cruise,
    };
    assert!(!hosting.hosts("s-adopted", None, aterm_phase::Program::Claude));
    assert!(hosting.hosts("s-other", None, aterm_phase::Program::Claude));
    assert!(!HarnessHosting::default().hosts("s-other", None, aterm_phase::Program::Claude));
}

#[test]
fn an_off_session_is_not_the_hosts() {
    use aterm_agent::supervise::Autopilot;
    use aterm_phase::Program;
    let codex_off = aterm_agent::supervise::SupervisorConfig {
        off: vec![Program::Codex],
        ..Default::default()
    };
    let mut hosting = HarnessHosting {
        supervising: true,
        agents: crate::harness_host::Admission::of(&codex_off),
        ..HarnessHosting::default()
    };
    assert!(hosting.hosts("s-a", None, Program::Claude));
    assert!(hosting.hosts("s-a", Some(Autopilot::Land), Program::Claude));
    assert!(!hosting.hosts("s-a", Some(Autopilot::Off), Program::Claude));
    hosting.autopilot = Autopilot::Off;
    assert!(!hosting.hosts("s-a", None, Program::Claude));
    assert!(hosting.hosts("s-a", Some(Autopilot::Cruise), Program::Claude));
    assert!(!HarnessHosting::default().hosts("s-a", Some(Autopilot::Cruise), Program::Claude));
    assert!(!hosting.hosts("s-a", Some(Autopilot::Cruise), Program::Codex));
}

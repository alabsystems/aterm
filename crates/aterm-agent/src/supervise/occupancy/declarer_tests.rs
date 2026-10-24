// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The declarer's proof, by chains of kernel records.

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

const PTY: AgentId = AgentId {
    pid: 100,
    birth_us: 1_000,
};

const AGENT: AgentId = AgentId {
    pid: 200,
    birth_us: 2_000,
};

const fn link(pid: u32, birth_us: i64, ppid: u32) -> ProcLink {
    ProcLink {
        pid,
        birth_us,
        ppid,
    }
}

fn under_the_agent() -> Vec<ProcLink> {
    vec![
        link(400, 4_000, 300),
        link(300, 3_000, 200),
        link(200, 2_000, 100),
        link(100, 1_000, 1),
        link(1, 0, 0),
    ]
}

#[test]
fn a_descendant_of_the_pty_child_is_admitted() {
    let chain = under_the_agent();
    assert_eq!(prove(&chain, PTY, Some(AGENT)), Ok(AGENT));
    assert_eq!(prove(&chain, PTY, None), Ok(AGENT), "the PTY child's child");
    assert_eq!(prove(&chain[3..], PTY, None), Ok(PTY), "the tab's program");
    assert_eq!(
        prove(&chain[2..], PTY, Some(AGENT)),
        Ok(AGENT),
        "the agent itself"
    );
}

#[test]
fn a_chain_under_another_sessions_pty_child_is_refused() {
    let other = vec![link(910, 9_100, 900), link(900, 9_000, 1), link(1, 0, 0)];
    let outside = vec![link(77, 7_700, 1), link(1, 0, 0)];
    for chain in [&other, &outside] {
        assert_eq!(
            prove(chain, PTY, Some(AGENT)),
            Err(DeclarerRefusal::OtherSession)
        );
        assert_eq!(prove(chain, PTY, None), Err(DeclarerRefusal::OtherSession));
    }
    let admitted = fault::with(Fault::AwaitFromAnyone, || prove(&other, PTY, Some(AGENT)));
    assert_eq!(admitted, Ok(AGENT));
}

#[test]
fn no_peer_is_refused() {
    assert_eq!(prove(&[], PTY, Some(AGENT)), Err(DeclarerRefusal::NoPeer));
    assert_eq!(prove(&[], PTY, None), Err(DeclarerRefusal::NoPeer));
}

#[test]
fn a_child_born_before_its_parent_is_not_in_the_tree() {
    let mut reused = under_the_agent();
    reused[2].birth_us = 3_500;
    assert_eq!(
        prove(&reused, PTY, Some(AGENT)),
        Err(DeclarerRefusal::NotInTree)
    );
    assert_eq!(prove(&reused, PTY, None), Err(DeclarerRefusal::NotInTree));
    let stranger = vec![link(77, 500, 100), link(100, 1_000, 1)];
    assert_eq!(prove(&stranger, PTY, None), Err(DeclarerRefusal::NotInTree));
    let mut skipped = under_the_agent();
    skipped.remove(1);
    assert_eq!(prove(&skipped, PTY, None), Err(DeclarerRefusal::NotInTree));
    let mut reborn = under_the_agent();
    reborn[3].birth_us = 1_500;
    reborn[2].birth_us = 2_000;
    assert_eq!(prove(&reborn, PTY, None), Err(DeclarerRefusal::NotInTree));
    let beside = vec![link(250, 2_500, 100), link(100, 1_000, 1)];
    assert_eq!(
        prove(&beside, PTY, Some(AGENT)),
        Err(DeclarerRefusal::NotInTree)
    );
    assert_eq!(prove(&under_the_agent(), PTY, Some(AGENT)), Ok(AGENT));
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The declarer's proof, by chains of kernel records.

use super::*;
use crate::supervise::occupancy::fault::{self, Fault};

/// This session's PTY child.
const PTY: AgentId = AgentId {
    pid: 100,
    birth_us: 1_000,
};

/// The hosted agent, the PTY child's child.
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

/// A build script the agent runs, up to launchd.
fn under_the_agent() -> Vec<ProcLink> {
    vec![
        link(400, 4_000, 300),
        link(300, 3_000, 200),
        link(200, 2_000, 100),
        link(100, 1_000, 1),
        link(1, 0, 0),
    ]
}

/// A DESCENDANT OF THE TAB IS ADMITTED, for the hosted agent where one is named, else for the PTY
/// child's own child, else for the PTY child itself (the peer is the tab's program).
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

/// A CHAIN UNDER ANOTHER SESSION'S PTY CHILD IS ANOTHER SESSION'S, and so is one under no tab.
/// NEGATIVE CONTROL: `AwaitFromAnyone` admits it.
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

/// A CONNECTION WITH NO KERNEL PEER (the bridge, a relay) proves nothing.
#[test]
fn no_peer_is_refused() {
    assert_eq!(prove(&[], PTY, Some(AGENT)), Err(DeclarerRefusal::NoPeer));
    assert_eq!(prove(&[], PTY, None), Err(DeclarerRefusal::NoPeer));
}

/// A CHAIN THAT DOES NOT HOLD UP IS NOT IN THE TREE: a parent born after its child (its pid
/// reused), whether or not the host names its agent, and a stranger whose dead parent's pid the
/// PTY child took; a link whose parent is not the next, the tab's pid reborn, and a process in
/// the tab that is not under the agent it hosts (the person's own shell). CONTROL: the chain as
/// recorded is admitted.
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

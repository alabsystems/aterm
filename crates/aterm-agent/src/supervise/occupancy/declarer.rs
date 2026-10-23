// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO MAY TOUCH A SESSION'S WAKES.

use serde::{Deserialize, Serialize};

/// A PROCESS NAMED SO A REUSED PID CANNOT PASS FOR IT: its pid and its kernel birth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentId {
    /// Its pid.
    pub pid: u32,
    /// When the kernel started it, unix µs.
    pub birth_us: i64,
}

/// One process of the peer's ancestry, as the kernel records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcLink {
    /// Its pid.
    pub pid: u32,
    /// When the kernel started it, unix µs.
    pub birth_us: i64,
    /// Its parent's pid.
    pub ppid: u32,
}

impl ProcLink {
    /// The process it names.
    #[must_use]
    pub const fn id(self) -> AgentId {
        AgentId {
            pid: self.pid,
            birth_us: self.birth_us,
        }
    }
}

/// Why a peer may not touch this session's wakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarerRefusal {
    /// The connection carries no kernel peer (the bridge, a relay).
    NoPeer,
    /// The peer's proven ancestry never meets this session's PTY child: another session's.
    OtherSession,
    /// The peer's ancestry does not hold up (a parent born after its child: a reused pid).
    NotInTree,
}

impl DeclarerRefusal {
    /// Its words, as `ERR denied wake: <words>` says them.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            DeclarerRefusal::NoPeer => "no kernel peer",
            DeclarerRefusal::OtherSession => "another session's",
            DeclarerRefusal::NotInTree => "not in this session's tree",
        }
    }
}

/// THE PROOF THAT A PEER IS OF THIS SESSION'S TREE.
/// # Errors
/// [`DeclarerRefusal`]: no peer, a chain that never meets the tab, or one that does not hold up.
pub fn prove(
    chain: &[ProcLink],
    pty_child: AgentId,
    agent: Option<AgentId>,
) -> Result<AgentId, DeclarerRefusal> {
    use super::fault::{Fault, armed};
    let Some(peer) = chain.first() else {
        return Err(DeclarerRefusal::NoPeer);
    };
    let mut child = *peer;
    for (at, link) in chain.iter().enumerate() {
        if at > 0 {
            if link.pid != child.ppid || link.birth_us > child.birth_us {
                return Err(DeclarerRefusal::NotInTree);
            }
            child = *link;
        }
        if link.pid != pty_child.pid {
            continue;
        }
        if link.birth_us != pty_child.birth_us {
            return Err(DeclarerRefusal::NotInTree);
        }
        let below = &chain[..=at];
        return match agent {
            Some(agent) if below.iter().any(|l| l.id() == agent) => Ok(agent),
            Some(_) => Err(DeclarerRefusal::NotInTree),
            None => Ok(at
                .checked_sub(1)
                .map_or(pty_child, |under| chain[under].id())),
        };
    }
    if armed(Fault::AwaitFromAnyone) {
        return Ok(agent.unwrap_or(pty_child));
    }
    Err(DeclarerRefusal::OtherSession)
}

#[cfg(test)]
#[path = "declarer_tests.rs"]
mod tests;

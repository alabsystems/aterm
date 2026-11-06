// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stable same-account ancestry observations. No process-control, admission or
//! settlement authority is derived from an observed numeric path.
use super::{Birth, Identity, read_lineage};
use std::time::Instant;
const MAX_ANCESTORS: usize = 128;
#[derive(Clone, Copy)]
struct Peer {
    pid: u32,
    birth: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Node {
    identity: Identity,
    parent: u32,
}
fn native_node(pid: u32) -> Option<Node> {
    let lineage = read_lineage(i32::try_from(pid).ok()?)?;
    Some(Node {
        identity: lineage.identity,
        parent: lineage.parent,
    })
}
/// Each node comes from a native same-user lineage read. Reread the entire
/// bounded path, including identity and parent, rather than trusting PID links.
fn descendant(
    peer: Peer,
    shell: Peer,
    deadline: Instant,
    mut read: impl FnMut(u32) -> Option<Node>,
) -> bool {
    let mut path = Vec::new();
    let mut pid = peer.pid;
    for _ in 0..MAX_ANCESTORS {
        if Instant::now() >= deadline || pid <= 1 || path.iter().any(|(seen, _)| *seen == pid) {
            return false;
        }
        let Some(node) = read(pid) else {
            return false;
        };
        if u32::try_from(node.identity.pid).ok() != Some(pid) {
            return false;
        }
        if path.is_empty() && node.identity.birth.0 != peer.birth {
            return false;
        }
        path.push((pid, node));
        if pid == shell.pid {
            if node.identity.birth.0 != shell.birth {
                return false;
            }
            return path
                .iter()
                .rev()
                .all(|(pid, node)| Instant::now() < deadline && read(*pid) == Some(*node));
        }
        pid = node.parent;
    }
    false
}
/// Read and reread a bounded root-or-descendant path, with native births and
/// parents stable throughout. Each kernel lineage read must belong to this UID.
/// The root tuple must come from the caller's actual ownership receipt whenever
/// this observation is used to authenticate access to existing resources.
#[must_use]
pub fn stable_descendant(
    peer_pid: u32,
    peer_birth: Birth,
    root_pid: u32,
    root_birth: Birth,
    deadline: Instant,
) -> bool {
    root_pid > 1
        && root_birth.0 > 0
        && peer_birth.0 > 0
        && descendant(
            Peer {
                pid: peer_pid,
                birth: peer_birth.0,
            },
            Peer {
                pid: root_pid,
                birth: root_birth.0,
            },
            deadline,
            native_node,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn node(pid: u32, birth: u64, parent: u32) -> Node {
        Node {
            identity: Identity {
                pid: pid as i32,
                birth: Birth(birth),
                idversion: None,
            },
            parent,
        }
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }
    #[test]
    fn stable_birth_parent_chain_refuses_reuse_reparenting_cycles_and_expiry() {
        let peer = Peer { pid: 30, birth: 3 };
        let shell = Peer { pid: 10, birth: 1 };
        let nodes = [node(30, 3, 20), node(20, 2, 10), node(10, 1, 1)];
        assert!(descendant(peer, shell, deadline(), |pid| nodes
            .iter()
            .find(|n| n.identity.pid == pid as i32)
            .copied()));
        assert!(!descendant(
            Peer { birth: 4, ..peer },
            shell,
            deadline(),
            |pid| nodes.iter().find(|n| n.identity.pid == pid as i32).copied()
        ));
        for reused in [false, true] {
            let mut reads = 0;
            assert!(!descendant(peer, shell, deadline(), |pid| {
                reads += 1;
                let mut n = nodes
                    .iter()
                    .find(|n| n.identity.pid == pid as i32)
                    .copied()?;
                if reads > 3 && pid == 20 {
                    if reused {
                        n.identity.birth = Birth(99);
                    } else {
                        n.parent = 1;
                    }
                }
                Some(n)
            }));
        }
        assert!(!descendant(peer, shell, deadline(), |pid| Some(node(
            pid,
            if pid == 30 { 3 } else { 2 },
            30
        ))));
        assert!(!descendant(peer, shell, Instant::now(), |_| panic!(
            "expired chain read"
        )));
        assert!(!descendant(peer, shell, deadline(), |pid| Some(node(
            pid,
            if pid == 30 { 3 } else { 2 },
            pid + 1
        ))));
    }
}

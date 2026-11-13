// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stable same-account ancestry observations. No process-control, admission or
//! settlement authority is derived from an observed numeric path.
use super::{Birth, Identity, Lineage, read_lineage};
use std::{io, time::Instant};
const MAX_ANCESTORS: usize = 128;

/// A read must start and finish within the observation window. In particular,
/// the last read cannot escape a deadline checked only before it.
fn completed_before<T>(
    deadline: Instant,
    mut now: impl FnMut() -> Instant,
    read: impl FnOnce() -> T,
) -> Option<T> {
    if now() >= deadline {
        return None;
    }
    let answer = read();
    (now() < deadline).then_some(answer)
}

/// A complete bounded same-account parent path observed and reread from the
/// kernel. It grants no process, admission or settlement authority.
#[derive(Debug)]
pub struct Ancestry {
    nodes: Vec<Lineage>,
}
impl Ancestry {
    /// Capture the actual caller through its parent-one boundary. Unknown,
    /// changed, cyclic, foreign-account and excessive paths are refusals.
    /// # Errors
    /// An invalid PID (`InvalidInput`), an expired deadline (`TimedOut`), an
    /// unavailable native record, a changed identity, a cycle or an excessive
    /// path. Record failures identify the PID without inferring why it is unreadable.
    pub fn capture(pid: u32, birth: Birth, deadline: Instant) -> io::Result<Self> {
        Self::capture_with(pid, birth, deadline, read_lineage)
    }

    fn capture_with(
        pid: u32,
        birth: Birth,
        deadline: Instant,
        mut read: impl FnMut(i32) -> Option<Lineage>,
    ) -> io::Result<Self> {
        let mut nodes = Vec::new();
        let mut next = pid;
        for _ in 0..MAX_ANCESTORS {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "native ancestry capture expired",
                ));
            }
            let native_pid = i32::try_from(next)
                .ok()
                .filter(|pid| *pid > 1)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("invalid native ancestry pid {next}"),
                    )
                })?;
            if nodes
                .iter()
                .any(|node: &Lineage| node.identity.pid == native_pid)
            {
                return Err(io::Error::other(format!(
                    "native ancestry cycles at pid {next}"
                )));
            }
            let node = read(native_pid).ok_or_else(|| {
                io::Error::other(format!(
                    "native same-account ancestry record unavailable for pid {next}"
                ))
            })?;
            if node.identity.pid as u32 != next || nodes.is_empty() && node.identity.birth != birth
            {
                return Err(io::Error::other(format!(
                    "native ancestry identity changed for pid {next}"
                )));
            }
            next = node.parent;
            nodes.push(node);
            if next == 1 {
                let ancestry = Self { nodes };
                ancestry.check_current_with(deadline, read)?;
                return Ok(ancestry);
            }
        }
        Err(io::Error::other(format!(
            "native ancestry exceeds {MAX_ANCESTORS} records"
        )))
    }

    /// Exact native observations, starting with the requesting process.
    #[must_use]
    pub fn nodes(&self) -> &[Lineage] {
        &self.nodes
    }

    /// Test seam: `pid`'s path as far as this account reads it natively, without
    /// [`Self::capture`]'s completeness. It holds `pid`'s own record, then each parent's, up to
    /// pid 1 or to the first parent whose same-account record is unavailable. Under SSH a
    /// root-owned `sshd` is an ancestor of every process the session starts, so there no
    /// complete path exists to capture. This is for a test of what reads an ancestry's nodes,
    /// never of a refusal [`Self::capture`] owes.
    /// # Errors
    /// `pid`'s own record is unavailable, or its birth is not `birth`.
    #[cfg(any(test, debug_assertions))]
    pub fn readable_prefix(pid: u32, birth: Birth) -> io::Result<Self> {
        Self::readable_prefix_with(pid, birth, read_lineage)
    }

    #[cfg(any(test, debug_assertions))]
    fn readable_prefix_with(
        pid: u32,
        birth: Birth,
        mut read: impl FnMut(i32) -> Option<Lineage>,
    ) -> io::Result<Self> {
        let mut nodes: Vec<Lineage> = Vec::new();
        let mut next = pid;
        while nodes.len() < MAX_ANCESTORS
            && next > 1
            && !nodes.iter().any(|node| node.identity.pid as u32 == next)
        {
            let Some(node) = i32::try_from(next).ok().and_then(&mut read) else {
                break;
            };
            if node.identity.pid as u32 != next {
                break;
            }
            next = node.parent;
            nodes.push(node);
        }
        match nodes.first() {
            Some(own) if own.identity.birth == birth => Ok(Self { nodes }),
            Some(_) => Err(io::Error::other(format!(
                "native ancestry identity changed for pid {pid}"
            ))),
            None => Err(io::Error::other(format!(
                "native same-account ancestry record unavailable for pid {pid}"
            ))),
        }
    }

    /// Reread every original birth, exec generation, group and parent.
    /// # Errors
    /// A read changes or becomes unavailable (identifying its PID), or the
    /// deadline expires (`TimedOut`).
    pub fn check_current(&self, deadline: Instant) -> io::Result<()> {
        self.check_current_with(deadline, read_lineage)
    }

    fn check_current_with(
        &self,
        deadline: Instant,
        mut read: impl FnMut(i32) -> Option<Lineage>,
    ) -> io::Result<()> {
        for node in self.nodes.iter().rev() {
            let current = completed_before(deadline, Instant::now, || read(node.identity.pid))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "native ancestry revalidation expired",
                    )
                })?;
            let current = current.ok_or_else(|| {
                io::Error::other(format!("native same-account ancestry record unavailable for pid {} during revalidation", node.identity.pid))
            })?;
            if current != *node {
                return Err(io::Error::other(format!(
                    "native ancestry changed for pid {} during revalidation",
                    node.identity.pid
                )));
            }
        }
        Ok(())
    }
}
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
            return path.iter().rev().all(|(pid, node)| {
                completed_before(deadline, Instant::now, || read(*pid)) == Some(Some(*node))
            });
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
#[path = "process_ancestry_tests.rs"]
mod tests;

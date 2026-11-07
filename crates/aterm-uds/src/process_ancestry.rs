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
    fn a_final_native_read_must_finish_before_its_deadline() {
        use std::cell::Cell;
        let start = Instant::now();
        let end = start + Duration::from_secs(60);
        let clock = Cell::new(start);
        assert_eq!(completed_before(end, || clock.get(), || 7), Some(7));
        for finished in [end, end + Duration::from_secs(1)] {
            clock.set(start);
            assert_eq!(
                completed_before(
                    end,
                    || clock.get(),
                    || {
                        clock.set(finished);
                        7
                    }
                ),
                None
            );
        }
        assert_eq!(
            completed_before(end, || end, || panic!("expired read started")),
            None::<()>
        );
    }
    #[test]
    fn actual_native_identity_checks_birth_and_expiry() {
        let pid = std::process::id();
        let identity = super::super::read_identity(pid as i32).unwrap();
        // SSH can put a foreign-account process in the parent chain. A complete
        // same-account path is then correctly unavailable; the native identity
        // itself must still be readable and revalidated.
        assert!(stable_descendant(
            pid,
            identity.birth,
            pid,
            identity.birth,
            deadline()
        ));
        assert!(Ancestry::capture(pid, Birth(identity.birth.0 + 1), deadline()).is_err());
        assert_eq!(
            Ancestry::capture(pid, identity.birth, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn complete_ancestry_revalidates_every_node_and_refuses_unavailable_parents() {
        let lineage = |pid, birth, parent| Lineage {
            identity: node(pid, birth, parent).identity,
            uniqueid: birth,
            parent,
            pgid: 10,
            launcher_version: None,
            tty: None,
        };
        let nodes = [lineage(30, 3, 20), lineage(20, 2, 10), lineage(10, 1, 1)];
        let read = |pid| nodes.iter().find(|n| n.identity.pid == pid).copied();
        let path = Ancestry::capture_with(30, Birth(3), deadline(), read).unwrap();
        assert_eq!(path.nodes(), &nodes);
        path.check_current_with(deadline(), read).unwrap();
        assert_eq!(
            path.check_current_with(Instant::now(), read)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        for invalid in [0, 1, u32::MAX] {
            assert_eq!(
                Ancestry::capture_with(invalid, Birth(3), deadline(), |_| panic!(
                    "invalid pid reached the reader"
                ))
                .unwrap_err()
                .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        for missing in [10, 20, 30] {
            assert!(
                Ancestry::capture_with(30, Birth(3), deadline(), |pid| {
                    (pid != missing).then(|| read(pid)).flatten()
                })
                .is_err()
            );
            assert!(
                path.check_current_with(deadline(), |pid| {
                    (pid != missing).then(|| read(pid)).flatten()
                })
                .is_err()
            );
        }
        for changed in [10, 20, 30] {
            for field in 0..6 {
                let mut calls = 0;
                assert!(
                    Ancestry::capture_with(30, Birth(3), deadline(), |pid| {
                        calls += 1;
                        let mut n = read(pid)?;
                        if calls > nodes.len() && pid == changed {
                            match field {
                                0 => n.identity.birth.0 += 1,
                                1 => n.identity.idversion = Some(99),
                                2 => n.uniqueid += 1,
                                3 => n.parent += 1,
                                4 => n.pgid += 1,
                                _ => n.tty = Some(99),
                            }
                        }
                        Some(n)
                    })
                    .is_err(),
                    "changed pid {changed}, field {field}"
                );
            }
        }
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

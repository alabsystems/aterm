// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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

/// Needs a native lineage reader ([`super::super::read_lineage`]): Apple, Linux and Android only.
#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
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

/// The seam's readable prefix: `pid`'s own native record first, each next node its parent,
/// ending at pid 1 or at the first parent the account cannot read (a foreign-account
/// `sshd`), and refused when `pid` itself is unreadable or reborn. NEGATIVE CONTROL: a walk
/// that refused an unreadable parent, as [`Ancestry::capture`] does, fails the second case.
#[test]
fn a_readable_prefix_ends_at_the_first_parent_this_account_cannot_read() {
    // The live walk needs a native lineage reader (Apple, Linux, Android); the seam does not.
    #[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
    {
        let pid = std::process::id();
        let identity = super::super::read_identity(pid as i32).unwrap();
        let own = Ancestry::readable_prefix(pid, identity.birth).unwrap();
        assert_eq!(own.nodes().first().unwrap().identity, identity);
        for pair in own.nodes().windows(2) {
            assert_eq!(pair[0].parent, pair[1].identity.pid as u32);
        }
        assert!(Ancestry::readable_prefix(pid, Birth(identity.birth.0 + 1)).is_err());
    }
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
    let whole = Ancestry::readable_prefix_with(30, Birth(3), read).unwrap();
    assert_eq!(whole.nodes(), &nodes);
    let foreign_parent = |pid| (pid != 10).then(|| read(pid)).flatten();
    let prefix = Ancestry::readable_prefix_with(30, Birth(3), foreign_parent).unwrap();
    assert_eq!(prefix.nodes(), &nodes[..2]);
    assert!(Ancestry::capture_with(30, Birth(3), deadline(), foreign_parent).is_err());
    let unreadable_self = |pid| (pid != 30).then(|| read(pid)).flatten();
    assert!(Ancestry::readable_prefix_with(30, Birth(3), unreadable_self).is_err());
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

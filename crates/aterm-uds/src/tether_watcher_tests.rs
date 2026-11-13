// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Proof, Watched, end_once_run, end_watched};
use crate::process::{Birth, EndControl, EndRefusal, Ended, Identity, Lineage};
use std::collections::VecDeque;
use std::time::Duration;

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const PID: i32 = 4242;
const BIRTH: Birth = Birth(1_792_000_000_000_042);
const UNIQUE: u64 = 51_832_760;

/// The child as the tether read itself before it forked its watcher: on Apple its pid
/// version is the tether's own, which the exec of the child moves.
fn armed() -> Watched {
    Watched::of(&Lineage {
        identity: Identity {
            pid: PID,
            birth: BIRTH,
            idversion: Some(7),
        },
        uniqueid: UNIQUE,
        parent: 1,
        pgid: 4200,
        launcher_version: Some(3),
        tty: None,
    })
}

/// A read of the pid: its birth, and its pid version where the kernel keeps one.
const fn read(birth: Birth, idversion: Option<i32>) -> Option<Identity> {
    Some(Identity {
        pid: PID,
        birth,
        idversion,
    })
}

/// An `EndControl` whose identity reads follow a script (`None` past it) and that logs each
/// signal; its naps take no time.
struct Script {
    reads: VecDeque<Option<Identity>>,
    signals: Vec<(i32, i32)>,
}

impl EndControl for Script {
    fn identity(&mut self, _pid: i32) -> Option<Identity> {
        self.reads.pop_front().flatten()
    }
    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        self.signals.push((token.pid, sig));
        Ok(())
    }
    fn nap(&mut self, _pause: Duration) {}
}

/// One case: its name, the identity reads, the proof reads (`None` past the script), the
/// answer and the signals sent.
type Case = (
    &'static str,
    Vec<Option<Identity>>,
    Vec<Option<Proof>>,
    Result<Ended, EndRefusal>,
    Vec<i32>,
);

const fn ended(killed: bool, gone: bool) -> Result<Ended, EndRefusal> {
    Ok(Ended {
        termed: true,
        killed,
        gone,
    })
}

#[test]
fn the_watcher_ends_by_proof() {
    let watched = armed();
    assert_eq!(
        watched.token(),
        Identity {
            pid: PID,
            birth: BIRTH,
            idversion: None
        },
        "the token is the pid and birth with no pid version"
    );
    let same = read(BIRTH, None);
    let exec_d = read(BIRTH, Some(8));
    let reborn = read(Birth(BIRTH.0 + 1), None);
    let proven = Some(Proof {
        uniqueid: UNIQUE,
        ran: true,
    });
    let moved = Some(Proof {
        uniqueid: UNIQUE + 1,
        ran: true,
    });
    let held = Some(Proof {
        uniqueid: UNIQUE,
        ran: false,
    });
    let cases: [Case; 8] = [
        (
            "gone after the SIGTERM",
            vec![same, same, None],
            vec![proven],
            ended(false, true),
            vec![SIGTERM],
        ),
        (
            "still there past the grace",
            vec![same, same, same, same, same, None],
            vec![proven, proven],
            ended(true, true),
            vec![SIGTERM, SIGKILL],
        ),
        (
            "exec'd in place since the arming, its pid version moved",
            vec![exec_d, exec_d, None],
            vec![proven],
            ended(false, true),
            vec![SIGTERM],
        ),
        (
            "its unique id moved before the SIGTERM (the control)",
            vec![same, same],
            vec![moved],
            Err(EndRefusal::NotProven),
            vec![],
        ),
        (
            "its pid reused, another birth",
            vec![reborn, reborn],
            vec![proven],
            Err(EndRefusal::Stale),
            vec![],
        ),
        (
            "held at macOS's program check, its thread not yet run",
            vec![same, same],
            vec![held],
            Err(EndRefusal::NotProven),
            vec![],
        ),
        (
            "its proof does not read",
            vec![same, same],
            vec![None],
            Err(EndRefusal::NotProven),
            vec![],
        ),
        (
            "its unique id moved before the SIGKILL",
            vec![same, same, same, same],
            vec![proven, moved],
            ended(false, false),
            vec![SIGTERM],
        ),
    ];
    for (name, reads, proofs, want, signals) in cases {
        let mut control = Script {
            reads: reads.into(),
            signals: Vec::new(),
        };
        let mut proofs = proofs.into_iter();
        let out = end_watched(watched, Duration::ZERO, &mut control, |pid| {
            assert_eq!(pid, PID, "{name}: the proof read is the child's");
            proofs.next().flatten()
        });
        let sent: Vec<i32> = control.signals.iter().map(|&(_, sig)| sig).collect();
        if signals.is_empty() {
            assert!(sent.is_empty(), "{name}: no signal, sent {sent:?}");
        }
        assert_eq!(out, want, "{name}: sent {sent:?}");
        assert_eq!(sent, signals, "{name}");
        assert!(
            control.signals.iter().all(|&(pid, _)| pid == PID),
            "{name}: every signal went to the child's pid alone: {:?}",
            control.signals
        );
    }
}

/// Once its parent is gone, a child held before its first instruction gets no signal and is
/// waited on until its thread has run, then ended by proof; one that exits while held gets
/// none at all, and one whose pid reads another birth is not waited on.
#[test]
fn a_held_child_is_ended_once_its_thread_has_run() {
    let same = read(BIRTH, None);
    let reborn = read(Birth(BIRTH.0 + 1), None);
    let proven = Some(Proof {
        uniqueid: UNIQUE,
        ran: true,
    });
    let held = Some(Proof {
        uniqueid: UNIQUE,
        ran: false,
    });
    /// One run of `end_once_run` against a scripted child.
    struct Case {
        name: &'static str,
        /// What each identity read of the child returns, in turn.
        reads: Vec<Option<Identity>>,
        /// What each proof read of the child returns, in turn.
        proofs: Vec<Option<Proof>>,
        /// The wait at which the child is seen to have exited.
        exits_at_wait: usize,
        /// What `end_once_run` returns.
        want: Option<Result<Ended, EndRefusal>>,
        /// The signals the child is sent.
        signals: Vec<i32>,
    }
    let cases = [
        Case {
            name: "held for two reads, then run",
            reads: vec![same, same, same, same, same, None],
            proofs: vec![held, held, proven, proven],
            exits_at_wait: usize::MAX,
            want: Some(ended(false, true)),
            signals: vec![SIGTERM],
        },
        Case {
            name: "held, then exited",
            reads: vec![same],
            proofs: vec![held],
            exits_at_wait: 1,
            want: None,
            signals: vec![],
        },
        Case {
            name: "its pid reused, another birth",
            reads: vec![reborn, reborn],
            proofs: vec![],
            exits_at_wait: usize::MAX,
            want: Some(Err(EndRefusal::Stale)),
            signals: vec![],
        },
    ];
    for Case {
        name,
        reads,
        proofs,
        exits_at_wait,
        want,
        signals,
    } in cases
    {
        let mut control = Script {
            reads: reads.into(),
            signals: Vec::new(),
        };
        let mut proofs = proofs.into_iter();
        let mut waits = 0;
        let out = end_once_run(
            armed(),
            Duration::ZERO,
            &mut control,
            |pid| {
                assert_eq!(pid, PID, "{name}: the proof read is the child's");
                proofs.next().flatten()
            },
            || {
                waits += 1;
                waits >= exits_at_wait
            },
        );
        let sent: Vec<i32> = control.signals.iter().map(|&(_, sig)| sig).collect();
        assert_eq!(out, want, "{name}: sent {sent:?} after {waits} waits");
        assert_eq!(sent, signals, "{name}");
    }
}

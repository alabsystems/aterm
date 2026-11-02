// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `EndByProof` (`aterm_models_misc::end_by_proof_model`): the real end protocol
//! `aterm_uds::process::end_proven` runs over a scripted `EndControl` and tier judgment, every
//! identity read, judgment and signal it makes is logged, and the log is replayed through the
//! model: a read that shows the identity moved fires `Move`, a tier judgment that holds fires
//! `Judge` and one that fails `Lose`, a `SIGTERM` fires `Term`, a `SIGKILL` fires `Kill`, and a run
//! that returns before a `SIGKILL` fires `Stop`. A tether's watcher ends its child the same way
//! (`aterm_uds::tether::end_watched`): its tier is the child's kernel unique id and a thread that
//! ran, read again before each signal, and its runs replay through the same model.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use aterm_models_misc::end_by_proof_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_uds::process::{Birth, EndControl, EndRefusal, Ended, Identity, end_proven};
use aterm_uds::tether::{Proof, Watched, end_watched};

type S = BTreeMap<&'static str, i64>;

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

fn ident(idversion: i32) -> Identity {
    Identity {
        pid: 4242,
        birth: Birth(1_792_000_000_000_007),
        idversion: Some(idversion),
    }
}

/// What `end_proven` did, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    /// An identity read: `true` when it no longer equals the token.
    Read(bool),
    /// A judgment of the tier on the fresh read: whether it still holds.
    Proof(bool),
    /// A signal sent.
    Signal(i32),
}

/// An `EndControl` whose identity reads follow a script, logging each read and signal.
struct Scripted<'a> {
    token: Identity,
    reads: std::iter::Copied<std::slice::Iter<'a, Option<Identity>>>,
    log: &'a RefCell<Vec<Event>>,
}

impl EndControl for Scripted<'_> {
    fn identity(&mut self, _pid: i32) -> Option<Identity> {
        let read = self.reads.next().flatten();
        self.log
            .borrow_mut()
            .push(Event::Read(read != Some(self.token)));
        read
    }
    fn signal(&mut self, _token: &Identity, sig: i32) -> std::io::Result<()> {
        self.log.borrow_mut().push(Event::Signal(sig));
        Ok(())
    }
    fn nap(&mut self, _pause: Duration) {}
}

/// The real `end_proven` of the token `ident(7)` over `reads` (identity reads, in order) and
/// `proofs` (the tier's judgments, in order; past the script it is lost), with a zero grace so
/// a process that never goes reaches the `SIGKILL` judgment at once.
fn run(reads: &[Option<Identity>], proofs: &[bool]) -> (Result<Ended, EndRefusal>, Vec<Event>) {
    let token = ident(7);
    let log = RefCell::new(Vec::new());
    let mut proofs = proofs.iter().copied();
    let mut control = Scripted {
        token,
        reads: reads.iter().copied(),
        log: &log,
    };
    let out = end_proven(token, Duration::ZERO, &mut control, |_| {
        let holds = proofs.next().unwrap_or(false);
        log.borrow_mut().push(Event::Proof(holds));
        holds
    });
    (out, log.into_inner())
}

fn fire(
    m: &Model,
    s: &mut S,
    action: &'static str,
    fired: &mut BTreeSet<&'static str>,
) -> Result<(), String> {
    if !m.action_enabled(action, s) {
        return Err(format!("`{action}` is not enabled in {s:?}"));
    }
    m.fire(action, s);
    fired.insert(action);
    Ok(())
}

/// Replay one run's log on `m`; the error names the first event the model refuses, or the law
/// that breaks.
fn replay(m: &Model, events: &[Event], fired: &mut BTreeSet<&'static str>) -> Result<S, String> {
    let mut s = m.init_state();
    for (i, event) in events.iter().enumerate() {
        let step = match *event {
            Event::Read(true) if s["moved"] == 0 => fire(m, &mut s, "Move", fired),
            Event::Proof(true) => fire(m, &mut s, "Judge", fired),
            Event::Proof(false) if s["lost"] == 0 => fire(m, &mut s, "Lose", fired),
            Event::Read(_) | Event::Proof(_) => Ok(()),
            Event::Signal(SIGTERM) => fire(m, &mut s, "Term", fired),
            Event::Signal(SIGKILL) => fire(m, &mut s, "Kill", fired),
            Event::Signal(sig) => Err(format!("signal {sig} is no step of the model")),
        };
        step.map_err(|e| format!("event {i} ({event:?}): {e}"))?;
        for inv in &m.invariants {
            if !m.check_invariant(inv.name, &s) {
                return Err(format!("event {i} ({event:?}): `{}` breaks", inv.name));
            }
        }
    }
    if s["v"] < 2 {
        fire(m, &mut s, "Stop", fired).map_err(|e| format!("at the return: {e}"))?;
    }
    Ok(s)
}

/// One scripted run: its name, its identity reads, its tier judgments, and what it returns.
type Lane = (
    &'static str,
    Vec<Option<Identity>>,
    Vec<bool>,
    Result<Ended, EndRefusal>,
);

#[test]
fn every_end_proven_run_is_a_run_of_end_by_proof() {
    let m = interp::with_buggy(&end_by_proof_model(), 0);
    let t = ident(7);
    let moved = ident(8);
    let mut fired = BTreeSet::new();
    let lanes: [Lane; 8] = [
        (
            "gone within the grace",
            vec![Some(t), Some(t), None],
            vec![true],
            Ok(Ended {
                termed: true,
                killed: false,
                gone: true,
            }),
        ),
        (
            "outlives the grace, killed",
            vec![Some(t), Some(t), Some(t), Some(t), Some(t), None],
            vec![true, true],
            Ok(Ended {
                termed: true,
                killed: true,
                gone: true,
            }),
        ),
        (
            "moved before the SIGTERM",
            vec![Some(moved)],
            vec![],
            Err(EndRefusal::Stale),
        ),
        (
            "moved before the SIGKILL",
            vec![Some(t), Some(t), Some(t), Some(moved)],
            vec![true],
            Ok(Ended {
                termed: true,
                killed: false,
                gone: false,
            }),
        ),
        (
            "exec'd while the tier was judged",
            vec![Some(t), Some(moved)],
            vec![true],
            Err(EndRefusal::Stale),
        ),
        (
            "exec'd after the SIGTERM",
            vec![Some(t), Some(t), Some(moved)],
            vec![true],
            Ok(Ended {
                termed: true,
                killed: false,
                gone: false,
            }),
        ),
        (
            "tier lost before the SIGTERM",
            vec![Some(t)],
            vec![false],
            Err(EndRefusal::NotProven),
        ),
        (
            "tier lost before the SIGKILL",
            vec![Some(t), Some(t), Some(t), Some(t)],
            vec![true, false],
            Ok(Ended {
                termed: true,
                killed: false,
                gone: false,
            }),
        ),
    ];
    let mut bad = Vec::new();
    for (name, reads, proofs, want) in lanes {
        let (out, events) = run(&reads, &proofs);
        assert_eq!(out, want, "{name}: {events:?}");
        match replay(&m, &events, &mut fired) {
            Ok(s) => assert_eq!((s["v"], s["bad"]), (2, 0), "{name}: {events:?}"),
            Err(e) => bad.push(format!("{name}: {e} in {events:?}")),
        }
    }
    assert!(
        bad.is_empty(),
        "runs the model refuses:\n  {}",
        bad.join("\n  ")
    );
    assert_eq!(
        fired,
        ["Judge", "Kill", "Lose", "Move", "Stop", "Term"]
            .into_iter()
            .collect(),
        "the real runs drive every action of the model"
    );
}

/// The child a watcher armed on, as `aterm_uds::tether::Watched` records it.
const WATCHED: Watched = Watched {
    pid: 4242,
    birth: Birth(1_792_000_000_000_007),
    uniqueid: 51_832_760,
};

/// The real `end_watched` of [`WATCHED`] over `reads` (identity reads, in order, with no pid
/// version, as its token has none) and `proofs` (the child's unique id and whether its thread ran
/// at each judgment, in order; past the script it does not read), with a zero grace. Each proof
/// read is logged as the judgment it makes: whether it is still the recorded unique id, its
/// thread run.
fn run_watched(
    reads: &[Option<Identity>],
    proofs: &[Option<Proof>],
) -> (Result<Ended, EndRefusal>, Vec<Event>) {
    let log = RefCell::new(Vec::new());
    let mut proofs = proofs.iter().copied();
    let mut control = Scripted {
        token: WATCHED.token(),
        reads: reads.iter().copied(),
        log: &log,
    };
    let out = end_watched(WATCHED, Duration::ZERO, &mut control, |pid| {
        assert_eq!(pid, WATCHED.pid, "the proof read is the child's");
        let read = proofs.next().flatten();
        log.borrow_mut()
            .push(Event::Proof(read == Some(proven(WATCHED.uniqueid, true))));
        read
    });
    (out, log.into_inner())
}

/// A proof read of the watched child: its unique id, and whether its thread ran.
const fn proven(uniqueid: u64, ran: bool) -> Proof {
    Proof { uniqueid, ran }
}

/// One scripted watcher end: its name, its identity reads, its proof reads, and what it returns.
type WatchedLane = (
    &'static str,
    Vec<Option<Identity>>,
    Vec<Option<Proof>>,
    Result<Ended, EndRefusal>,
);

#[test]
fn every_watcher_end_is_a_run_of_end_by_proof() {
    let m = interp::with_buggy(&end_by_proof_model(), 0);
    let t = Some(WATCHED.token());
    let reborn = Some(Identity {
        birth: Birth(WATCHED.birth.0 + 1),
        ..WATCHED.token()
    });
    let same = Some(proven(WATCHED.uniqueid, true));
    let moved = Some(proven(WATCHED.uniqueid + 1, true));
    let held = Some(proven(WATCHED.uniqueid, false));
    let mut fired = BTreeSet::new();
    let lanes: [WatchedLane; 9] = [
        (
            "gone within the grace",
            vec![t, t, None],
            vec![same],
            Ok(Ended {
                termed: true,
                killed: false,
                gone: true,
            }),
        ),
        (
            "outlives the grace, killed",
            vec![t, t, t, t, t, None],
            vec![same, same],
            Ok(Ended {
                termed: true,
                killed: true,
                gone: true,
            }),
        ),
        (
            "its pid reused before the SIGTERM",
            vec![reborn],
            vec![],
            Err(EndRefusal::Stale),
        ),
        (
            "its pid reused while its unique id was read",
            vec![t, reborn],
            vec![same],
            Err(EndRefusal::Stale),
        ),
        (
            "its pid reused before the SIGKILL",
            vec![t, t, t, reborn],
            vec![same],
            Ok(Ended {
                termed: true,
                killed: false,
                gone: true,
            }),
        ),
        (
            "its unique id moved before the SIGTERM",
            vec![t],
            vec![moved],
            Err(EndRefusal::NotProven),
        ),
        (
            "held at macOS's program check, its thread not yet run",
            vec![t],
            vec![held],
            Err(EndRefusal::NotProven),
        ),
        (
            "its proof does not read",
            vec![t],
            vec![None],
            Err(EndRefusal::NotProven),
        ),
        (
            "its unique id moved before the SIGKILL",
            vec![t, t, t, t],
            vec![same, moved],
            Ok(Ended {
                termed: true,
                killed: false,
                gone: false,
            }),
        ),
    ];
    let mut bad = Vec::new();
    for (name, reads, uniqueids, want) in lanes {
        let (out, events) = run_watched(&reads, &uniqueids);
        assert_eq!(out, want, "{name}: {events:?}");
        match replay(&m, &events, &mut fired) {
            Ok(s) => assert_eq!((s["v"], s["bad"]), (2, 0), "{name}: {events:?}"),
            Err(e) => bad.push(format!("{name}: {e} in {events:?}")),
        }
    }
    assert!(
        bad.is_empty(),
        "watcher ends the model refuses:\n  {}",
        bad.join("\n  ")
    );
    assert_eq!(
        fired,
        ["Judge", "Kill", "Lose", "Move", "Stop", "Term"]
            .into_iter()
            .collect(),
        "the watcher's runs drive every action of the model"
    );
}

/// NEGATIVE CONTROLS: what an end that signalled without its judgment would log. Each is
/// rejected at Buggy 0, admitted by the mutant, and the mutant's run breaks the law.
#[test]
fn a_signal_after_a_move_or_a_lost_tier_is_no_run_of_the_model() {
    let healthy = interp::with_buggy(&end_by_proof_model(), 0);
    let buggy = interp::with_buggy(&end_by_proof_model(), 1);
    let traces: [(&str, Vec<Event>); 4] = [
        (
            "a SIGTERM after a moved read",
            vec![Event::Read(true), Event::Signal(SIGTERM)],
        ),
        ("a SIGTERM with no judgment", vec![Event::Signal(SIGTERM)]),
        (
            "a SIGKILL with no judgment since the SIGTERM",
            vec![
                Event::Read(false),
                Event::Proof(true),
                Event::Read(false),
                Event::Signal(SIGTERM),
                Event::Read(false),
                Event::Signal(SIGKILL),
            ],
        ),
        (
            "a SIGKILL after a lost tier",
            vec![
                Event::Read(false),
                Event::Proof(true),
                Event::Read(false),
                Event::Signal(SIGTERM),
                Event::Read(false),
                Event::Read(false),
                Event::Proof(false),
                Event::Signal(SIGKILL),
            ],
        ),
    ];
    for (name, events) in traces {
        let mut fired = BTreeSet::new();
        let refused = replay(&healthy, &events, &mut fired).expect_err(name);
        assert!(refused.contains("is not enabled"), "{name}: {refused}");
        let broken = replay(&buggy, &events, &mut fired).expect_err(name);
        assert!(
            broken.contains("`NoUnprovenSignal` breaks"),
            "{name}: the mutant admits it and breaks the law: {broken}"
        );
    }
}

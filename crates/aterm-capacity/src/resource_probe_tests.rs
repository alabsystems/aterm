// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{Fixed, Proc};
use std::cell::Cell;

struct Counted {
    fixed: Fixed,
    reads: Cell<usize>,
    sweeps: Cell<usize>,
}

impl Counted {
    fn new() -> Self {
        let fixed = Fixed::new(Reading::of_gib(48, 40, 14));
        fixed.set_procs(Some(Procs::of(vec![Proc {
            pid: std::process::id(),
            charged: 1,
            footprint: 1,
            ..Proc::default()
        }])));
        Self {
            fixed,
            reads: Cell::new(0),
            sweeps: Cell::new(0),
        }
    }
}

impl Machine for Counted {
    fn read(&self, path: Option<&std::path::Path>) -> Reading {
        self.reads.set(self.reads.get() + 1);
        self.fixed.read(path)
    }

    fn procs(&self) -> Option<Procs> {
        self.sweeps.set(self.sweeps.get() + 1);
        self.fixed.procs()
    }
}

fn held(ledger: &Ledger, machine: &Counted) -> crate::Claim {
    try_admit_with(ledger, machine, Need::new("existing holder").mem_gib(8))
        .unwrap()
        .unwrap()
}

#[test]
fn blocked_envelopes_share_one_read_only_observation() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let claim = held(&ledger, &machine);
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    machine.reads.set(0);
    machine.sweeps.set(0);
    let bytes = std::fs::read(claim.path()).unwrap();
    let at = Instant::now();
    let probe = ResourceProbe::capture_at(&ledger, &machine, at)
        .unwrap()
        .observed()
        .unwrap();
    for gib in 1..=32 {
        assert_eq!(
            probe.check_at(&Need::new("candidate").mem_gib(gib), at),
            ProbeDecision::Blocked
        );
    }
    assert_eq!((machine.reads.get(), machine.sweeps.get()), (1, 1));
    assert_eq!(std::fs::read(claim.path()).unwrap(), bytes);
    assert_eq!(
        ledger.list().unwrap().len(),
        1,
        "no candidate entered the ledger"
    );
    assert!(
        try_admit_with(&ledger, &machine, Need::new("fresh oracle").mem_gib(1))
            .unwrap()
            .is_none()
    );
}

#[test]
fn possible_observation_cannot_override_a_fresh_process_or_ledger_refusal() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let claim = held(&ledger, &machine);
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    machine.fixed.set_procs(Some(Procs::of(vec![Proc {
        pid: std::process::id(),
        charged: 8 << 30,
        footprint: 8 << 30,
        ..Proc::default()
    }])));
    let need = Need::new("candidate").mem_gib(4);
    let probe = ResourceProbe::capture(&ledger, &machine)
        .unwrap()
        .observed()
        .unwrap();
    assert_eq!(probe.check(&need), ProbeDecision::Possible);
    machine.fixed.set_procs(Some(Procs::of(vec![Proc {
        pid: std::process::id(),
        ..Proc::default()
    }])));
    assert!(
        try_admit_with(&ledger, &machine, need.clone())
            .unwrap()
            .is_none(),
        "fresh tree credit governs admission"
    );
    drop(claim);
    let fresh = try_admit_with(&ledger, &machine, need).unwrap().unwrap();
    assert_eq!(fresh.record().need.mem, 4 << 30);
}

#[test]
fn expired_missing_and_unsupported_observations_defer_to_fresh_admission() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let _claim = held(&ledger, &machine);
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    let need = Need::new("candidate").mem_gib(4);
    let at = Instant::now();
    let probe = ResourceProbe::capture_at(&ledger, &machine, at)
        .unwrap()
        .observed()
        .unwrap();
    assert_eq!(probe.check_at(&need, at), ProbeDecision::Blocked);
    assert_eq!(
        probe.check_at(&Need::timing("timing").mem_gib(4), at),
        ProbeDecision::Possible
    );
    assert_eq!(
        probe.check_at(&need.clone().rank(crate::Rank::Background), at),
        ProbeDecision::Possible
    );
    assert_eq!(
        probe.check_at(&need, at + RESOURCE_PROBE_MAX_AGE),
        ProbeDecision::Possible
    );
    machine.fixed.set_procs(None);
    assert!(matches!(
        ResourceProbe::capture(&ledger, &machine).unwrap(),
        Capture::Unsupported
    ));
    machine.fixed.set_procs(Some(Procs::of(vec![Proc {
        pid: std::process::id(),
        ..Proc::default()
    }])));
    assert!(
        matches!(
            ResourceProbe::capture(&ledger, &machine).unwrap(),
            Capture::Unsupported
        ),
        "unread footprint is not rejection evidence"
    );
    let row = Proc {
        pid: std::process::id(),
        charged: 1,
        footprint: 1,
        ..Proc::default()
    };
    machine.fixed.set_procs(Some(Procs::of(vec![row, row])));
    assert!(
        matches!(
            ResourceProbe::capture(&ledger, &machine).unwrap(),
            Capture::Unsupported
        ),
        "duplicate identity rows are ambiguous"
    );
    let mut unread = Reading::of_gib(48, 8, 14);
    unread.available = None;
    machine.fixed.set(unread);
    assert!(matches!(
        ResourceProbe::capture(&ledger, &machine).unwrap(),
        Capture::Unsupported
    ));
}

/// THE PROBE ASKS AS A WAITING CLAIM OF ITS PROCESS ([`ResourceProbe::capture_at`]'s stand-in),
/// which every look of the ledger reads: its process then holds two claims there, so its held
/// claim's end gives back what it claims and never its whole tree (ATERM-KI-1113-341). On 15 GiB
/// available this process holds 8 GiB beside a tree of 20 GiB, and another process waits ahead
/// for 30 GiB with a 1 GiB margin: that end cannot bring the 16 GiB it lacks, so the boot is
/// stuck, reserves nothing, and a 10 GiB envelope, which fits the room, is possible. NEGATIVE
/// CONTROL: M-PROBE-ALONE (the probe's look read without its stand-in) gives that end the whole
/// 20 GiB tree, keeps the boot in its place sparing 4 GiB and blocks the envelope.
#[test]
fn the_probe_asks_as_its_process_waiting_claim() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let _claim = held(&ledger, &machine);
    let boot = Waiter::enter(
        &ledger,
        &machine,
        Need::new("vm boot").mem_gib(30).margin(1 << 30),
    )
    .unwrap();
    // The boot is another process's: its record, which the waiter keeps locked, names that one.
    let me = std::process::id();
    let other = if me == 2 { 3 } else { 2 };
    let path = std::fs::read_dir(ledger.dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "wait"))
        .expect("the boot waits");
    let text = std::fs::read_to_string(&path).unwrap();
    let (head, rest) = text.split_once('\n').expect("a record has a head line");
    let named = format!("\"pid\":{me}");
    let at = head.find(&named).expect("the head names its pid");
    assert!(
        !head[at + named.len()..].starts_with(|c: char| c.is_ascii_digit()),
        "{head}"
    );
    let head = format!(
        "{}\"pid\":{other}{}",
        &head[..at],
        &head[at + named.len()..]
    );
    std::fs::write(&path, format!("{head}\n{rest}")).unwrap();
    machine.fixed.set(Reading::of_gib(48, 15, 14));
    machine.fixed.set_procs(Some(Procs::of(vec![
        Proc {
            pid: me,
            charged: 20 << 30,
            footprint: 20 << 30,
            ..Proc::default()
        },
        Proc {
            pid: other,
            charged: 1,
            footprint: 1,
            ..Proc::default()
        },
    ])));
    let at = Instant::now();
    let probe = ResourceProbe::capture_at(&ledger, &machine, at)
        .unwrap()
        .observed()
        .unwrap();
    assert_eq!(
        probe.check_at(&Need::new("candidate").mem_gib(10), at),
        ProbeDecision::Possible
    );
    drop(boot);
}

/// THE LEDGER'S LOCK IS AN ANSWER: while another admission holds it, a capture says so and names
/// that holder, after one try of the lock and nothing else: no machine read, no process sweep,
/// no claim. Once the lock is let go the same capture observes.
#[test]
fn a_held_lock_is_the_captures_answer_after_one_try() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let claim = held(&ledger, &machine);
    machine.reads.set(0);
    machine.sweeps.set(0);
    let turn = ledger.hold_turn().unwrap().unwrap();
    let tries = ledger.turn_tries();
    match ResourceProbe::capture(&ledger, &machine).unwrap() {
        Capture::Turn(holder) => assert_eq!(holder, Some(std::process::id())),
        Capture::Observed(_) => panic!("a held lock was read through"),
        Capture::Unsupported => panic!("a held lock was taken for an unsupported observation"),
    }
    assert_eq!(ledger.turn_tries() - tries, 1);
    assert_eq!((machine.reads.get(), machine.sweeps.get()), (0, 0));
    assert_eq!(ledger.list().unwrap().len(), 1, "no claim was laid");
    drop(turn);
    assert!(
        ResourceProbe::capture(&ledger, &machine)
            .unwrap()
            .observed()
            .is_some()
    );
    drop(claim);
}

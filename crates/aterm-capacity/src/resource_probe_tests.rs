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
    let probe = ResourceProbe::capture(&ledger, &machine).unwrap().unwrap();
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
    assert!(ResourceProbe::capture(&ledger, &machine).unwrap().is_none());
    machine.fixed.set_procs(Some(Procs::of(vec![Proc {
        pid: std::process::id(),
        ..Proc::default()
    }])));
    assert!(
        ResourceProbe::capture(&ledger, &machine).unwrap().is_none(),
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
        ResourceProbe::capture(&ledger, &machine).unwrap().is_none(),
        "duplicate identity rows are ambiguous"
    );
    let mut unread = Reading::of_gib(48, 8, 14);
    unread.available = None;
    machine.fixed.set(unread);
    assert!(ResourceProbe::capture(&ledger, &machine).unwrap().is_none());
}

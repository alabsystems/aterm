// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{CpuPoint, Machine, Procs, Reading, TimingIssue};
use std::cell::{Cell, RefCell};

struct Actual {
    tick: Cell<u64>,
    busy: Cell<bool>,
    gpu: Cell<Option<u8>>,
    load: Cell<Option<f64>>,
    memory: Cell<Option<u64>>,
    stale: RefCell<Option<CpuPoint>>,
}
impl Default for Actual {
    fn default() -> Self {
        Self {
            tick: Cell::new(0),
            busy: Cell::new(false),
            gpu: Cell::new(Some(0)),
            load: Cell::new(Some(1.0)),
            memory: Cell::new(Some(100)),
            stale: RefCell::new(None),
        }
    }
}
impl Machine for Actual {
    fn read(&self, _: Option<&Path>) -> Reading {
        let mut reading = Reading::of_gib(48, 40, 14);
        reading.load1 = self.load.get();
        reading.gpu_busy = self.gpu.get();
        reading
    }
    fn cpu_point(&self) -> Option<CpuPoint> {
        if let Some(point) = self.stale.borrow().as_ref() {
            return Some(point.clone());
        }
        let tick = self.tick.get() + 100;
        self.tick.set(tick);
        Some(CpuPoint {
            at: Instant::now(),
            unix_ms: now_ms(),
            ticks: vec![aterm_sysprobe::CoreTicks {
                user: if self.busy.get() { tick } else { 0 },
                idle: if self.busy.get() { 0 } else { tick },
                ..Default::default()
            }],
        })
    }
    fn procs(&self) -> Option<Procs> {
        let footprint = self.memory.get()?;
        Some(Procs::of(vec![crate::Proc {
            pid: std::process::id(),
            ppid: 1,
            start: 0,
            charged: footprint,
            footprint,
            written: 0,
            cpu_ns: 0,
            runnable_ns: 0,
        }]))
    }
}
fn admitted(ledger: &Ledger, machine: &Actual) -> Claim {
    let waiter = Waiter::enter(
        ledger,
        machine,
        Need::timing("comparison test").mem(1024).margin(128),
    )
    .unwrap();
    let waiter = match waiter.poll(machine) {
        Polled::Waiting(waiter, _) => waiter,
        _ => panic!("warming"),
    };
    match waiter.poll(machine) {
        Polled::Admitted(claim) => claim,
        _ => panic!("ordinary timing admission"),
    }
}
fn observe(
    claim: &mut Claim,
    actual: &Actual,
) -> Result<crate::ComparisonObservation, TimingIssue> {
    claim.comparison_observation_on(actual, Pace::default(), &mut |_| false)
}

#[test]
fn real_bad_final_is_rejected_even_when_a_peer_and_previous_arm_are_quiet() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let actual = Actual::default();
    let mut own = admitted(&ledger, &actual);
    let quiet_entry = observe(&mut own, &actual).unwrap();
    // A peer on a different isolated ledger can qualify itself; this is no authority
    // to replace the actual final reading under our original claim.
    let peer_ledger = Ledger::at(scratch.path().join("peer-ledger"));
    let peer_actual = Actual::default();
    let mut peer = admitted(&peer_ledger, &peer_actual);
    assert!(observe(&mut peer, &peer_actual).is_ok());
    actual.load.set(Some(1000.0));
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::Busy));
    assert!(quiet_entry.window.ready());
    // Negative control: the old completion check omits the actual final sample and
    // accepts exactly the same bad-final trace. Removing the sample above breaks this test.
    assert!(own.timing_admission().is_ok());
}

#[test]
fn stale_previous_arm_gpu_unknown_load_unknown_and_reset_refuse() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let actual = Actual::default();
    let mut own = admitted(&Ledger::at(scratch.path().join("ledger")), &actual);
    assert!(observe(&mut own, &actual).is_ok());
    let previous = actual.cpu_point();
    *actual.stale.borrow_mut() = previous;
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::Unavailable));
    actual.stale.borrow_mut().take();
    actual.gpu.set(None);
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::Unavailable));
    actual.gpu.set(Some(100));
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::Busy));
    actual.gpu.set(Some(0));
    actual.load.set(None);
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::LoadUnknown));
    actual.load.set(Some(1.0));
    let result = own.comparison_observation_on(&actual, Pace::default(), &mut |_| {
        actual.tick.set(0);
        false
    });
    assert_eq!(result, Err(TimingIssue::CounterChanged));
}

#[test]
fn original_expiry_custody_and_memory_are_required_across_every_arm() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let actual = Actual::default();
    let mut own = admitted(&Ledger::at(scratch.path().join("ledger")), &actual);
    let admitted = own.record.timing.admitted_ms;
    for _ in 0..3 {
        assert!(observe(&mut own, &actual).is_ok());
    }
    assert_eq!(own.record.timing.admitted_ms, admitted);
    actual.memory.set(Some(1024));
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::Memory));
    actual.memory.set(None);
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::Unavailable));
    actual.memory.set(Some(100));
    own.record.need.mem = 0;
    assert_eq!(
        observe(&mut own, &actual),
        Err(TimingIssue::UnknownAdmission)
    );
    own.record.need.mem = 1024;
    own.record.need.margin = 0;
    assert_eq!(
        observe(&mut own, &actual),
        Err(TimingIssue::UnknownAdmission)
    );
    own.record.need.margin = 128;
    let expired = now_ms().saturating_sub(millis(own.record.need.patience.unwrap().hold));
    own.record.timing.admitted_ms = Some(expired);
    own.record.held.as_mut().unwrap().admitted_ms = expired;
    assert_eq!(observe(&mut own, &actual), Err(TimingIssue::Expired));
    own.record.timing.admitted_ms = admitted;
    own.record.held.as_mut().unwrap().admitted_ms = admitted.unwrap();
    std::fs::remove_file(own.path()).unwrap();
    assert_eq!(
        observe(&mut own, &actual),
        Err(TimingIssue::UnknownAdmission)
    );
}

#[test]
fn ordinary_reservation_blocks_later_work_between_arms_and_releases_between_quads() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(scratch.path().join("ledger"));
    let actual = Actual::default();
    let mut own = admitted(&ledger, &actual);
    let original = own.record.timing.admitted_ms.unwrap();
    let hold = millis(own.record.need.patience.unwrap().hold);
    let mut later = own.record.clone();
    later.pid = u32::MAX;
    later.start_ms = own.record.start_ms + 1;
    later.held = None;
    later.need = Need::new("unrelated later build").mem(100);
    let mut earlier = later.clone();
    earlier.start_ms = own.record.start_ms - 1;
    let unrelated = |_: u32| false;
    // Before admission an older waiter retains priority. Once this ordinary claim
    // has actually admitted, its original held reservation protects every arm.
    let mut waiting = own.record.clone();
    waiting.held = None;
    waiting.next_ms = Some(original - millis(crate::QUIET_PATIENCE.reserve));
    assert!(reserving(&earlier, &[&waiting], &unrelated, original).is_none());
    for _ in 0..4 {
        observe(&mut own, &actual).unwrap();
        let records = ledger.list().unwrap();
        let refs: Vec<_> = records.iter().collect();
        let (held, left) = reserving(&later, &refs, &unrelated, original + 1).unwrap();
        assert_eq!(held.timing.admitted_ms, Some(original));
        assert_eq!(left, Duration::from_millis(hold - 1));
        assert!(
            reserving(&earlier, &refs, &unrelated, original + 1).is_some(),
            "an actually held ordinary reservation protects its work until original expiry"
        );
        assert!(
            reserving(&later, &refs, &unrelated, original + hold).is_none(),
            "original hold expires without renewal"
        );
    }
    drop(own);
    let records = ledger.list().unwrap();
    assert!(records.is_empty());
    assert!(
        reserving(
            &later,
            &records.iter().collect::<Vec<_>>(),
            &unrelated,
            original + 1
        )
        .is_none(),
        "queued work can proceed before a new quad requeues"
    );
}

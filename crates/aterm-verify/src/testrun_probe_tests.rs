// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_capacity::{Fixed, Ledger, Machine, Need, Proc, Procs, Reading};

struct Counted {
    fixed: Fixed,
    reads: AtomicUsize,
    sweeps: AtomicUsize,
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
            reads: AtomicUsize::new(0),
            sweeps: AtomicUsize::new(0),
        }
    }
}

impl Machine for Counted {
    fn read(&self, path: Option<&Path>) -> Reading {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.fixed.read(path)
    }

    fn procs(&self) -> Option<Procs> {
        self.sweeps.fetch_add(1, Ordering::SeqCst);
        self.fixed.procs()
    }
}

fn held(ledger: &Ledger, machine: &Counted) -> aterm_capacity::Claim {
    aterm_capacity::admit::try_admit_with(ledger, machine, Need::new("holder").mem_gib(8))
        .unwrap()
        .unwrap()
}

#[test]
fn cached_and_fresh_refusals_keep_the_same_wait_reason() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let _holder = held(&ledger, &machine);
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    machine.reads.store(0, Ordering::SeqCst);
    machine.sweeps.store(0, Ordering::SeqCst);
    let pass = Mutex::new(AdmissionPass::default());
    let at = Instant::now();
    let refusal = |cached: bool, gib| {
        let mut waiting = None;
        assert!(
            admit_noting(
                cached.then_some((&pass, 0)),
                &ledger,
                &machine,
                Need::new("candidate").mem_gib(gib),
                &|| at,
                &mut waiting,
            )
            .unwrap()
            .is_none()
        );
        waiting.expect("each refused attempt must retain its reason")
    };
    let memory = refusal(true, 4);
    let cached_memory = refusal(true, 4);
    let unreachable = refusal(true, 48);
    assert_eq!(memory.classification(), ("ledger", &["memory"][..]));
    assert_eq!(cached_memory.classification(), memory.classification());
    assert_eq!(
        unreachable.classification(),
        ("ledger", &["unreachable"][..])
    );
    assert_eq!(
        (
            machine.reads.load(Ordering::SeqCst),
            machine.sweeps.load(Ordering::SeqCst),
        ),
        (1, 1),
        "retaining cached wait reasons does not take another observation"
    );
    assert_eq!(refusal(false, 4).classification(), memory.classification());
    assert_eq!(
        refusal(false, 48).classification(),
        unreachable.classification()
    );
    assert!(machine.reads.load(Ordering::SeqCst) > 1);
    assert_eq!(ledger.list().unwrap().len(), 1);

    machine.fixed.set(Reading::of_gib(48, 40, 14));
    let mut waiting = Some(memory);
    let admitted = admit_noting(
        Some((&pass, 1)),
        &ledger,
        &machine,
        Need::new("now available").mem_gib(1),
        &|| at,
        &mut waiting,
    )
    .unwrap();
    assert!(admitted.is_some());
    assert!(
        waiting.is_none(),
        "a later grant cannot keep an old refusal"
    );
    assert!(pass.lock().unwrap().probe.is_none());
}

#[test]
fn blocked_workers_share_one_observation_until_dispatch_changes() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let claim = held(&ledger, &machine);
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    machine.reads.store(0, Ordering::SeqCst);
    machine.sweeps.store(0, Ordering::SeqCst);
    let mut plan = test_plan::plan(&vec![None; 32], 4, None, 4);
    for binary in &mut plan.order {
        binary.peak = 4 << 30;
    }
    let state = Mutex::new(Dispatcher::new(&plan, None));
    let pass = Mutex::new(AdmissionPass::default());
    let at = Instant::now();
    let candidates = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let mut dispatcher = state.lock().unwrap();
                let generation = dispatcher.generation();
                assert!(
                    dispatcher
                        .next_with(0.0, &mut |b| {
                            candidates.fetch_add(1, Ordering::SeqCst);
                            match probe_admit_observed_with_clock(
                                Some((&pass, generation)),
                                &ledger,
                                &machine,
                                Need::new("candidate").mem(b.peak),
                                &|| at,
                            )
                            .unwrap()
                            {
                                aterm_capacity::admit::AdmissionAttempt::Waiting(short) => {
                                    assert!(short.shorts.iter().any(|reason| matches!(
                                        reason,
                                        aterm_capacity::admit::Short::Memory { .. }
                                    )));
                                    assert!(
                                        short
                                            .holders
                                            .iter()
                                            .any(|holder| holder.pid == std::process::id())
                                    );
                                    false
                                }
                                other => {
                                    panic!("blocked candidate lost its memory reason: {other:?}")
                                }
                            }
                        })
                        .is_none()
                );
            });
        }
    });
    assert_eq!(candidates.load(Ordering::SeqCst), 64);
    match probe_admit_observed_with_clock(
        Some((&pass, state.lock().unwrap().generation())),
        &ledger,
        &machine,
        Need::new("impossible candidate").mem_gib(48),
        &|| at,
    )
    .unwrap()
    {
        aterm_capacity::admit::AdmissionAttempt::Unreachable(why) => assert!(!why.is_empty()),
        other => panic!("unreachable candidate lost its reason: {other:?}"),
    }
    assert_eq!(
        (
            machine.reads.load(Ordering::SeqCst),
            machine.sweeps.load(Ordering::SeqCst)
        ),
        (1, 1),
        "blocked candidates do not enter the fresh admission path"
    );
    assert_eq!(ledger.list().unwrap().len(), 1);
    machine.fixed.set(Reading::of_gib(48, 40, 14));
    let mut dispatcher = state.lock().unwrap();
    // A completed peer changes the generation independently of the resource observation.
    let peer = dispatcher.next(0.0).unwrap();
    dispatcher.finish(peer.index).unwrap();
    let generation = dispatcher.generation();
    let mut admitted = None;
    assert!(
        dispatcher
            .next_with(0.0, &mut |b| {
                admitted = probe_admit_with_clock(
                    Some((&pass, generation)),
                    &ledger,
                    &machine,
                    Need::new("candidate").mem(b.peak),
                    &|| at,
                )
                .unwrap();
                admitted.is_some()
            })
            .is_some(),
        "a dispatch transition does not reuse the former rejection"
    );
    assert!(
        machine.reads.load(Ordering::SeqCst) > 2,
        "a Possible result still reads fresh admission data"
    );
    assert!(
        pass.lock().unwrap().probe.is_none(),
        "acquisition clears the observation"
    );
    drop((admitted, claim));
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn slow_observation_expires_before_rejection_and_uses_fresh_admission() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let _claim = held(&ledger, &machine);
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    machine.reads.store(0, Ordering::SeqCst);
    machine.sweeps.store(0, Ordering::SeqCst);
    let pass = Mutex::new(AdmissionPass::default());
    let at = Instant::now();
    let ticks = std::cell::Cell::new(0);
    let clock = || {
        let tick = ticks.get();
        ticks.set(tick + 1);
        at + if tick == 0 {
            Duration::ZERO
        } else {
            aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE
        }
    };
    assert!(
        probe_admit_with_clock(
            Some((&pass, 0)),
            &ledger,
            &machine,
            Need::new("candidate").mem_gib(4),
            &clock
        )
        .unwrap()
        .is_none()
    );
    assert!(ticks.get() >= 2, "rejection samples time after observation");
    assert!(
        machine.reads.load(Ordering::SeqCst) > 1,
        "an observation that expired while being collected must use fresh admission"
    );
    assert_eq!(
        ledger.list().unwrap().len(),
        1,
        "fresh refusal keeps no candidate claim"
    );
}

#[test]
fn stale_possible_and_expired_rejections_never_grant() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let _claim = held(&ledger, &machine);
    let need = Need::new("candidate").mem_gib(4);
    let pass = Mutex::new(AdmissionPass::default());
    let at = Instant::now();
    assert!(
        pass.lock()
            .unwrap()
            .allows(1, &ledger, &machine, &need, &|| at)
    );
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    assert!(
        probe_admit_with_clock(Some((&pass, 1)), &ledger, &machine, need.clone(), &|| at)
            .unwrap()
            .is_none(),
        "a formerly Possible observation must pass fresh admission"
    );
    assert_eq!(ledger.list().unwrap().len(), 1);
    assert!(
        !pass
            .lock()
            .unwrap()
            .allows(2, &ledger, &machine, &need, &|| at)
    );
    machine.fixed.set(Reading::of_gib(48, 40, 14));
    {
        let mut pass = pass.lock().unwrap();
        assert!(
            pass.allows(2, &ledger, &machine, &need, &|| at
                + aterm_capacity::admit::RESOURCE_PROBE_MAX_AGE),
            "expired rejection is sampled anew"
        );
    }
}

#[cfg(unix)]
#[test]
fn acquired_claim_invalidates_before_a_partial_jobserver_attempt() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let mut ctx = crate::Ctx::new(
        dir.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    let pool = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 2).unwrap();
    let held_token = pool.try_acquire().unwrap().unwrap();
    let plan = test_plan::plan(&[None], 2, None, 2);
    let mut binary = plan.order[0];
    binary.charge = 2;
    binary.peak = 4 << 30;
    let pass = Mutex::new(AdmissionPass::default());
    let result = binary_permit_with(Some(&ctx), Some(&pool), &binary, &mut |need, mode| {
        assert!(mode.is_none());
        let result = probe_admit(Some((&pass, 1)), &ledger, &machine, need);
        assert!(
            matches!(&result, Ok(Some(_))),
            "memory is acquired before FIFO tokens"
        );
        assert!(
            pass.lock().unwrap().probe.is_none(),
            "even a subsequent token failure cannot retain a probe"
        );
        result
    });
    assert!(result.is_none());
    assert!(
        ledger.list().unwrap().is_empty(),
        "the failed attempt returned its claim"
    );
    let token = pool.try_acquire().unwrap().unwrap();
    assert!(pool.try_acquire().unwrap().is_none());
    drop((token, held_token));
    assert!(pool.try_acquire().unwrap().is_some());
}

#[test]
fn cancellation_stops_before_a_warmed_possible_probe_can_start_work() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    let need = Need::new("candidate").mem_gib(1);
    let pass = Mutex::new(AdmissionPass::default());
    assert!(
        pass.lock()
            .unwrap()
            .allows(0, &ledger, &machine, &need, &Instant::now)
    );
    let plan = test_plan::plan(&[None], 1, None, 1);
    let state = Mutex::new(Dispatcher::new(&plan, None));
    let turn = Condvar::new();
    let mut attempts = 0;
    let stopped = admitted_holding_pass(
        &state,
        &turn,
        &|| 0.0,
        &|| true,
        &|| {},
        &mut |generation, _| {
            attempts += 1;
            probe_admit(Some((&pass, generation)), &ledger, &machine, need.clone()).unwrap()
        },
    );
    assert!(stopped.is_none());
    assert_eq!(attempts, 0);
    assert!(ledger.list().unwrap().is_empty());
    let live = Mutex::new(Dispatcher::new(&plan, None));
    let admitted = admitted_holding_pass(
        &live,
        &turn,
        &|| 0.0,
        &|| false,
        &|| {},
        &mut |generation, _| {
            probe_admit(Some((&pass, generation)), &ledger, &machine, need.clone()).unwrap()
        },
    );
    assert!(
        admitted.is_some(),
        "the same envelope starts without cancellation"
    );
    drop(admitted);
    assert!(ledger.list().unwrap().is_empty());
}

#[cfg(test)]
fn admit_noting(
    pass: Option<(&Mutex<AdmissionPass>, u64)>,
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
    clock: &dyn Fn() -> Instant,
    waiting: &mut Option<test_waits::Reason>,
) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
    use aterm_capacity::admit::AdmissionAttempt;
    *waiting = None;
    match probe_admit_observed_with_clock(pass, ledger, machine, need, clock)? {
        AdmissionAttempt::Admitted(claim) => Ok(Some(claim)),
        AdmissionAttempt::Waiting(short) => {
            *waiting = Some(test_waits::Reason::ledger(&short));
            Ok(None)
        }
        AdmissionAttempt::Unreachable(why) => {
            *waiting = Some(test_waits::Reason::unreachable(why));
            Ok(None)
        }
    }
}

#[cfg(test)]
fn probe_admit(
    pass: Option<(&Mutex<AdmissionPass>, u64)>,
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
    probe_admit_with_clock(pass, ledger, machine, need, &Instant::now)
}

#[cfg(test)]
fn probe_admit_with_clock(
    pass: Option<(&Mutex<AdmissionPass>, u64)>,
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
    clock: &dyn Fn() -> Instant,
) -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
    use aterm_capacity::admit::AdmissionAttempt;
    probe_admit_observed_with_clock(pass, ledger, machine, need, clock).map(|attempt| match attempt
    {
        AdmissionAttempt::Admitted(claim) => Some(claim),
        AdmissionAttempt::Waiting(_) | AdmissionAttempt::Unreachable(_) => None,
    })
}

#[cfg(test)]
fn probe_admit_observed_with_clock(
    pass: Option<(&Mutex<AdmissionPass>, u64)>,
    ledger: &aterm_capacity::Ledger,
    machine: &dyn aterm_capacity::Machine,
    need: aterm_capacity::Need,
    clock: &dyn Fn() -> Instant,
) -> Result<aterm_capacity::admit::AdmissionAttempt, aterm_capacity::Error> {
    use aterm_capacity::admit::{AdmissionAttempt, ProbeRejection};
    if let Some((pass, generation)) = pass
        && let Some(rejected) = pass
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rejection(generation, ledger, machine, &need, clock)
    {
        return Ok(match rejected {
            ProbeRejection::Waiting(short) => AdmissionAttempt::Waiting(short),
            ProbeRejection::Unreachable(why) => AdmissionAttempt::Unreachable(why),
        });
    }
    // A cached rejection supplies the same resource evidence without another Live read.
    // Every possible start takes fresh admission; invalidate before partial-core/FIFO work.
    let result = aterm_capacity::admit::try_admit_observed_with(ledger, machine, need);
    if matches!(&result, Ok(AdmissionAttempt::Admitted(_)))
        && let Some((pass, _)) = pass
    {
        pass.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .acquired();
    }
    result
}

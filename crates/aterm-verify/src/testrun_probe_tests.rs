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
    let mut plan = test_plan::plan(&vec![None; 32], &[], 4, None, 4);
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
    let plan = test_plan::plan(&[None], &[], 2, None, 2);
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
    let plan = test_plan::plan(&[None], &[], 1, None, 1);
    let state = Mutex::new(Dispatcher::new(&plan, None));
    let turn = Condvar::new();
    let mut attempts = 0;
    let stopped =
        admitted_holding_pass(&state, &turn, &|| 0.0, &|| true, &|| {}, &mut |look, _| {
            attempts += 1;
            probe_admit(
                Some((&pass, look.generation)),
                &ledger,
                &machine,
                need.clone(),
            )
            .unwrap()
        });
    assert!(stopped.is_none());
    assert_eq!(attempts, 0);
    assert!(ledger.list().unwrap().is_empty());
    let live = Mutex::new(Dispatcher::new(&plan, None));
    let admitted =
        admitted_holding_pass(&live, &turn, &|| 0.0, &|| false, &|| {}, &mut |look, _| {
            probe_admit(
                Some((&pass, look.generation)),
                &ledger,
                &machine,
                need.clone(),
            )
            .unwrap()
        });
    assert!(
        admitted.is_some(),
        "the same envelope starts without cancellation"
    );
    drop(admitted);
    assert!(ledger.list().unwrap().is_empty());
}

/// Whether looks that began at `starts`, in order, keep THE PACE: at most `1 + floor(T / again)`
/// of them in any span `T`.
fn paced(starts: &[Instant], again: Duration) -> bool {
    starts.iter().enumerate().all(|(i, first)| {
        starts[i..].iter().enumerate().all(|(after, last)| {
            u128::try_from(after).unwrap()
                <= last.saturating_duration_since(*first).as_nanos() / again.as_nanos()
        })
    })
}

/// A deterministic stream of numbers below `bound` (a linear congruential generator).
struct Draws(u64);

impl Draws {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

/// THE LAW. A dispatch looks at its queue once per interval, not once per worker: for a fixed
/// generation, any number of workers and any sequence of instants, at most
/// `1 + floor(T / LOOK_AGAIN)` looks begin in a span `T`, and a look is due at once after each
/// generation change (a start, a finish). Driven twice: on the dispatcher's own pacing over
/// random instants, and through the real loop ([`admitted_holding_observed`]) with eight workers
/// and a queue none of whose binaries can start, each look one pass over the queue.
/// NEGATIVE CONTROL: workers that each keep their own interval, as each did before, let a second
/// worker look inside the first's interval, and the pace is broken.
#[test]
fn a_dispatch_that_admits_nothing_looks_once_per_interval() {
    let base = Instant::now();
    let plan = test_plan::plan(&vec![None; 64], &[], 4, None, 4);
    for seed in 0..64 {
        let mut draws = Draws(seed);
        let mut d = Dispatcher::new(&plan, None);
        let mut now = base;
        let mut looks: Vec<(u64, Instant)> = Vec::new();
        for _ in 0..400 {
            now += Duration::from_millis(draws.below(120));
            match draws.below(40) {
                0 => {
                    // A start: a look is due at once.
                    if let Some(b) = d.next(0.0) {
                        assert_eq!(d.look_due(now), None, "a start makes a look due");
                        d.finish(b.index).unwrap();
                        assert_eq!(d.look_due(now), None, "a finish makes a look due");
                    }
                }
                _ if d.look_due(now).is_none() => {
                    let look = d.begin_look();
                    looks.push((look.generation, now));
                    // The pass takes its time under the mutex; no other worker asks meanwhile.
                    now += Duration::from_millis(draws.below(80));
                    d.looked(look, now, LOOK_AGAIN);
                }
                _ => {}
            }
        }
        let mut generations: Vec<u64> = looks.iter().map(|(g, _)| *g).collect();
        generations.dedup();
        assert!(generations.len() > 1, "the history changed generation");
        for generation in generations {
            let starts: Vec<Instant> = looks
                .iter()
                .filter(|(g, _)| *g == generation)
                .map(|(_, at)| *at)
                .collect();
            assert!(paced(&starts, LOOK_AGAIN), "seed {seed}: {starts:?}");
        }
    }
    // The control: two workers, each paced by its own last look, asking 100 ms apart.
    let mut own: [Option<Instant>; 2] = [None, None];
    let mut starts = Vec::new();
    for step in 0..20_u32 {
        let at = base + Duration::from_millis(100) * step;
        let worker = usize::try_from(step % 2).unwrap();
        if own[worker].is_none_or(|last| at >= last + LOOK_AGAIN) {
            starts.push(at);
            own[worker] = Some(at);
        }
    }
    assert!(
        !paced(&starts, LOOK_AGAIN),
        "a second worker that looks inside the interval breaks the pace"
    );

    // Through the real loop: eight workers, four binaries, nothing starts.
    let plan = test_plan::plan(&vec![None; 4], &[], 4, None, 4);
    let state = Mutex::new(Dispatcher::new(&plan, None));
    let turn = Condvar::new();
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let visits: Mutex<Vec<(u64, Instant)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let started = admitted_holding_observed(
                    &state,
                    &turn,
                    &|| 0.0,
                    &|| stopped.load(Ordering::SeqCst),
                    &|| {},
                    &mut |look, _, _| {
                        visits.lock().unwrap().push((look.seq, Instant::now()));
                        None::<()>
                    },
                    &mut |_, _| {},
                    &|_| LOOK_AGAIN,
                );
                assert!(started.is_none());
            });
        }
        // A hang detector, not a budget: three looks take a second and a half.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let mut seqs: Vec<u64> = visits.lock().unwrap().iter().map(|(s, _)| *s).collect();
            seqs.dedup();
            if seqs.len() >= 3 {
                break;
            }
            assert!(Instant::now() < deadline, "the dispatch stopped looking");
            std::thread::park_timeout(Duration::from_millis(20));
        }
        stopped.store(true, Ordering::SeqCst);
        turn.notify_all();
    });
    let visits = visits.into_inner().unwrap();
    let mut seqs: Vec<u64> = visits.iter().map(|(seq, _)| *seq).collect();
    seqs.dedup();
    assert!(seqs.len() >= 3);
    for seq in &seqs {
        assert_eq!(
            visits.iter().filter(|(s, _)| s == seq).count(),
            4,
            "a look is one pass over the queue"
        );
    }
    let starts: Vec<Instant> = seqs
        .iter()
        .map(|seq| visits.iter().find(|(s, _)| s == seq).unwrap().1)
        .collect();
    assert!(paced(&starts, LOOK_AGAIN), "{starts:?}");
}

/// What one look of a dispatch over `state`'s queue touched and found ([`look_once`]).
struct OneLook {
    started: Option<usize>,
    waits: Vec<Option<test_waits::Wait>>,
    entries: usize,
    turn_tries: u64,
    sweeps: usize,
}

/// One look of a dispatch over `state`'s queue, each binary asked through the real permit path
/// ([`binary_permit_in`]) in `ledger` on `machine`, sharing `pass` where there is one (`None`:
/// every binary but the retained one entered afresh, as before a busy lock was an answer).
fn look_once(
    state: &Mutex<Dispatcher>,
    pass: Option<&Mutex<AdmissionPass>>,
    waiters: &BinaryWaiters,
    ctx: &crate::Ctx,
    ledger: &Ledger,
    machine: &Counted,
    claims: &mut Vec<aterm_capacity::Claim>,
) -> (OneLook, test_plan::Look) {
    let entries = AtomicUsize::new(0);
    let enter = |need, place: Option<&aterm_capacity::admit::Place>| {
        entries.fetch_add(1, Ordering::SeqCst);
        enter_in(Some(ledger), machine, need, place)
    };
    let asking = Asking {
        ledger: Some(ledger),
        machine,
        enter: &enter,
    };
    let tries = ledger.turn_tries();
    machine.sweeps.store(0, Ordering::SeqCst);
    let mut d = state.lock().unwrap();
    let look = d.begin_look();
    let mut waits = Vec::new();
    let started = d.next_with(0.0, &mut |b| {
        let (permit, wait) = binary_permit_in(
            Some(ctx),
            None,
            b,
            waiters,
            &|| false,
            pass.map(|pass| (pass, look)),
            &asking,
        );
        waits.push(wait);
        match permit {
            Some(Ok(permit)) => {
                claims.extend(permit.claim.and_then(|claim| Arc::try_unwrap(claim).ok()));
                true
            }
            Some(Err(why)) => panic!("a look failed: {why}"),
            None => false,
        }
    });
    let again = pass.map_or(LOOK_AGAIN, |pass| pass.lock().unwrap().again(look));
    d.looked(look, Instant::now(), again);
    (
        OneLook {
            started: started.map(|b| b.index),
            waits,
            entries: entries.load(Ordering::SeqCst),
            turn_tries: ledger.turn_tries() - tries,
            sweeps: machine.sweeps.load(Ordering::SeqCst),
        },
        look,
    )
}

/// THE LAW. While another admission holds the ledger's lock ([`Ledger::hold_turn`]), one look
/// over 32 binaries with one retained request lays no claim, sweeps no process table and tries
/// the lock at most twice: once where the retained request is asked first and its poll meets
/// the lock, twice where a binary the shared observation answers comes before it (that
/// observation's try, then the poll). The lock is that look's answer for every other binary,
/// which decides nothing of why it waits, and the next look is due after [`LOCK_AGAIN`]. Deleting
/// either way a look learns it met the lock fails one of the two. The retained request keeps its
/// record, its place and its wait mark unbroken across that look and a decided one, and the
/// first look after the lock is let go admits a binary that fits: that request, at its place.
/// NEGATIVE CONTROL: with no shared look state each other binary enters afresh under the busy
/// lock, laying a claim and trying the lock, and the bound fails.
#[test]
fn a_busy_turn_costs_a_pass_no_claim() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let machine = Counted::new();
    // A holder of 8 GiB whose name only this test's marks can say, then 8 GiB available: a
    // binary of 4 GiB waits behind it.
    let holder_name = format!("LS-74 holder in {}", dir.path().display());
    let holder = aterm_capacity::admit::try_admit_with(
        &ledger,
        &machine,
        Need::new(&holder_name).mem_gib(8),
    )
    .unwrap()
    .unwrap();
    machine.fixed.set(Reading::of_gib(48, 8, 14));
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
    let mut plan = test_plan::plan(&vec![None; 32], &[], 4, None, 4);
    for binary in &mut plan.order {
        binary.peak = 4 << 30;
        binary.charge = 1;
    }
    let state = Mutex::new(Dispatcher::new(&plan, None));
    let pass = Mutex::new(AdmissionPass::default());
    let waiters = BinaryWaiters::default();
    let mut claims = Vec::new();
    let me = std::process::id();
    let retained = || {
        waiters
            .state
            .lock()
            .unwrap()
            .pending
            .as_ref()
            .map(|(index, waiter)| (*index, waiter.record().order()))
            .expect("a retained request")
    };
    // The retained request's wait mark: the one mark of this process that names the holder.
    let mark = || {
        let marks: Vec<_> = aterm_toolchain::waits::waiting(
            &aterm_toolchain::waits::user_dir().expect("a waits directory"),
        )
        .into_iter()
        .filter(|w| w.pid == me && w.text.contains(&holder_name))
        .collect();
        assert_eq!(marks.len(), 1, "{marks:?}");
        marks.into_iter().next().unwrap()
    };

    // The dispatch's first look, with no shared state: the first binary's request enters and is
    // retained, each other's enters and leaves.
    let (first, _) = look_once(&state, None, &waiters, &ctx, &ledger, &machine, &mut claims);
    assert_eq!(first.started, None);
    let (index, order) = retained();
    let records = ledger.list().unwrap();
    let marked = mark();

    // THE BUSY LOOK.
    let turn = ledger.hold_turn().unwrap().unwrap();
    let (busy, look) = look_once(
        &state,
        Some(&pass),
        &waiters,
        &ctx,
        &ledger,
        &machine,
        &mut claims,
    );
    assert_eq!(busy.started, None);
    assert_eq!(busy.waits.len(), 32, "every binary was asked");
    assert!(
        busy.waits
            .iter()
            .all(|wait| matches!(wait, Some(test_waits::Wait::Unread(Some(pid))) if *pid == me)),
        "the lock decides nothing, and names its holder: {:?}",
        busy.waits
    );
    assert_eq!(busy.entries, 0, "no claim was laid");
    assert_eq!(busy.sweeps, 0, "no process table was swept");
    assert_eq!(
        busy.turn_tries, 1,
        "the retained request's poll met the lock, and only it"
    );
    assert_eq!(pass.lock().unwrap().again(look), LOCK_AGAIN);
    assert_eq!(retained(), (index, order), "the retained request stands");
    assert_eq!(ledger.list().unwrap(), records, "no record came or went");
    assert!(marked.same_mark(&mark()), "its wait mark is unbroken");

    // THE CONTROL: no shared look state, so each binary enters afresh under the busy lock.
    let (control, _) = look_once(&state, None, &waiters, &ctx, &ledger, &machine, &mut claims);
    assert_eq!(control.started, None);
    assert!(
        control.entries > 0 && control.turn_tries > 2,
        "the control lays claims and tries the lock for each: {} entries, {} tries",
        control.entries,
        control.turn_tries
    );
    drop(turn);

    // A decided look with the lock let go: the observation rejects the others without entering
    // them, and the mark is still the same one.
    let (decided, _) = look_once(
        &state,
        Some(&pass),
        &waiters,
        &ctx,
        &ledger,
        &machine,
        &mut claims,
    );
    assert_eq!(decided.started, None);
    assert_eq!(decided.entries, 0, "only the retained request polled");
    assert!(
        decided
            .waits
            .iter()
            .all(|wait| matches!(wait, Some(test_waits::Wait::For(_)))),
        "{:?}",
        decided.waits
    );
    assert_eq!(retained(), (index, order));
    assert!(marked.same_mark(&mark()), "its wait mark is unbroken");

    // Room behind the lock: a binary that fits waits only for the lock, and the first look after
    // the lock is let go admits it, the retained request at its place.
    drop(holder);
    machine.fixed.set(Reading::of_gib(48, 40, 14));
    let turn = ledger.hold_turn().unwrap().unwrap();
    let (behind, _) = look_once(
        &state,
        Some(&pass),
        &waiters,
        &ctx,
        &ledger,
        &machine,
        &mut claims,
    );
    assert_eq!(behind.started, None, "nothing starts without the lock");
    assert_eq!(behind.entries, 0, "no claim was laid");
    assert!(claims.is_empty());
    assert_eq!(retained(), (index, order));
    drop(turn);
    let (admitting, _) = look_once(
        &state,
        Some(&pass),
        &waiters,
        &ctx,
        &ledger,
        &machine,
        &mut claims,
    );
    assert_eq!(admitting.started, Some(index));
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].record().order(), order, "admitted at its place");
    claims.clear();

    // The lock met first by the shared observation: binary 0 can never fit (64 GiB on a 48 GiB
    // machine, in a run whose book sets no budget, so nothing is asked alone), so a dispatch's
    // first look retains binary 1, and each later look asks the observation about binary 0
    // before the retained request polls.
    let mut late_ctx = crate::Ctx::new(
        dir.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    );
    late_ctx.memory = Some(crate::memory::Book::at(None, Some(48 << 30), None));
    let holder = aterm_capacity::admit::try_admit_with(
        &ledger,
        &machine,
        Need::new(&holder_name).mem_gib(8),
    )
    .unwrap()
    .unwrap();
    machine.fixed.set(Reading::of_gib(48, 8, 14));
    let mut plan = test_plan::plan(&vec![None; 32], &[], 4, None, 4);
    for binary in &mut plan.order {
        binary.peak = 4 << 30;
        binary.charge = 1;
    }
    plan.order[0].peak = 64 << 30;
    let late = Mutex::new(Dispatcher::new(&plan, None));
    let late_pass = Mutex::new(AdmissionPass::default());
    let late_waiters = BinaryWaiters::default();
    let late_retained = || {
        late_waiters
            .state
            .lock()
            .unwrap()
            .pending
            .as_ref()
            .map(|(index, _)| *index)
    };
    let (first, _) = look_once(
        &late,
        None,
        &late_waiters,
        &late_ctx,
        &ledger,
        &machine,
        &mut claims,
    );
    assert_eq!(first.started, None);
    assert_eq!(late_retained(), Some(1), "binary 0 entered nothing");
    let records = ledger.list().unwrap();
    let turn = ledger.hold_turn().unwrap().unwrap();
    let (busy, look) = look_once(
        &late,
        Some(&late_pass),
        &late_waiters,
        &late_ctx,
        &ledger,
        &machine,
        &mut claims,
    );
    assert_eq!(busy.started, None);
    assert_eq!(busy.waits.len(), 32, "every binary was asked");
    assert!(
        busy.waits
            .iter()
            .all(|wait| matches!(wait, Some(test_waits::Wait::Unread(Some(pid))) if *pid == me)),
        "the observation's lock answers binary 0, and the look: {:?}",
        busy.waits
    );
    assert_eq!(
        (busy.entries, busy.sweeps, busy.turn_tries),
        (0, 0, 2),
        "no claim, no sweep; the observation's try and the retained request's poll"
    );
    assert_eq!(late_pass.lock().unwrap().again(look), LOCK_AGAIN);
    assert_eq!(late_retained(), Some(1), "the retained request stands");
    assert_eq!(ledger.list().unwrap(), records, "no record came or went");
    drop(turn);
    late_waiters.clear();
    drop(holder);
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
    use aterm_capacity::admit::{AdmissionAttempt, ProbeRejection, Short, Shortfall};
    if let Some((pass, generation)) = pass
        && let Some(rejected) = pass
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .rejection(
                test_plan::Look { generation, seq: 0 },
                ledger,
                machine,
                &need,
                None,
                clock,
            )
    {
        return Ok(match rejected {
            ProbeRejection::Waiting(short) => AdmissionAttempt::Waiting(short),
            ProbeRejection::Unreachable(why) => AdmissionAttempt::Unreachable(why),
            ProbeRejection::Turn(holder) => AdmissionAttempt::Waiting(Shortfall {
                shorts: vec![Short::Turn { holder }],
                holders: Vec::new(),
            }),
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

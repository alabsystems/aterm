// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn blocking_preserves_observed_fields_and_refuses_missing_evidence() {
    let original = Reading {
        cores: 8,
        load1: Some(1.0),
        load5: Some(2.0),
        available: Some(8 << 30),
        physical: Some(16 << 30),
        wired: Some(2 << 30),
        ..Reading::default()
    };
    let mut blocked = original.clone();
    assert!(block_observed_memory(&mut blocked));
    let mut expected = original.clone();
    expected.available = Some(0);
    assert_eq!(blocked, expected);
    for mut missing in [
        Reading {
            available: None,
            ..original.clone()
        },
        Reading {
            available: Some(17 << 30),
            ..original.clone()
        },
        Reading {
            load5: None,
            ..original.clone()
        },
        Reading {
            load1: Some(f64::INFINITY),
            ..original.clone()
        },
        Reading {
            wired: None,
            ..original.clone()
        },
    ] {
        let before = missing.clone();
        assert!(!block_observed_memory(&mut missing));
        assert_eq!(missing, before);
    }
}

/// A MEASUREMENT, run by hand on macOS (`targo --unverified test -p aterm-verify --lib --
/// --ignored --exact testrun::admission_bench::tests::two_dispatches_waiting_behind_a_held_claim
/// --nocapture`): two dispatches of a landing judgment's size (300 binaries, 14 workers each)
/// wait a minute behind a claim held in a private ledger, on Live observations with available
/// memory read as none, each through the real look (`admitted_holding_observed`,
/// `binary_permit_in`) writing its wait rows to its own events file. They share the ledger's
/// lock as two judgments do. It prints the process's CPU seconds and each dispatch's events
/// bytes per waiting minute, with the looks and lock tries that cost them.
#[test]
#[ignore = "a measurement of a minute, run by hand"]
fn two_dispatches_waiting_behind_a_held_claim() {
    const BINARIES: usize = 300;
    const DISPATCH_WORKERS: u32 = 14;
    const WARM: Duration = Duration::from_secs(5);
    const SPAN: Duration = Duration::from_secs(60);
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    let reading = aterm_capacity::Live.read(None);
    assert!(complete_reading(&reading), "a Live reading");
    let physical = reading.physical.unwrap();
    let mut holder_reading = reading.clone();
    holder_reading.available = holder_reading.reachable();
    let holder = aterm_capacity::admit::try_admit_with(
        &ledger,
        &HolderMachine(holder_reading),
        Need::new("LS-74 measurement holder").mem_gib(1),
    )
    .unwrap()
    .expect("the holder is admitted");
    let machine = BlockedLive {
        reads: AtomicUsize::new(0),
        sweeps: AtomicUsize::new(0),
        lineages: AtomicUsize::new(0),
        waits: AtomicUsize::new(0),
        unavailable: AtomicUsize::new(0),
    };
    let bins: Vec<Binary> = (0..BINARIES)
        .map(|i| Binary {
            header: format!("t{i}"),
            spec: format!("-p x --test t{i}"),
            record: crate::testrun::Record {
                cwd: dir.path().to_owned(),
                argv: vec![format!("/r/t{i}").into()],
                env: Vec::new(),
            },
            harness: false,
        })
        .collect();
    let mut plan = test_plan::plan(&vec![None; BINARIES], &[], DISPATCH_WORKERS, None, 1 << 30);
    for binary in &mut plan.order {
        binary.peak = 1 << 30;
        binary.charge = 1;
    }
    let events: Vec<PathBuf> = (0..2)
        .map(|i| dir.path().join(format!("events-{i}")))
        .collect();
    let ctxs: Vec<crate::Ctx> = events
        .iter()
        .map(|events| {
            let mut ctx = crate::Ctx::new(
                dir.path().to_owned(),
                crate::Scope::workspace(),
                crate::EnvSnapshot::default(),
                dir.path().join("scratch"),
            )
            .with_events(Some(crate::event_log::EventLog::open(events).unwrap()));
            ctx.memory = Some(crate::memory::Book::at(
                None,
                Some(physical),
                Some(physical),
            ));
            ctx
        })
        .collect();
    let states: Vec<Mutex<Dispatcher>> = (0..2)
        .map(|_| Mutex::new(Dispatcher::new(&plan, None)))
        .collect();
    let passes: Vec<Mutex<AdmissionPass>> = (0..2)
        .map(|_| Mutex::new(AdmissionPass::default()))
        .collect();
    let waiters: Vec<BinaryWaiters> = (0..2).map(|_| BinaryWaiters::default()).collect();
    let turns: Vec<Condvar> = (0..2).map(|_| Condvar::new()).collect();
    let traces: Vec<test_waits::Trace<'_>> = ctxs
        .iter()
        .map(|ctx| test_waits::Trace::new(Some(ctx), &bins, &plan, Instant::now()))
        .collect();
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let size = |path: &PathBuf| std::fs::metadata(path).map_or(0, |m| m.len());
    let (cpu_ns, bytes, looks, tries) = std::thread::scope(|scope| {
        for d in 0..2 {
            for _ in 0..DISPATCH_WORKERS {
                let (state, pass, waiters, turn, trace, ctx, ledger, machine, stopped) = (
                    &states[d],
                    &passes[d],
                    &waiters[d],
                    &turns[d],
                    &traces[d],
                    &ctxs[d],
                    &ledger,
                    &machine,
                    &stopped,
                );
                scope.spawn(move || {
                    let enter = |need, place: Option<&aterm_capacity::admit::Place>| {
                        enter_in(Some(ledger), machine, need, place)
                    };
                    let asking = Asking {
                        ledger: Some(ledger),
                        machine,
                        enter: &enter,
                    };
                    let next = admitted_holding_observed(
                        state,
                        turn,
                        &|| 0.0,
                        &|| stopped.load(Ordering::SeqCst),
                        &|| {
                            if stopped.load(Ordering::SeqCst) {
                                waiters.clear();
                            }
                        },
                        &mut |look, b, _| {
                            let (permit, wait) = binary_permit_in(
                                Some(ctx),
                                None,
                                b,
                                waiters,
                                &|| false,
                                Some((pass, look)),
                                &asking,
                            );
                            if permit.is_some() {
                                trace.started(b.index);
                            } else if let Some(wait) = wait {
                                trace.waits(b.index, wait);
                            }
                            permit
                        },
                        LookHooks {
                            rejected:
                                &mut |b: &Planned, resources: test_plan::DispatchResources| {
                                    trace.waits(
                                        b.index,
                                        test_waits::Wait::For(test_waits::Reason::local(
                                            b, resources,
                                        )),
                                    );
                                },
                            again: &|look| {
                                pass.lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .again(look)
                            },
                        },
                    );
                    assert!(next.is_none(), "nothing starts behind the held claim");
                });
            }
        }
        std::thread::sleep(WARM);
        let looks_at = || {
            states
                .iter()
                .map(|s| s.lock().unwrap_or_else(PoisonError::into_inner).looks())
                .collect::<Vec<_>>()
        };
        let (cpu, bytes, looks, tries) = (
            counters().unwrap().cpu_ns,
            events.iter().map(size).collect::<Vec<_>>(),
            looks_at(),
            ledger.turn_tries(),
        );
        std::thread::sleep(SPAN);
        let measured = (
            counters().unwrap().cpu_ns - cpu,
            events
                .iter()
                .zip(&bytes)
                .map(|(path, before)| size(path) - before)
                .collect::<Vec<_>>(),
            looks_at()
                .iter()
                .zip(&looks)
                .map(|(after, before)| after - before)
                .collect::<Vec<_>>(),
            ledger.turn_tries() - tries,
        );
        stopped.store(true, Ordering::SeqCst);
        for turn in &turns {
            turn.notify_all();
        }
        measured
    });
    for trace in &traces {
        trace.finish_pending();
    }
    assert_eq!(
        machine.unavailable.load(Ordering::SeqCst),
        0,
        "Live answered throughout"
    );
    drop(holder);
    let cpu = Duration::from_nanos(cpu_ns).as_secs_f64();
    println!(
        "LS-74 two dispatches waiting behind a held claim, {BINARIES} binaries and \
         {DISPATCH_WORKERS} workers each, over {} s of waiting: process CPU {cpu:.2} s ({:.2} s \
         each); events bytes {bytes:?}; looks {looks:?}; ledger lock tries {tries}; Live reads \
         {}, process sweeps {}",
        SPAN.as_secs(),
        cpu / 2.0,
        machine.reads.load(Ordering::SeqCst),
        machine.sweeps.load(Ordering::SeqCst),
    );
}

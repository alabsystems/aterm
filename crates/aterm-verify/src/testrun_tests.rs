// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A scratch ledger and each claim asked of it: memory, cores, requested admission mode, and
/// whether this process held a cores claim as it was asked.
type Scratch = Option<(
    aterm_capacity::Ledger,
    Vec<(u64, u32, aterm_capacity::Admit, bool)>,
)>;

thread_local! {
    /// The scratch ledger [`scratch_admit`] admits in, and the claims asked of it.
    static SCRATCH: std::cell::RefCell<Scratch> = const { std::cell::RefCell::new(None) };
}

/// Admit `need` at once in the thread's scratch ledger, on a machine with room for it, noting
/// what it asked for and whether this process held cores as it asked.
fn scratch_admit(
    need: aterm_capacity::Need,
    mode: aterm_capacity::Admit,
) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
    SCRATCH.with_borrow_mut(|scratch| {
        let (ledger, asked) = scratch.as_mut().expect("a scratch ledger");
        let cores_held = ledger
            .of_pid(std::process::id())
            .expect("the scratch ledger reads")
            .iter()
            .any(|r| r.held.is_some_and(|h| h.cores > 0));
        asked.push((need.mem, need.cores, mode, cores_held));
        crate::admit_in(
            ledger,
            &crate::timing_test_machine::Measured::new(&aterm_capacity::Fixed::new(
                aterm_capacity::Reading::of_gib(48, 40, 14),
            )),
            aterm_capacity::Pace::default(),
            need,
            // Unit tests record Background admission but need not own their harness's
            // process group. The directly spawned candidate verifier exercises that real
            // precondition.
            if mode == aterm_capacity::Admit::Preemptible {
                aterm_capacity::Admit::Wait
            } else {
                mode
            },
            &mut |_| {},
        )
    })
}

/// Retrying a memory-starved binary retains its durable queue identity ahead of later work.
/// Clearing unstarted work removes its reservation. A one-shot retry drops the first ledger
/// entry and fails these checks before any resource becomes available.
#[test]
fn a_waiting_binary_keeps_its_admission_order_until_admitted_or_cancelled() {
    use aterm_capacity::admit::Waiter;
    let tmp = aterm_tempfile::tempdir().unwrap();
    let ledger = aterm_capacity::Ledger::at(tmp.path().join("ledger"));
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 14));
    let holder = match Waiter::enter(
        &ledger,
        &machine,
        aterm_capacity::Need::new("competing work").mem(64 << 20),
    )
    .unwrap()
    .poll(&machine)
    {
        aterm_capacity::admit::Polled::Admitted(claim) => claim,
        other => panic!("the competing claim should fit: {other:?}"),
    };
    machine.set(aterm_capacity::Reading::of_gib(48, 0, 14));
    let waiters = BinaryWaiters::default();
    let need = || aterm_capacity::Need::new("waiting binary").mem(64 << 20);
    let enter =
        |need| Waiter::enter(&ledger, &machine, need).map_err(aterm_capacity::Error::Ledger);
    assert!(
        waiters
            .poll_noting(7, need(), &machine, enter,
                &mut |_| panic!("the first blocked binary must enter the fair queue, regardless of a cached shortfall"),
                &mut None)
            .expect("entering the first waiting request")
            .is_none()
    );
    let records = ledger
        .of_pid(std::process::id())
        .expect("reading the queued request");
    assert_eq!(
        records.len(),
        2,
        "the competing holder and denied attempt remain"
    );
    let original = records
        .iter()
        .find(|record| record.held.is_none())
        .unwrap()
        .order();
    let later = Waiter::enter(&ledger, &machine, need()).unwrap();
    assert_eq!(original.1, later.record().pid);
    assert!(original.2 < later.record().seq);
    assert!(
        waiters
            .poll_with(8, need(), &machine, enter)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        ledger.of_pid(std::process::id()).unwrap().len(),
        3,
        "a blocked peer must not reserve the whole plan against later work"
    );
    for _ in 0..3 {
        assert!(
            waiters
                .poll_with(7, need(), &machine, |_| {
                    panic!("a retry must poll the original waiter")
                })
                .expect("polling the retained request")
                .is_none()
        );
        let records = ledger.of_pid(std::process::id()).unwrap();
        assert_eq!(
            records.len(),
            3,
            "the holder and one entry per queued request"
        );
        assert!(records.iter().any(|record| record.order() == original));
    }
    let mut reason = None;
    assert!(
        waiters
            .poll_noting(
                8,
                need(),
                &machine,
                |_| panic!("a cached rejection must not create a temporary waiter"),
                &mut |_| Some(aterm_capacity::admit::ProbeRejection::Unreachable(
                    "observed shortfall".into()
                )),
                &mut reason,
            )
            .unwrap()
            .is_none()
    );
    assert!(reason.is_some());
    assert!(
        waiters
            .poll_noting(
                7,
                need(),
                &machine,
                |_| panic!("the retained waiter must keep its original queue identity"),
                &mut |_| panic!(
                    "the retained waiter must poll fresh instead of consulting a stale rejection"
                ),
                &mut None,
            )
            .unwrap()
            .is_none()
    );
    assert!(
        ledger
            .of_pid(std::process::id())
            .unwrap()
            .iter()
            .any(|record| record.order() == original)
    );
    machine.set(aterm_capacity::Reading::of_gib(48, 40, 14));
    let claim = waiters
        .poll_with(7, need(), &machine, |_| {
            panic!("admission must retain the original waiter")
        })
        .expect("polling after memory becomes available")
        .expect("the retained request is admitted with memory available");
    assert_eq!(claim.record().order(), original);
    drop((claim, later));
    machine.set(aterm_capacity::Reading::of_gib(48, 0, 14));
    assert!(
        waiters
            .poll_with(11, need(), &machine, enter)
            .unwrap()
            .is_none()
    );
    waiters.clear();
    assert_eq!(
        ledger.of_pid(std::process::id()).unwrap().len(),
        1,
        "cancellation leaves the competing holder alone"
    );
    drop(holder);
    assert!(ledger.of_pid(std::process::id()).unwrap().is_empty());
}

/// A decisive red closes the dispatch under the same mutex as a start, before its event is
/// written: a worker that already read the external stop but has not taken that mutex finds
/// the head still queued. The run-wide halt is armed afterward. NEGATIVE CONTROL: removing
/// `mark_red_decided`'s synchronized halt starts that head; a non-red outcome, or a red in a
/// run that does not stop, leaves a fresh dispatcher live.
#[test]
fn a_decisive_red_stops_queued_binaries_before_its_event_is_written() {
    let plan = test_plan::plan(&[None], 1, None, 1);
    let state = Mutex::new(Dispatcher::new(&plan, Some(1)));
    let turn = Condvar::new();
    let (stop_read, read_stop) = std::sync::mpsc::channel();
    let (may_lock, lock_now) = std::sync::mpsc::channel();
    let lock_now = Mutex::new(lock_now);
    let visits = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let after_stop = || {
            if visits.fetch_add(1, Ordering::SeqCst) == 0 {
                stop_read.send(()).expect("the test hears the stop read");
                lock_now
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(60))
                    .expect("the admission lock was not released within one minute");
            }
        };
        let admission_state = &state;
        let admission_turn = &turn;
        let attempt = scope.spawn(move || {
            admitted_after_stop(
                admission_state,
                admission_turn,
                &|| 0.0,
                &|| false,
                &after_stop,
            )
        });
        read_stop
            .recv_timeout(Duration::from_secs(60))
            .expect("the admission did not read stop within one minute");
        let halt = || halt_dispatch(&state, &turn);
        assert!(mark_red_decided(
            Outcome::Fail(Severity::GateFailed),
            true,
            Some(&halt)
        ));
        may_lock
            .send(())
            .expect("the admission still waits to take the lock");
        assert_eq!(attempt.join().unwrap(), None);
    });
    assert_eq!(
        state.lock().unwrap().phase(0),
        Some(crate::test_plan::Phase::Queued)
    );

    let halts = AtomicUsize::new(0);
    let count_halt = || {
        halts.fetch_add(1, Ordering::SeqCst);
    };
    assert!(!mark_red_decided(Outcome::Ok, true, Some(&count_halt)));
    assert!(!mark_red_decided(
        Outcome::Fail(Severity::GateFailed),
        false,
        Some(&count_halt)
    ));
    assert_eq!(halts.load(Ordering::SeqCst), 0);

    let live = Mutex::new(Dispatcher::new(&plan, Some(1)));
    assert_eq!(
        admitted(&live, &turn, &|| 0.0, &|| false).map(|b| b.index),
        Some(0),
        "without a stopping red, the same queued binary starts"
    );
}

/// Empty history and a timed sample whose tree peak was unread both plan a positive cold
/// peak and therefore a positive run claim. NEGATIVE CONTROL: leaving an unseen or zero-peak
/// Seen at zero makes its corresponding assertion fail and restores unclaimed concurrency.
#[test]
fn binaries_without_a_measured_peak_get_a_positive_plan_and_claim() {
    let binary = Binary {
        harness: false,
        header: String::new(),
        spec: "-p fixture --lib".to_owned(),
        record: Record {
            cwd: PathBuf::new(),
            argv: Vec::new(),
            env: Vec::new(),
        },
    };
    let mut history = crate::test_history::History::default();
    let physical = Some(48_u64 << 30);
    for known_without_peak in [false, true] {
        if known_without_peak {
            history.push(
                &binary.spec,
                crate::test_history::Sample {
                    partition: None,
                    exe_sha256: None,
                    at_ms: 1,
                    outcome: "ok".to_owned(),
                    wall_ms: 1000,
                    cpu_ms: Some(1000),
                    starved_ms: Some(0),
                    peak_bytes: None,
                    width: 1,
                    cores: None,
                    tests: Some(1),
                },
            );
        }
        let fallback = fallback_peak(&history, &[], physical);
        assert!(fallback > 0);
        let plan = plan_from_history(&history, std::slice::from_ref(&binary), 4, None, fallback);
        assert_eq!(plan.order[0].peak, fallback, "{plan:?}");
        assert!(plan_claim(&plan).is_some(), "{plan:?}");
        assert!(
            aterm_capacity::peak_claim(fallback) > 0,
            "an unseen binary's fallback peak is positive"
        );
    }
}

/// The actual admission asks for the record's bytes, without a stage envelope or a GiB
/// floor per measured binary. NEGATIVE CONTROL: an unrecorded peer keeps the conservative bound;
/// making all charges tiny fails its exact admitted-memory assertion.
#[test]
fn measured_binary_reservations_release_individually_and_unrecorded_keeps_its_bound() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    SCRATCH.set(Some((
        aterm_capacity::Ledger::at(tmp.path().join("ledger")),
        Vec::new(),
    )));
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    let mut history = crate::test_history::History::default();
    let known = Binary {
        harness: false,
        header: String::new(),
        spec: "known".into(),
        record: Record {
            cwd: PathBuf::new(),
            argv: Vec::new(),
            env: Vec::new(),
        },
    };
    let unknown = Binary {
        spec: "unrecorded".into(),
        ..known.clone()
    };
    let bytes = 64 << 20;
    let sample = crate::test_history::Sample {
        partition: None,
        exe_sha256: None,
        at_ms: 1,
        outcome: "ok".into(),
        wall_ms: 1000,
        cpu_ms: Some(1000),
        starved_ms: Some(0),
        peak_bytes: Some(bytes),
        width: 1,
        cores: None,
        tests: Some(1),
    };
    history.push(&known.spec, sample.clone());
    history.push(
        "bound-record",
        crate::test_history::Sample {
            peak_bytes: Some(16 << 30),
            ..sample
        },
    );
    let fallback = fallback_peak(&history, &[], Some(48 << 30));
    assert_eq!(
        fallback,
        16 << 30,
        "the heaviest recorded selection bounds unrecorded work"
    );
    let plan = plan_from_history(&history, &[known, unknown], 4, None, fallback);
    let attempt = &mut |need, mode: Option<aterm_capacity::Admit>| {
        scratch_admit(need, mode.unwrap_or(aterm_capacity::Admit::Wait)).map(Some)
    };
    for mode in [None, Some(aterm_capacity::Admit::Now)] {
        ctx.quiet = mode;
        for (index, expected) in [(0, bytes), (1, fallback)] {
            let planned = plan.order.iter().find(|p| p.index == index).unwrap();
            let permit = binary_permit_with(Some(&ctx), None, planned, attempt)
                .unwrap()
                .unwrap();
            assert_eq!(permit.claim.as_ref().unwrap().record().need.mem, expected);
            assert_eq!(permit.claim.as_ref().unwrap().cores(), planned.charge);
            drop(permit);
            SCRATCH.with_borrow(|scratch| {
                assert!(
                    scratch
                        .as_ref()
                        .unwrap()
                        .0
                        .of_pid(std::process::id())
                        .unwrap()
                        .is_empty(),
                    "no stage reservation remains"
                )
            });
        }
    }
    SCRATCH.with_borrow(|scratch| {
        assert_eq!(
            scratch
                .as_ref()
                .unwrap()
                .1
                .iter()
                .map(|asked| asked.2)
                .collect::<Vec<_>>(),
            [
                aterm_capacity::Admit::Wait,
                aterm_capacity::Admit::Wait,
                aterm_capacity::Admit::Now,
                aterm_capacity::Admit::Now
            ]
        )
    });
    SCRATCH.set(None);
}

/// A TEST RUN CLAIMS MEMORY AND CORES ATOMICALLY: the recording pass has one combined claim and
/// releases it before per-binary dispatch. It therefore holds neither resource while waiting for
/// the other. NEGATIVE CONTROL: the old memory-then-cores ordering produces four requests, while
/// a claim taken beside the first still sees its combined claim held.
#[test]
fn a_test_run_claims_memory_and_cores_atomically() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    SCRATCH.set(Some((
        aterm_capacity::Ledger::at(tmp.path().join("ledger")),
        Vec::new(),
    )));
    let asked = || SCRATCH.with_borrow_mut(|s| std::mem::take(&mut s.as_mut().expect("scratch").1));
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    let gib = 1_u64 << 30;
    let first = claim_both(&ctx, "t", None, 2 * gib, "one", 6, scratch_admit).expect("admitted");
    assert!(first.resources.is_some());
    assert_eq!(first.granted, 6);
    let plan =
        claim_both(&ctx, "t", Some(first), 4 * gib, "plan", 6, scratch_admit).expect("admitted");
    assert_eq!(
        asked(),
        [
            (2 * gib, 6, aterm_capacity::Admit::Wait, false),
            (4 * gib, 6, aterm_capacity::Admit::Wait, false),
        ]
    );
    let beside =
        claim_both(&ctx, "t", None, 2 * gib, "beside", 6, scratch_admit).expect("admitted");
    assert_eq!(asked(), [(2 * gib, 6, aterm_capacity::Admit::Wait, true)]);
    drop((plan, beside));
    SCRATCH.set(None);
}

/// UNDER THE RELEASE'S POOL a test run claims its memory as ever and no cores: the pool's
/// slots are its grant, which the release's own core claim bounds, and a claim of its own
/// would count the release's other branches' and wait for them. NEGATIVE CONTROL: the same
/// run with no pool asks for memory and cores together.
#[cfg(unix)]
#[test]
fn a_run_under_the_releases_pool_claims_its_memory_and_no_cores() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    SCRATCH.set(Some((
        aterm_capacity::Ledger::at(tmp.path().join("ledger")),
        Vec::new(),
    )));
    let asked = || SCRATCH.with_borrow_mut(|s| std::mem::take(&mut s.as_mut().expect("scratch").1));
    let release = aterm_capacity::Jobserver::new(&tmp.path().join("qualify.jobs"), 5)
        .expect("the release's pool");
    let ctx = |pool| {
        let mut ctx = crate::Ctx::new(
            tmp.path().to_path_buf(),
            crate::Scope::workspace(),
            crate::EnvSnapshot {
                release_pool: Some(release.handed()),
                ..crate::EnvSnapshot::default()
            },
            tmp.path().join("scratch"),
        )
        .with_pool(pool);
        ctx.memory = Some(crate::memory::Book::at(
            None,
            Some(48 << 30),
            Some(40 << 30),
        ));
        ctx
    };
    let pooled = ctx(Some(
        aterm_capacity::Jobserver::adopt(&release.handed()).expect("adopted"),
    ));
    let gib = 1_u64 << 30;
    let claims =
        claim_both(&pooled, "t", None, 2 * gib, "one", 5, scratch_admit).expect("admitted");
    assert!(claims.resources.is_some(), "its memory is claimed as ever");
    assert_eq!(claims.granted, 5, "the pool's slots are its grant");
    assert_eq!(
        asked(),
        [(2 * gib, 0, aterm_capacity::Admit::Wait, false)],
        "and no cores are asked for"
    );
    drop(claims);
    let unpooled = ctx(None);
    let claims =
        claim_both(&unpooled, "t", None, 2 * gib, "one", 5, scratch_admit).expect("admitted");
    assert_eq!(asked(), [(2 * gib, 5, aterm_capacity::Admit::Wait, false)]);
    drop(claims);
    SCRATCH.set(None);
}

/// A repeatable tree's recording pass asks once for memory and cores together, at
/// Background rank, and uses that one claim's actual core grant. NEGATIVE CONTROL: restoring
/// the split ordinary path is caught by `a_test_run_claims_memory_and_cores_atomically`.
#[test]
fn a_background_test_run_claims_memory_and_cores_once() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    SCRATCH.set(Some((
        aterm_capacity::Ledger::at(tmp.path().join("ledger")),
        Vec::new(),
    )));
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    ctx.background = true;
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    let gib = 1_u64 << 30;
    let claims = claim_both(&ctx, "t", None, 2 * gib, "one", 6, scratch_admit).expect("admitted");
    assert!(claims.resources.is_some());
    assert_eq!(claims.granted, 6);
    let asked = SCRATCH.with_borrow_mut(|s| std::mem::take(&mut s.as_mut().expect("scratch").1));
    assert_eq!(
        asked,
        [(2 * gib, 6, aterm_capacity::Admit::Preemptible, false)]
    );
    drop(claims);
    SCRATCH.set(None);
}

/// A 12-core machine whose 5-minute load is 24, none of it claimed in the ledger: what a run's
/// own test compile, which holds no core claim, leaves as its test stage starts.
fn after_its_compile() -> aterm_capacity::Reading {
    aterm_capacity::Reading {
        load1: Some(24.0),
        load5: Some(24.0),
        ..aterm_capacity::Reading::of_gib(48, 40, 12)
    }
}

/// [`scratch_admit`]'s ledger on the machine [`after_its_compile`].
fn admit_after_its_compile(
    need: aterm_capacity::Need,
    mode: aterm_capacity::Admit,
) -> Result<aterm_capacity::Claim, aterm_capacity::Error> {
    SCRATCH.with_borrow_mut(|scratch| {
        let (ledger, _) = scratch.as_mut().expect("a scratch ledger");
        crate::admit_in(
            ledger,
            &crate::timing_test_machine::Measured::new(&aterm_capacity::Fixed::new(
                after_its_compile(),
            )),
            aterm_capacity::Pace::default(),
            need,
            mode,
            &mut |_| {},
        )
    })
}

/// One second of one test on one thread, kept for each of `bins`: a binary planned one thread
/// wide and charged one core.
fn one_core_history(bins: &[Binary]) -> crate::test_history::History {
    let mut history = crate::test_history::History::default();
    for binary in bins {
        history.push(
            &binary.spec,
            crate::test_history::Sample {
                partition: None,
                exe_sha256: None,
                at_ms: 1,
                outcome: "ok".into(),
                wall_ms: 1000,
                cpu_ms: Some(100),
                starved_ms: Some(0),
                peak_bytes: Some(64 << 20),
                width: 1,
                cores: None,
                tests: Some(1),
            },
        );
    }
    history
}

/// A RUN'S BINARIES TOGETHER HOLD THE SHARE ONE CLAIM WOULD BE GRANTED, READ AGAIN AT EACH
/// START. A run that asks for 12 cores as its own test compile ends is granted 4: the ledger
/// shares the machine with the 5-minute load that compile left, which no claim holds. Twelve
/// binaries of one core each are then dispatched within the 12 asked, each claiming its own
/// core in that ledger, on the run's live share: four run at once while the load stands, and
/// all twelve once it has gone. NEGATIVE CONTROL: with no live share the ledger grants each of
/// the twelve claims its core at that same load, and twelve run at once on a machine whose
/// share for the run is four.
#[test]
fn a_runs_binaries_together_hold_the_share_one_claim_would_be_granted() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let at = tmp.path().join("ledger");
    SCRATCH.set(Some((aterm_capacity::Ledger::at(at.clone()), Vec::new())));
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    let claims = claim_both(
        &ctx,
        "t",
        None,
        1 << 30,
        "for the recording pass",
        12,
        admit_after_its_compile,
    )
    .expect("admitted");
    assert_eq!(
        claims.granted, 4,
        "12 cores shared with a 5-minute load of two machines' worth"
    );
    let cores = Cores {
        asked: 12,
        granted: claims.granted,
    };
    // The recording claim goes back before the dispatch, as `run_planned_observed` drops it.
    drop(claims);
    SCRATCH.set(None);

    let bins: Vec<Binary> = (0..12)
        .map(|i| Binary {
            harness: false,
            header: format!("t{i}"),
            spec: format!("-p x --test t{i}"),
            record: rec(&format!("/r/t{i}"), "x"),
        })
        .collect();
    let history = one_core_history(&bins);
    let fallback = fallback_peak(&history, &[], Some(48 << 30));
    let plan = dispatch_plan(&history, &bins, cores, None, fallback);
    assert_eq!(plan.share, 12, "dispatched within the cores asked");
    assert!(plan.order.iter().all(|b| b.charge == 1), "{plan:?}");
    let machine = aterm_capacity::Fixed::new(after_its_compile());
    // The most binaries that held a core of the ledger at once, each waiting, while it holds
    // its core, for `together` of them to have started.
    let most_at_once = |share: Option<&crate::test_share::Live>, together: usize| {
        let admitting = Mutex::new(());
        let running = Mutex::new((0_usize, 0_usize));
        let started = (Mutex::new(0_usize), Condvar::new());
        let ran = dispatch_resources(
            &bins,
            &plan,
            None,
            None,
            None,
            share,
            &|| 0.0,
            &|| false,
            &|| false,
            |b, planned, _| {
                let permit = {
                    let _one_at_a_time = admitting.lock().unwrap();
                    binary_permit_with(Some(&ctx), None, planned, &mut |need, mode| {
                        assert_eq!(mode, None, "the ledger keeps its authority");
                        aterm_capacity::admit::try_admit_with(
                            &aterm_capacity::Ledger::at(at.clone()),
                            &machine,
                            need,
                        )
                    })
                }
                .unwrap_or_else(|| panic!("{}: the ledger held its claim back", b.spec))
                .expect("the ledger admits");
                assert_eq!(
                    permit.claim.as_ref().expect("a ledger claim").cores(),
                    1,
                    "{} runs on a core of its own claim",
                    b.spec
                );
                {
                    let mut running = running.lock().unwrap();
                    running.0 += 1;
                    running.1 = running.1.max(running.0);
                }
                *started.0.lock().unwrap() += 1;
                started.1.notify_all();
                let (count, waited) = started
                    .1
                    .wait_timeout_while(
                        started.0.lock().unwrap(),
                        aterm_spec::wait::MUST_HAPPEN,
                        |n| *n < together,
                    )
                    .unwrap();
                assert!(
                    *count >= together && !waited.timed_out(),
                    "{together} binaries never ran together"
                );
                drop(count);
                running.lock().unwrap().0 -= 1;
                drop(permit);
                Some(Run {
                    ok: true,
                    code: Some(0),
                    ..Run::default()
                })
            },
            |_, _, _| {},
        );
        assert!(ran.iter().all(Option::is_some), "every binary ran");
        running.lock().unwrap().1
    };
    // The run's share, read from that ledger on that machine: its own binaries are the claim.
    let live = || {
        let asker = crate::test_share::Asker {
            pid: std::process::id(),
            ancestors: Vec::new(),
            rank: aterm_capacity::Rank::Ordinary,
            since_ms: 0,
        };
        let (at, machine) = (at.clone(), &machine);
        let reading = aterm_capacity::Machine::read(machine, None);
        crate::test_share::Live::new(
            cores.asked,
            cores.granted,
            Box::new(move || {
                Some(crate::test_share::Look {
                    machine: reading.clone(),
                    others: crate::test_share::Others::of(
                        &aterm_capacity::Ledger::at(at.clone()).list().ok()?,
                        &asker,
                        0,
                    ),
                })
            }),
            None,
        )
    };
    let loaded = live();
    assert_eq!(
        most_at_once(Some(&loaded), 4),
        4,
        "the share one claim of 12 is granted at that load"
    );
    assert_eq!(loaded.range(), Some((4, 4)));
    assert_eq!(
        most_at_once(None, 12),
        12,
        "the ledger grants each of twelve claims its core whatever the load"
    );
    machine.set(aterm_capacity::Reading::of_gib(48, 40, 12));
    assert_eq!(
        most_at_once(Some(&live()), 12),
        12,
        "the load gone, the run has the cores it asked"
    );
    assert!(
        aterm_capacity::Ledger::at(at)
            .of_pid(std::process::id())
            .unwrap()
            .is_empty(),
        "every claim went back"
    );
}

/// THE PLAN A RUN DISPATCHES IS PLANNED ON ITS RECORDING GRANT AND DISPATCHED WITHIN ITS ASK.
/// A run granted 4 of the 12 cores it asked plans a binary of twelve one-second tests no wider
/// than 4 threads and charges it no more than 4 cores, as its kept runs measured it, and the
/// plan's share is the 12 asked, so three such binaries fit side by side. NEGATIVE CONTROL:
/// planned on the 12 asked, the same binary is 12 threads wide, a width none of its kept runs
/// measured its memory or its subprocess slots at.
#[test]
fn a_runs_plan_keeps_the_recording_grants_widths_within_the_share_asked() {
    let bins = vec![Binary {
        harness: false,
        header: "t".into(),
        spec: "-p x --test t".into(),
        record: rec("/r/t", "x"),
    }];
    let mut history = crate::test_history::History::default();
    history.push(
        "-p x --test t",
        crate::test_history::Sample {
            partition: None,
            exe_sha256: None,
            at_ms: 1,
            outcome: "ok".into(),
            wall_ms: 12_000,
            cpu_ms: Some(48_000),
            starved_ms: Some(0),
            peak_bytes: Some(64 << 20),
            width: 4,
            cores: None,
            tests: Some(12),
        },
    );
    let cores = Cores {
        asked: 12,
        granted: 4,
    };
    assert_eq!((cores.planned_on(), cores.dispatch_share()), (4, 12));
    assert!(cores.held_back());
    assert_eq!(
        cores.subprocess_slots(),
        subprocess_slots_for_cores(12),
        "the pool is made for the cores asked"
    );
    let plan = dispatch_plan(&history, &bins, cores, None, 1 << 30);
    assert_eq!(plan.share, 12);
    assert_eq!(
        (plan.order[0].width, plan.order[0].charge),
        (4, 4),
        "{plan:?}"
    );
    let on_the_ask = plan_from_history(&history, &bins, cores.dispatch_share(), None, 1 << 30);
    assert_eq!(on_the_ask.order[0].width, 12, "{on_the_ask:?}");
    let alone = Cores {
        asked: 12,
        granted: 12,
    };
    assert_eq!(
        dispatch_plan(&history, &bins, alone, None, 1 << 30),
        on_the_ask,
        "granted all it asked, a run plans as it always did"
    );
}

/// A FAILED TEST'S RERUN NAMES THE WIDTH AND CHARGE ITS BINARY RAN AT. A binary planned four
/// threads wide on 8 cores whose claim was granted 2 ran two threads wide on 2, and its rerun
/// says so; one the run never started, and every binary of a run that keys content on the
/// planned command, has the plan's. NEGATIVE CONTROL: the plan's own width for the first.
#[test]
fn a_rerun_names_the_width_and_charge_a_binary_ran_at() {
    let bins: Vec<Binary> = ["a", "b"]
        .iter()
        .map(|name| Binary {
            harness: false,
            header: (*name).into(),
            spec: format!("-p x --test {name}"),
            record: rec(&format!("/r/{name}"), "x"),
        })
        .collect();
    let planned = |index| Planned {
        index,
        width: 4,
        charge: 8,
        peak: 0,
        predicted: None,
    };
    let plan = Plan {
        share: 12,
        target: Duration::ZERO,
        order: vec![planned(0), planned(1)],
    };
    let granted = Ran {
        planned: Planned {
            width: 2,
            charge: 2,
            ..planned(0)
        },
        ..Ran::of(Run::default(), Duration::ZERO, Duration::from_secs(1))
    };
    let left = Ran {
        started: false,
        planned: planned(1),
        ..Ran::of(Run::default(), Duration::ZERO, Duration::ZERO)
    };
    let ran = [Some(granted), Some(left)];
    assert_eq!(
        ran_widths(&bins, &plan, &ran, false),
        [
            ("-p x --test a".to_owned(), 2, 2),
            ("-p x --test b".to_owned(), 4, 8)
        ]
    );
    assert_eq!(
        ran_widths(&bins, &plan, &ran, true),
        [
            ("-p x --test a".to_owned(), 4, 8),
            ("-p x --test b".to_owned(), 4, 8)
        ],
        "a content-keyed command keeps the plan's width whatever was granted"
    );
}

/// THE CORE-GRANT EVENT says what a claim asked and was granted, who granted it, the load it
/// was granted under and, for the recording pass, the subprocess slots open under it; the
/// dispatch's live share says the same of itself at a look that moved it, with what that look
/// read; and every reader of the events file knows its kind.
#[test]
fn a_core_grant_event_names_the_ask_the_grant_and_the_load() {
    let machine = after_its_compile();
    let standing = crate::test_share::Standing {
        share: 4,
        look: Some(crate::test_share::Look {
            machine: machine.clone(),
            others: crate::test_share::Others {
                held: 2,
                in_load: 1.5,
                waiting: 1,
                reserved: 0,
            },
        }),
        own_load: 0.25,
        slots: Some([2, 1]),
    };
    let line = share_event(12, &standing);
    let json = line.strip_prefix("verify-event: ").expect("an event line");
    let crate::event_log::Observation::Known { kind, value } = crate::event_log::observation(json)
    else {
        panic!("{json} is no event its readers know");
    };
    assert_eq!(kind, crate::event_log::EventKind::TestCoreGrant);
    assert_eq!(
        (value["scope"].as_str(), value["source"].as_str()),
        (Some("dispatch"), Some("live share"))
    );
    assert_eq!(
        (
            value["asked"].as_u64(),
            value["granted"].as_u64(),
            value["subprocess_slots"].as_u64(),
            value["machine_cores"].as_u64(),
            value["held_by_others"].as_u64(),
            value["waiting_ahead"].as_u64(),
        ),
        (Some(12), Some(4), Some(2), Some(12), Some(2), Some(1)),
        "{json}"
    );
    assert_eq!(
        (
            value["load5"].as_f64(),
            value["others_in_load"].as_f64(),
            value["own_load"].as_f64()
        ),
        (Some(24.0), Some(1.5), Some(0.25)),
        "{json}"
    );
    let unread = share_event(
        12,
        &crate::test_share::Standing {
            look: None,
            ..standing
        },
    );
    assert!(unread.contains(r#""source":"recording grant""#), "{unread}");
    for (scope, test, slots) in [
        ("recording", None, Some(2_usize)),
        ("binary", Some("-p x --test t0"), None),
    ] {
        let line = core_grant_event(scope, test, (12, 4), "capacity ledger", slots, &machine);
        let json = line.strip_prefix("verify-event: ").expect("an event line");
        let crate::event_log::Observation::Known { kind, value } =
            crate::event_log::observation(json)
        else {
            panic!("{json} is no event its readers know");
        };
        assert_eq!(kind, crate::event_log::EventKind::TestCoreGrant);
        assert_eq!(value["event"].as_str(), Some("test-core-grant"));
        assert_eq!(value["scope"].as_str(), Some(scope));
        assert_eq!(value["test"].as_str(), test);
        assert_eq!(
            (value["asked"].as_u64(), value["granted"].as_u64()),
            (Some(12), Some(4))
        );
        assert_eq!(value["source"].as_str(), Some("capacity ledger"));
        assert_eq!(
            value["subprocess_slots"].as_u64(),
            slots.map(|n| n as u64),
            "{json}"
        );
        assert_eq!(
            (
                value["load1"].as_f64(),
                value["load5"].as_f64(),
                value["machine_cores"].as_u64()
            ),
            (Some(24.0), Some(24.0), Some(12))
        );
    }
}

/// A background dispatch retains cancellation authority even with no binary running,
/// without reserving a single byte or core. The ordinary control retains no such claim.
#[test]
fn a_background_dispatch_lifetime_holds_no_memory_or_cpu() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    SCRATCH.set(Some((
        aterm_capacity::Ledger::at(tmp.path().join("ledger")),
        Vec::new(),
    )));
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    ctx.background = true;
    let guard = dispatch_lifetime(&ctx, scratch_admit).unwrap().unwrap();
    assert_eq!((guard.record().need.mem, guard.record().need.cores), (0, 0));
    SCRATCH.with_borrow(|s| {
        assert_eq!(
            s.as_ref().unwrap().1,
            [(0, 0, aterm_capacity::Admit::Preemptible, false)]
        )
    });
    drop(guard);
    ctx.background = false;
    assert!(dispatch_lifetime(&ctx, scratch_admit).unwrap().is_none());
    SCRATCH.with_borrow(|s| {
        assert!(
            s.as_ref()
                .unwrap()
                .0
                .of_pid(std::process::id())
                .unwrap()
                .is_empty()
        )
    });
    SCRATCH.set(None);
}

#[test]
fn broken_test_home_preparation_refuses_before_test_dispatch() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let events = tmp.path().join("phases.events");
    let scratch = tmp.path().join("not-a-directory");
    std::fs::write(&scratch, b"retained").expect("block home preparation");
    let ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        scratch.clone(),
    )
    .with_events(Some(crate::event_log::EventLog::open(&events).unwrap()));
    let mut report = Report::new("pool refusal");
    run_tests(
        &ctx,
        &mut report,
        "fixture",
        &Cmd::new("must-not-be-dispatched"),
        crate::checks::Check::Test,
    );
    assert!(
        matches!(&report.entries[..], [crate::ladder::Entry::Ladder {
        outcome: crate::ladder::Outcome::Fail(crate::ladder::Severity::CouldNotRun), label, ..
    }] if label.starts_with("cannot prepare release test environment:"))
    );
    assert_eq!(
        std::fs::read(&scratch).expect("retained blocker"),
        b"retained"
    );
    let phases: Vec<aterm_json::Value> = std::fs::read_to_string(events)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("verify-event: "))
        .map(|line| aterm_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        phases.last().unwrap()["phase"].as_str(),
        Some("preparation")
    );
    assert_eq!(
        phases.last().unwrap()["scope_exit"].as_str(),
        Some("returned")
    );
    assert!(
        !phases
            .iter()
            .any(|event| event["phase"].as_str() == Some("recording"))
    );
}

/// Cargo itself resolves the effective target before the recorder is installed: the
/// environment, `CARGO_HOME`, command-local config and an explicit `--target` all refuse the
/// native dispatcher. Diagnostic stderr is not parsed as machine JSON, and no ordinary targo
/// command runs. NEGATIVE CONTROL: with none of those selections, Cargo's exact "not set"
/// answer is the native default.
#[cfg(unix)]
#[test]
fn every_effective_cargo_target_refuses_before_test_dispatch() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let targo = tmp.path().join("targo");
    let dispatched = tmp.path().join("dispatched");
    crate::standin_body(
        &targo,
        r#"case "$*" in
  *"config get build.target"*)
    echo 'targo: diagnostic belongs on stderr' >&2
    if [ -n "$CARGO_BUILD_TARGET" ]; then
      printf '{"build":{"target":"%s"}}\n' "$CARGO_BUILD_TARGET"
    elif [ -f "$CARGO_HOME/config.toml" ]; then
      printf '{"build":{"target":"home-target"}}\n'
    else
      case "$*" in
        *command-target*) printf '{"build":{"target":"command-target"}}\n' ;;
        *) echo 'error: config value `build.target` is not set' >&2; exit 101 ;;
      esac
    fi
    exit 0 ;;
esac
printf ran > "$TARGET_DISPATCHED"
exit 0"#,
    );
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir(&scratch).expect("verifier scratch");
    let ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        scratch,
    );
    let targets = |cmd: Cmd| {
        configured_build_targets(
            &ctx,
            &cmd.remove_env("CARGO_HOME")
                .remove_env("CARGO_BUILD_TARGET")
                .env("TARGET_DISPATCHED", dispatched.as_os_str()),
        )
    };

    assert_eq!(targets(Cmd::new(&targo)), Ok(None));
    assert_eq!(
        targets(Cmd::new(&targo).env("CARGO_BUILD_TARGET", "env-target")),
        Ok(Some(vec!["env-target".to_owned()]))
    );
    let cargo_home = tmp.path().join("cargo-home");
    std::fs::create_dir(&cargo_home).expect("CARGO_HOME");
    std::fs::write(
        cargo_home.join("config.toml"),
        "[build]\ntarget = \"home-target\"\n",
    )
    .expect("target config");
    assert_eq!(
        targets(Cmd::new(&targo).env("CARGO_HOME", &cargo_home)),
        Ok(Some(vec!["home-target".to_owned()]))
    );
    assert_eq!(
        targets(Cmd::new(&targo).args([
            "--unverified",
            "test",
            "--config",
            "build.target=\"command-target\"",
            "--tests",
        ])),
        Ok(Some(vec!["command-target".to_owned()]))
    );
    assert_eq!(
        targets(Cmd::new(&targo).args(["--unverified", "test", "--target=cli-target"])),
        Ok(Some(vec!["cli-target".to_owned()]))
    );
    assert!(
        !dispatched.exists(),
        "only the target query may run before refusal"
    );
}

/// Cargo announcing a binary without invoking the exact-host recorder refuses the run. It is
/// never retried through Cargo, because that path would execute an un-watched test binary.
/// NEGATIVE CONTROL: the stand-in exits successfully, so treating Cargo's status alone as a
/// pass would fail this test.
#[cfg(unix)]
#[test]
fn an_announced_binary_that_bypasses_the_recorder_is_not_run_again() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let targo = tmp.path().join("targo");
    crate::standin_body(
        &tmp.path().join("trustc"),
        "printf 'rustc 1.93.0 (trust)\\nbinary: rustc\\nhost: aarch64-apple-darwin\\nrelease: \
         1\\n'",
    );
    std::fs::write(
        &targo,
        "#!/bin/sh\ncase \"$*\" in\n  *config*get*build.target*)\n    echo 'error: \
         config value `build.target` is not set' >&2\n    exit 101 ;;\nesac\nprintf '     \
         Running unittests src/lib.rs \
         (target/debug/deps/aterm_gpu-1)\\n'\nexit 0\n",
    )
    .expect("the stand-in");
    std::fs::set_permissions(&targo, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().to_path_buf(),
    );
    ctx.tools.stage2_dir = tmp.path().to_path_buf();
    let mut report = Report::new("the test run");
    run_tests(
        &ctx,
        &mut report,
        "fixture",
        &Cmd::new(&targo),
        crate::checks::Check::Test,
    );
    let rows: Vec<_> = report
        .entries
        .iter()
        .filter_map(|entry| match entry {
            crate::ladder::Entry::Ladder { outcome, label, .. } => Some((*outcome, label.as_str())),
            _ => None,
        })
        .collect();
    assert!(
        matches!(rows.as_slice(), [(crate::ladder::Outcome::Fail(
            crate::ladder::Severity::CouldNotRun
        ), label)] if label.contains("recording runner left no record")),
        "{rows:?}"
    );
}

#[test]
fn subprocess_pool_cleanup_owns_only_its_new_directory() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let existing = tmp.path().join("existing");
    std::fs::create_dir(&existing).expect("owned existing fixture");
    init_subprocess_pool_dir(&existing, 2).expect("existing pool");
    let sentinel = existing.join("sentinel");
    std::fs::write(&sentinel, b"retained").expect("sentinel");
    assert!(init_subprocess_pool_dir(&existing, 2).is_err());
    let owned = subprocess_pool(tmp.path(), 2).expect("owned pool");
    let owned_path = owned.path().to_path_buf();
    assert_ne!(owned_path, existing);
    drop(owned);
    assert!(!owned_path.exists());
    assert_eq!(
        std::fs::read(&sentinel).expect("existing sentinel"),
        b"retained"
    );
    assert!(existing.join("slot-0000.lock").is_file());
    assert!(existing.join("slot-0001.lock").is_file());
}

#[test]
fn failed_binary_is_reported_while_another_binary_is_still_running() {
    let bins: Vec<_> = ["slow", "failed"]
        .into_iter()
        .map(|name| Binary {
            harness: false,
            header: name.into(),
            spec: name.into(),
            record: rec("/r/test", "test"),
        })
        .collect();
    let released = (Mutex::new(false), std::sync::Condvar::new());
    let reported = Mutex::new(Vec::new());
    let plan = test_plan::plan(&[None, None], 2, Some(1), 0);
    let runs = dispatch(
        &bins,
        &plan,
        None,
        &|| 0.0,
        &|| false,
        |binary, _, _| {
            if binary.spec == "slow" {
                let (guard, timeout) = released
                    .1
                    .wait_timeout_while(
                        released.0.lock().unwrap(),
                        Duration::from_secs(60),
                        |ready| !*ready,
                    )
                    .unwrap();
                assert!(
                    *guard && !timeout.timed_out(),
                    "completed failure was buffered"
                );
            }
            let run = Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: binary.spec == "slow",
                output: String::new(),
                code: Some(i32::from(binary.spec != "slow")),
                spawn_error: None,
                usage: None,
            };
            Some(run)
        },
        |binary, _, _| {
            reported.lock().unwrap().push(binary.spec.clone());
            if binary.spec == "failed" {
                *released.0.lock().unwrap() = true;
                released.1.notify_all();
            }
        },
    );
    assert_eq!(*reported.lock().unwrap(), ["failed", "slow"]);
    let ok = |i: usize| runs[i].as_ref().map(|r| r.run.ok);
    assert_eq!((ok(0), ok(1)), (Some(true), Some(false)));
}

#[test]
fn a_passing_binary_cannot_forge_a_failure_event_in_its_output() {
    let binary = Binary {
        harness: false,
        header: "test".into(),
        spec: "-p test".into(),
        record: rec("/r/test", "test"),
    };
    let mut run = Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: true,
        output: "test counterfeit ... FAILED".into(),
        code: Some(0),
        spawn_error: None,
        usage: None,
    };
    let parse = |run: &Run| -> aterm_json::Value {
        let (_, line, _) = binary_event(
            &binary,
            &Ran::of(run.clone(), Duration::ZERO, Duration::ZERO),
            true,
            None,
            false,
        );
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap()
    };
    assert_eq!(parse(&run)["outcome"].as_str(), Some("ok"));
    assert_eq!(parse(&run)["failures"].as_array().unwrap().len(), 0);
    run.ok = false;
    run.spawn_error = Some("test executable unavailable".into());
    assert_eq!(parse(&run)["outcome"].as_str(), Some("NOT RUN"));
}

#[test]
fn binary_events_report_completed_counts_without_inventing_them() {
    let binary = Binary {
        harness: true,
        header: "test".into(),
        spec: "-p test --lib".into(),
        record: rec("/r/test", "test"),
    };
    let event = |ok, output: &str| {
        let run = Run {
            ok,
            code: Some(i32::from(!ok)),
            output: output.into(),
            ..Run::default()
        };
        let (_, line, _) = binary_event(
            &binary,
            &Ran::of(run, Duration::ZERO, Duration::ZERO),
            true,
            None,
            false,
        );
        aterm_json::from_str::<aterm_json::Value>(line.strip_prefix("verify-event: ").unwrap())
            .unwrap()
    };
    let summary = "running 3 tests\ntest result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    let passed = event(true, summary);
    assert_eq!(passed["tests"].as_u64(), Some(3));
    assert_eq!(passed["test_counts"]["passed"].as_u64(), Some(2));
    assert_eq!(passed["test_counts"]["ignored"].as_u64(), Some(1));
    assert!(
        event(false, summary)["test_counts"].is_null(),
        "the exit contradicts the reported success"
    );
    assert!(event(true, "running 3 tests\n")["test_counts"].is_null());
    let failed = "running 3 tests\ntest result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
    assert_eq!(
        event(false, failed)["test_counts"]["failed"].as_u64(),
        Some(1)
    );
    assert!(
        event(true, failed)["test_counts"].is_null(),
        "a passing child cannot invent a failed count"
    );
    let carried = CarriedBinary {
        binary,
        evidence: Evidence {
            exe_sha256: Some("aa".repeat(32)),
            content_key: Some("11".repeat(32)),
            reason: None,
            parts: crate::test_content::KeyParts::new(),
        },
        json: false,
        prior_execution_s: 0.01,
        origin_run: None,
    };
    let changed = Evidence {
        exe_sha256: carried.evidence.exe_sha256.clone(),
        content_key: None,
        reason: Some("runtime inputs changed".into()),
        parts: crate::test_content::KeyParts::new(),
    };
    for line in [
        carried_event(&carried),
        unstable_carried_event(&carried, &changed),
    ] {
        let event: aterm_json::Value =
            aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
        assert_eq!(
            event.get("test_counts"),
            Some(&aterm_json::Value::Null),
            "a retained candidate ran no tests now"
        );
    }
}

/// A proven hang is a bounded structured FAIL, even when the sampler's error contains the
/// full-disk sentence that ordinarily makes a child NOT RUN. Its first-red decision invokes
/// the local halt once. NEGATIVE CONTROL: removing the structured hang restores the
/// environmental classification.
#[test]
fn a_hung_binary_is_a_bounded_named_gate_failure() {
    let binary = Binary {
        harness: true,
        header: "test".into(),
        spec: "-p test --lib".into(),
        record: rec("/r/test", "test"),
    };
    let running = (0..HUNG_CASES_SHOWN + 5)
        .map(|index| format!("case-{index}-{}", "x".repeat(1024)))
        .collect();
    let run = Run {
        case_observations: None,
        stopped_at: None,
        hung: Some(crate::exec::Hung {
            spec: binary.spec.clone(),
            binary: format!("/r/{}", "binary".repeat(200)),
            running,
            window: Duration::from_secs(180),
            sample: crate::exec::StackSample {
                report: Some(PathBuf::from("/private/hung.partial.sample.txt")),
                error: Some(format!(
                    "{} while sampling: {}",
                    crate::exec::ENOSPC_SENTENCE,
                    "detail".repeat(2_000)
                )),
            },
            json: true,
        }),
        ok: false,
        output: format!("{} (os error 28)\n", crate::exec::ENOSPC_SENTENCE),
        code: None,
        spawn_error: None,
        usage: None,
    };
    assert_eq!(run.environment_failure(), None);
    let all = combined(std::slice::from_ref(&binary), std::slice::from_ref(&run));
    assert!(all.hung.is_some() && all.environment_failure().is_none());
    let (outcome, line, _) = binary_event(
        &binary,
        &Ran::of(run.clone(), Duration::ZERO, Duration::ZERO),
        true,
        None,
        true,
    );
    assert_eq!(outcome, Outcome::Fail(Severity::GateFailed));
    assert!(
        line.len() < 64 * 1024,
        "one atomic event line: {} bytes",
        line.len()
    );
    let event: aterm_json::Value =
        aterm_json::from_str(line.strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["outcome"].as_str(), Some("FAIL"));
    assert_eq!(event["hung"].as_bool(), Some(true));
    assert_eq!(event["hang_window_s"].as_f64(), Some(180.0));
    assert_eq!(
        event["hung_cases"].as_array().unwrap().len(),
        HUNG_CASES_SHOWN
    );
    assert_eq!(event["hung_cases_omitted"].as_u64(), Some(5));
    assert_eq!(
        event["stack_sample"].as_str(),
        Some("/private/hung.partial.sample.txt")
    );
    assert!(
        event["stack_sample_error"]
            .as_str()
            .is_some_and(|error| error.contains(crate::exec::ENOSPC_SENTENCE))
    );
    let halts = AtomicUsize::new(0);
    assert!(mark_red_decided(
        outcome,
        true,
        Some(&|| {
            halts.fetch_add(1, Ordering::SeqCst);
        })
    ));
    assert_eq!(halts.load(Ordering::SeqCst), 1);

    let mut control = run;
    control.hung = None;
    assert!(control.environment_failure().is_some());
}

fn rec(exe: &str, pkg: &str) -> Record {
    Record {
        cwd: PathBuf::from(format!("/r/crates/{pkg}")),
        argv: vec![exe.into(), "--skip".into(), "slow::".into()],
        env: vec![
            ("CARGO_PKG_NAME".into(), pkg.into()),
            ("RUST_TEST_THREADS".into(), "14".into()),
            ("CARGO".into(), "/store/bin/targo".into()),
        ],
    }
}

fn artifact(exe: &str, kind: &str, name: &str, test: bool) -> String {
    format!(
        "{{\"reason\":\"compiler-artifact\",\"package_id\":\"path+file:///r/crates/x#0.1.0\",\
         \"target\":{{\"kind\":[\"{kind}\"],\"crate_types\":[\"bin\"],\"name\":\"{name}\",\
         \"src_path\":\"/r/crates/x/src/lib.rs\",\"test\":true}},\
         \"profile\":{{\"opt_level\":\"0\",\"debuginfo\":2,\"test\":{test}}},\
         \"features\":[],\"filenames\":[\"{exe}\"],\"executable\":\"{exe}\",\"fresh\":true}}\n"
    )
}

/// A resume carries a binary only when the prior ok names both its current executable and
/// complete bounded content key, and never GOLDEN's package. Evidence is computed for every
/// binary, including the first run and binaries absent from the prior proof. Negative
/// controls: a changed content key and GOLDEN's exact match both run.
#[test]
fn a_resume_carries_only_unchanged_binaries_that_passed_and_never_the_golden_package() {
    let bin = |exe: &str, pkg: &str| Binary {
        harness: false,
        header: format!("     Running unittests src/lib.rs ({exe})"),
        spec: format!("-p {pkg} --lib"),
        record: rec(exe, pkg),
    };
    let bins = vec![
        bin("/t/deps/aterm_db-1", "aterm-db"),
        bin("/t/deps/aterm_grid-2", "aterm-grid"),
        bin("/t/deps/aterm_gpu-3", crate::checks::GOLDEN.0),
        bin("/t/deps/aterm_core-4", "aterm-core"),
    ];
    let evidence = |b: &Binary| Evidence {
        exe_sha256: Some(match b.record.exe().to_str() {
            Some("/t/deps/aterm_db-1") => "aa".repeat(32),
            Some("/t/deps/aterm_grid-2") => "bb".repeat(32),
            Some("/t/deps/aterm_gpu-3") => "cc".repeat(32),
            _ => "dd".repeat(32),
        }),
        content_key: Some(match b.record.exe().to_str() {
            Some("/t/deps/aterm_db-1") => "11".repeat(32),
            Some("/t/deps/aterm_grid-2") => "22".repeat(32),
            Some("/t/deps/aterm_gpu-3") => "33".repeat(32),
            _ => "44".repeat(32),
        }),
        reason: None,
        parts: [("env:MAKEFLAGS".to_owned(), "current".to_owned())].into(),
    };
    let mut passed = Passed::new();
    for (index, binary) in bins.iter().take(3).enumerate() {
        let mut prior = CarryProof::passed(&binary.spec, &evidence(binary), 1.25 + index as f64);
        if index == 1 {
            prior.content_key = Some("ee".repeat(32));
            prior
                .key_parts
                .insert("env:MAKEFLAGS".into(), "prior".into());
        }
        passed.insert(binary.spec.clone(), prior);
    }
    let mut failed = CarryProof::passed(&bins[3].spec, &evidence(&bins[3]), 2.0);
    failed.outcome = "FAIL".into();
    passed.insert(bins[3].spec.clone(), failed);
    let evidenced = Mutex::new(Vec::new());
    let Carried {
        run,
        carried,
        known,
        refused,
    } = carry(
        &passed,
        bins,
        |b, _| {
            evidenced.lock().unwrap().push(b.spec.clone());
            evidence(b)
        },
        2,
    );
    assert_eq!(
        refused,
        [
            (
                "-p aterm-grid --lib".to_owned(),
                "content key moved in env:MAKEFLAGS".to_owned()
            ),
            (
                format!("-p {} --lib", crate::checks::GOLDEN.0),
                format!("{} always runs", crate::checks::GOLDEN.0)
            ),
        ],
        "each offered ok proof that runs again says what differs; a prior FAIL was no offer"
    );
    assert_eq!(
        refused_lines(&refused),
        [
            format!(
                "  offered and run again: 1 ({0} always runs), as -p {0} --lib",
                crate::checks::GOLDEN.0
            ),
            "  offered and run again: 1 (content key moved in env:MAKEFLAGS), as -p aterm-grid --lib"
                .to_owned(),
        ]
    );
    let specs = |v: &[Binary]| v.iter().map(|b| b.spec.clone()).collect::<Vec<_>>();
    assert_eq!(
        carried
            .iter()
            .map(|carried| carried.binary.spec.as_str())
            .collect::<Vec<_>>(),
        ["-p aterm-db --lib"],
        "only the exact bounded prior ok"
    );
    assert_eq!(
        specs(&run),
        [
            "-p aterm-grid --lib",
            "-p aterm-gpu --lib",
            "-p aterm-core --lib"
        ],
        "a changed key runs, GOLDEN's package always runs, and a direct prior FAIL runs"
    );
    let mut evidenced = evidenced.into_inner().unwrap();
    evidenced.sort();
    assert_eq!(
        evidenced,
        [
            "-p aterm-core --lib",
            "-p aterm-db --lib",
            "-p aterm-gpu --lib",
            "-p aterm-grid --lib",
        ],
        "every release binary gets evidence on its first run too"
    );
    assert_eq!(
        known
            .get("-p aterm-grid --lib")
            .and_then(|evidence| evidence.content_key.as_deref()),
        Some("22".repeat(32).as_str())
    );
    let event: aterm_json::Value = aterm_json::from_str(
        carried_event(&carried[0])
            .strip_prefix("verify-event: ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(event["outcome"].as_str(), Some("ok"));
    assert_eq!(event["carried"].as_bool(), Some(true));
    assert_eq!(event["execution_s"].as_f64(), Some(0.0));
    assert_eq!(event["recorded_execution_s"].as_f64(), Some(1.25));
    assert_eq!(event["exe_sha256"].as_str(), Some("aa".repeat(32).as_str()));
    assert_eq!(
        event["content_key"].as_str(),
        Some("11".repeat(32).as_str())
    );
}

/// Deterministic release-line timeline for A19. A cfg(test) edit changes the edited target
/// and one dependent executable; a fixture changes only its declared reader; a harness or
/// verifier context change changes every complete key. The fixture scheduler has one
/// execution slot, so the sum is its modeled wall, not a production latency claim. NEGATIVE
/// CONTROL: an otherwise equal undeclared reader still runs.
#[test]
fn fix_hop_line_timeline_runs_only_changed_complete_inputs() {
    let binary = |name: &str| Binary {
        harness: false,
        header: String::new(),
        spec: format!("-p {name} --lib"),
        record: rec(&format!("/t/deps/{name}-0000000000000000"), name),
    };
    let bins = vec![
        binary("edited"),
        binary("dependent"),
        binary("fixture-reader"),
        binary("fixture-sibling"),
    ];
    let mut base = BTreeMap::new();
    for (index, binary) in bins.iter().enumerate() {
        base.insert(
            binary.spec.clone(),
            Evidence {
                exe_sha256: Some(format!("{:02x}", index + 1).repeat(32)),
                content_key: Some(format!("{:02x}", index + 17).repeat(32)),
                reason: None,
                parts: crate::test_content::KeyParts::new(),
            },
        );
    }
    let mut prior = Passed::new();
    for binary in &bins {
        prior.insert(
            binary.spec.clone(),
            CarryProof::passed(&binary.spec, &base[&binary.spec], 1.0),
        );
    }
    let walls = BTreeMap::from([
        ("-p edited --lib", 12_u64),
        ("-p dependent --lib", 8),
        ("-p fixture-reader --lib", 4),
        ("-p fixture-sibling --lib", 2),
    ]);
    let scenario = |evidence: BTreeMap<String, Evidence>| {
        let result = carry(
            &prior,
            bins.clone(),
            |binary, _| evidence[&binary.spec].clone(),
            2,
        );
        let run = result
            .run
            .iter()
            .map(|binary| binary.spec.clone())
            .collect::<Vec<_>>();
        let carried = result
            .carried
            .iter()
            .map(|binary| binary.binary.spec.clone())
            .collect::<Vec<_>>();
        let single_slot_wall_s = run.iter().map(|spec| walls[spec.as_str()]).sum::<u64>();
        (run, carried, single_slot_wall_s)
    };

    let mut cfg_test = base.clone();
    for (spec, byte) in [("-p edited --lib", "a1"), ("-p dependent --lib", "a2")] {
        let evidence = cfg_test.get_mut(spec).unwrap();
        evidence.exe_sha256 = Some(byte.repeat(32));
        evidence.content_key = Some(byte.repeat(32));
    }
    let (run, carried, wall) = scenario(cfg_test);
    assert_eq!(run, ["-p edited --lib", "-p dependent --lib"]);
    assert_eq!(
        carried,
        ["-p fixture-reader --lib", "-p fixture-sibling --lib"]
    );
    assert_eq!(wall, 20);

    let mut fixture = base.clone();
    fixture
        .get_mut("-p fixture-reader --lib")
        .unwrap()
        .content_key = Some("b1".repeat(32));
    let (run, carried, wall) = scenario(fixture);
    assert_eq!(run, ["-p fixture-reader --lib"]);
    assert_eq!(
        carried,
        [
            "-p edited --lib",
            "-p dependent --lib",
            "-p fixture-sibling --lib"
        ]
    );
    assert_eq!(wall, 4);

    let harness = base
        .iter()
        .map(|(spec, evidence)| {
            let mut evidence = evidence.clone();
            evidence.content_key = Some("c1".repeat(32));
            (spec.clone(), evidence)
        })
        .collect();
    let (run, carried, wall) = scenario(harness);
    assert_eq!(run.len(), 4);
    assert!(carried.is_empty());
    assert_eq!(wall, 26);

    let mut unbounded = base.clone();
    unbounded.insert(
        "-p fixture-sibling --lib".into(),
        Evidence {
            exe_sha256: base["-p fixture-sibling --lib"].exe_sha256.clone(),
            content_key: None,
            reason: Some("runtime inputs are undeclared".into()),
            parts: crate::test_content::KeyParts::new(),
        },
    );
    let (run, _, _) = scenario(unbounded);
    assert_eq!(run, ["-p fixture-sibling --lib"]);
}

/// A test binary's event lands in a release's events file whole, as its cutter reads it, beside
/// stderr. Negative control: an `emit` that writes stderr alone leaves the file empty.
#[test]
fn a_binarys_event_reaches_the_release_events_file_whole() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let at = dir.path().join("06-verify.events");
    let ctx = crate::Ctx::new(
        dir.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().to_path_buf(),
    )
    .with_events(Some(crate::event_log::EventLog::open(&at).unwrap()));
    let binary = Binary {
        harness: false,
        header: String::new(),
        spec: "-p aterm-db --lib".into(),
        record: Record {
            cwd: dir.path().to_path_buf(),
            argv: vec!["/t/deps/aterm_db-1".into()],
            env: Vec::new(),
        },
    };
    let evidence = Evidence {
        exe_sha256: Some("aa".repeat(32)),
        content_key: Some("11".repeat(32)),
        reason: None,
        parts: crate::test_content::KeyParts::new(),
    };
    let event = carried_event(&CarriedBinary {
        binary,
        evidence,
        json: false,
        prior_execution_s: 1.0,
        origin_run: None,
    });
    let initial = std::fs::read_to_string(&at).unwrap();
    let prepared = ctx.events.as_ref().unwrap().prepare(&event);
    emit(&ctx, &event);
    assert_eq!(
        std::fs::read_to_string(&at).unwrap(),
        format!("{initial}{prepared}")
    );
}

/// A RECORD IS THE PROCESS, BYTE FOR BYTE: every argument and variable
/// survives, spaces, `=` and newlines included, and a byte stream cut
/// mid-token or missing its header is refused rather than read short.
#[test]
fn a_record_round_trips_and_a_torn_one_is_refused() {
    let r = Record {
        cwd: PathBuf::from("/a dir/with space"),
        argv: vec!["/t/x-1".into(), "--skip".into(), "a b\nc".into()],
        env: vec![
            ("K".into(), "v=w".into()),
            ("EMPTY".into(), "".into()),
            ("NL".into(), "one\ntwo".into()),
        ],
    };
    let bytes = r.encode();
    assert_eq!(Record::decode(&bytes), Some(r.clone()));
    assert_eq!(r.var("NL"), Some(OsStr::new("one\ntwo")));
    let inherited = Record {
        env: vec![
            ("K".into(), "first".into()),
            ("Path".into(), "a".into()),
            ("K".into(), "last".into()),
        ],
        ..r.clone()
    };
    assert_eq!(inherited.var("Path"), Some(OsStr::new("a")));
    assert_eq!(
        inherited.var("K"),
        Some(OsStr::new("last")),
        "the last setting wins"
    );
    assert_eq!(
        inherited.var("PATH"),
        cfg!(windows).then_some(OsStr::new("a")),
        "a name in another case is the same variable on Windows alone"
    );
    assert_eq!(Record::decode(&bytes[..bytes.len() - 1]), None, "cut short");
    assert_eq!(
        Record::decode(&bytes[..bytes.len() - 3]),
        None,
        "cut mid-token"
    );
    assert_eq!(Record::decode(b"something else\0"), None);
}

/// THE RECORDER, THROUGH ITS OWN ENTRY POINT: each call takes the next
/// number, the file is the owner's alone, and what comes back is this
/// process — its directory and its environment — with the argv it was given.
#[test]
fn the_recorder_writes_the_next_owner_only_record() {
    let dir = crate::mktemp_dir("atv-rec").expect("mktemp");
    let d = dir.clone().into_os_string();
    assert_eq!(
        record_main(&[d.clone(), "/t/a".into(), "--skip".into(), "m::".into()]),
        0
    );
    assert_eq!(record_main(&[d.clone(), "/t/b".into()]), 0);
    assert_eq!(record_main(&[d]), 2, "no test binary is a usage error");
    let recs = read_records(&dir).expect("records");
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].argv, ["/t/a", "--skip", "m::"]);
    assert_eq!(recs[1].argv, ["/t/b"]);
    assert_eq!(recs[0].cwd, std::env::current_dir().expect("cwd"));
    assert_eq!(recs[0].var("PATH"), std::env::var_os("PATH").as_deref());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.join("000000.rec"))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the environment is the owner's alone: {mode:o}"
        );
    }
    let unwritable = dir.join("absent/deeper").into_os_string();
    assert_eq!(record_main(&[unwritable, "/t/c".into()]), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_runner_config_is_a_toml_array_so_no_path_splits() {
    assert_eq!(
        runner_config(
            Path::new("/a b/aterm-verify"),
            Path::new("/tmp/q\"d"),
            "aarch64-apple-darwin",
        )
        .as_deref(),
        Some(
            "target.aarch64-apple-darwin.runner=[\"/a b/aterm-verify\",\"--record-test-binary\",\
             \"/tmp/q\\\"d\"]"
        )
    );
    assert_eq!(
        runner_config(Path::new("/gate"), Path::new("/tmp/r"), "bad target"),
        None,
        "an invalid target cannot broaden the runner to another target"
    );
    let serial = Cmd::new("targo").args([
        "--config",
        "CACHE",
        "--unverified",
        "test",
        "--tests",
        "--",
        "--skip",
        "x",
    ]);
    assert_eq!(
        recording_cmd(&serial, "CFG", "aarch64-apple-darwin").argv(),
        [
            "targo",
            "--config",
            "CFG",
            "--config",
            "CACHE",
            "--unverified",
            "test",
            "--tests",
            "--message-format=json-render-diagnostics",
            "--",
            "--skip",
            "x"
        ],
        "the runner before the subcommand, beside the build cache's --config (one after \
         it would replace the cache's); libtest's arguments untouched"
    );
    let removed = &recording_cmd(&serial, "CFG", "aarch64-apple-darwin").remove_envs;
    assert!(removed.contains(&OsString::from("CARGO_BUILD_TARGET")));
    assert!(removed.contains(&OsString::from("CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER")));
}

#[test]
fn recording_keeps_the_runner_with_command_local_workspace_features() {
    let serial = Cmd::new("targo").args([
        "--unverified",
        "test",
        "--config",
        "resolver.feature-unification=\"workspace\"",
        "--tests",
        "--",
        "--skip",
        "x",
    ]);
    let command = recording_cmd(&serial, "RUNNER", "aarch64-apple-darwin");
    let args = command.argv();
    assert_eq!(
        &args[1..7],
        [
            "--unverified",
            "test",
            "--config",
            "RUNNER",
            "--config",
            "resolver.feature-unification=\"workspace\""
        ]
    );
    assert!(args.ends_with(&["--".to_owned(), "--skip".to_owned(), "x".to_owned()]));
}

/// THE COMPILE'S LIST, AND THE LOG WITHOUT IT: only test-mode executables
/// count, each with cargo's own flag, and every JSON line leaves the text
/// the ladder shows — the diagnostics and progress lines stay, in order.
#[test]
fn the_compile_names_its_test_executables_and_keeps_its_diagnostics() {
    let mut log = format!(
        "   Compiling x v0.1.0\n{}{}{}{}warning: unused\n{}{{\"reason\":\"build-finished\",\
         \"success\":true}}\n    Finished `test` profile\n",
        artifact("/r/target/debug/deps/x-1", "lib", "x", true),
        artifact("/r/target/debug/x", "bin", "x", false),
        artifact("/r/target/debug/deps/x-2", "bin", "x", true),
        artifact("/r/target/debug/deps/it-3", "test", "it", true),
        artifact("/r/target/debug/deps/ex-4", "example", "ex", true),
    );
    let exes = take_test_executables(&mut log).expect("readable");
    assert_eq!(
        exes.iter()
            .map(|e| (e.exe.to_str().unwrap(), e.flag.as_str()))
            .collect::<Vec<_>>(),
        [
            ("/r/target/debug/deps/x-1", "--lib"),
            ("/r/target/debug/deps/x-2", "--bin x"),
            ("/r/target/debug/deps/it-3", "--test it"),
            ("/r/target/debug/deps/ex-4", "--example ex"),
        ]
    );
    assert_eq!(
        log,
        "   Compiling x v0.1.0\nwarning: unused\n    Finished `test` profile\n"
    );
    let mut torn = "{\"reason\":\"compiler-artifact\",\"target\":\n".to_string();
    assert!(
        take_test_executables(&mut torn).is_err(),
        "a torn message is no list"
    );
}

#[test]
fn mixed_type_target_kinds_do_not_select_test_executables() {
    let valid = artifact("/built/x", "lib", "x", true);
    for kinds in [r#"["lib"]"#, r#"["rlib","lib"]"#] {
        let mut positive = valid.replace(r#"["lib"]"#, kinds);
        assert_eq!(take_test_executables(&mut positive).unwrap().len(), 1);
    }
    for kinds in [r#"["lib",7]"#, r#"[null,"lib"]"#, r#"["lib",{}]"#] {
        let malformed = valid.replace(r#"["lib"]"#, kinds);
        for mut wire in [
            malformed.clone(),
            format!("{valid}{malformed}"),
            format!("{malformed}{valid}"),
        ] {
            assert!(take_test_executables(&mut wire).is_err(), "{kinds}");
        }
    }
}

#[test]
fn ambiguous_compiler_artifacts_do_not_select_test_executables() {
    let valid = artifact("/built/x", "lib", "x", true);
    for mut wire in [
        valid.clone(),
        format!("  {valid}"),
        valid.replacen("\"reason\"", r#""\u0072eason""#, 1),
        valid.replacen(
            '{',
            r#"{"extra":[1,-1,1.5,true,false,null,{"text":"ok"}],"#,
            1,
        ),
    ] {
        assert_eq!(take_test_executables(&mut wire).unwrap().len(), 1);
        assert!(wire.is_empty());
    }
    let mut diagnostic = "human diagnostic\n{\"human\":1}\n{\"reason\":\"compiler-message\",\"message\":{}}\n{\"reason\":\"build-finished\",\"success\":true}\n".to_owned();
    assert!(take_test_executables(&mut diagnostic).unwrap().is_empty());
    assert_eq!(diagnostic, "human diagnostic\n{\"human\":1}\n");
    for message in include_str!("testdata/ambiguous-compiler-artifacts.jsonl").lines() {
        for before in [true, false] {
            let valid = artifact("/built/x", "lib", "x", true);
            let wire = if before {
                format!("{valid}{message}\n")
            } else {
                format!("{message}\n{valid}")
            };
            let mut output = format!("human diagnostic\n{wire}");
            assert!(take_test_executables(&mut output).is_err(), "{message}");
            assert_eq!(output, "human diagnostic\n");
        }
    }
}

/// THE PLAN ADDS UP OR IT IS NOT A PLAN: every record meets the header cargo
/// printed for it and the compile's flag for its executable, the re-run spec
/// is cargo's.
#[test]
fn records_headers_and_the_compile_agree_or_the_plan_is_refused() {
    let log = "    Finished `test` profile\n\
               \x20    Running unittests src/lib.rs (target/debug/deps/aterm_link-1)\n\
               \x20    Running tests/bridge_e2e.rs (target/debug/deps/bridge_e2e-2)\n";
    let exes = [
        TestExe {
            harness: false,
            exe: "/r/target/debug/deps/aterm_link-1".into(),
            flag: "--lib".into(),
            bin_env: Vec::new(),
        },
        TestExe {
            harness: false,
            exe: "/r/target/debug/deps/bridge_e2e-2".into(),
            flag: "--test bridge_e2e".into(),
            bin_env: Vec::new(),
        },
    ];
    let recs = || {
        vec![
            rec("/r/target/debug/deps/aterm_link-1", "aterm-link"),
            rec("/r/target/debug/deps/bridge_e2e-2", "aterm-link"),
        ]
    };
    let bins = plan(recs(), log, &exes).expect("a plan");
    assert_eq!(
        bins.iter().map(|b| b.spec.as_str()).collect::<Vec<_>>(),
        ["-p aterm-link --lib", "-p aterm-link --test bridge_e2e"]
    );
    let in_build_dir: Vec<TestExe> = exes
        .iter()
        .map(|e| TestExe {
            harness: false,
            exe: Path::new("/cache/r/.cargo-build/debug/deps")
                .join(e.exe.file_name().expect("a file name")),
            flag: e.flag.clone(),
            bin_env: Vec::new(),
        })
        .collect();
    assert_eq!(plan(recs(), log, &in_build_dir).expect("a plan"), bins);
    assert_eq!(
        bins[1].header,
        "     Running tests/bridge_e2e.rs (target/debug/deps/bridge_e2e-2)"
    );

    let mut one_short = recs();
    one_short.pop();
    assert!(
        plan(one_short, log, &exes)
            .unwrap_err()
            .contains("recorder saw 1")
    );
    let mut swapped = recs();
    swapped.swap(0, 1);
    assert!(
        plan(swapped, log, &exes)
            .unwrap_err()
            .contains("where the recorder saw")
    );
    assert!(
        plan(recs(), log, &exes[..1])
            .unwrap_err()
            .contains("did not report")
    );
    let mut nameless = recs();
    nameless[0].env.retain(|(k, _)| k != "CARGO_PKG_NAME");
    assert!(
        plan(nameless, log, &exes)
            .unwrap_err()
            .contains("CARGO_PKG_NAME")
    );
}

/// THE DISPATCH: a binary starts only while its charge fits in what the running ones leave of
/// the share, at its planned width, every binary once, and the answers in declared order
/// whatever order they ended in. Each of the first three waits until all three run (3 + 2 + 1
/// cores of 6), so the three ran at once. Every later binary charges at least two, so no four
/// fit. NEGATIVE CONTROL: had the fourth (3 more) not waited for room, the share would have
/// been passed at its start.
#[test]
fn binaries_run_while_their_charges_fit_and_answer_in_declared_order() {
    let charges = [3, 2, 1, 3, 2, 3, 2, 3, 2];
    let bins: Vec<Binary> = (0..charges.len())
        .map(|i| Binary {
            harness: false,
            header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
            spec: format!("-p x --test t{i}"),
            record: rec(&format!("/r/t{i}"), "x"),
        })
        .collect();
    let plan = Plan {
        share: 6,
        target: Duration::ZERO,
        order: charges
            .iter()
            .enumerate()
            .map(|(index, &charge)| Planned {
                index,
                width: charge,
                charge,
                peak: 0,
                predicted: Some(Duration::from_secs(1)),
            })
            .collect(),
    };
    let held = Mutex::new((0_u32, 0_u32));
    let started = (Mutex::new(0_usize), Condvar::new());
    let ran = dispatch(
        &bins,
        &plan,
        None,
        &|| 0.0,
        &|| false,
        |b, planned, _| {
            {
                let mut held = held.lock().unwrap();
                held.0 += planned.charge;
                held.1 = held.1.max(held.0);
                assert!(held.0 <= 6, "{} started past the share", b.spec);
            }
            *started.0.lock().unwrap() += 1;
            started.1.notify_all();
            if planned.index < 3 {
                let (count, waited) = started
                    .1
                    .wait_timeout_while(
                        started.0.lock().unwrap(),
                        aterm_spec::wait::MUST_HAPPEN,
                        |n| *n < 3,
                    )
                    .unwrap();
                assert!(
                    *count >= 3 && !waited.timed_out(),
                    "the first three never ran together"
                );
            }
            held.lock().unwrap().0 -= planned.charge;
            let run = Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: true,
                output: format!("{}\nran {} at {}\n", b.header, b.spec, planned.width),
                code: Some(0),
                spawn_error: None,
                usage: None,
            };
            Some(run)
        },
        |_, _, _| {},
    );
    assert_eq!(held.lock().unwrap().1, 6, "the first three ran at once");
    let said: Vec<String> = ran
        .iter()
        .map(|r| {
            r.as_ref()
                .unwrap()
                .run
                .output
                .lines()
                .nth(1)
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        said,
        (0..charges.len())
            .map(|i| format!("ran -p x --test t{i} at {}", charges[i]))
            .collect::<Vec<_>>(),
        "declared order, each at its width"
    );
    assert_eq!(most_at_once(&ran), 3, "the first three ran at once");
}

/// A RUN THAT PANICS (a gate defect) gives its binary's charge back and halts the dispatch:
/// the other worker, waiting for the share, starts nothing more, and the panic reaches the
/// caller instead of that worker waiting forever. NEGATIVE CONTROL: with no panic, every
/// binary runs, one at a time on two cores.
#[test]
fn a_run_that_panics_gives_its_charge_back_and_halts_the_dispatch() {
    let bins: Vec<Binary> = (0..3)
        .map(|i| Binary {
            harness: false,
            header: format!("t{i}"),
            spec: format!("t{i}"),
            record: rec(&format!("/r/t{i}"), "x"),
        })
        .collect();
    // Three binaries two wide on two cores: one at a time, on two workers.
    let plan = test_plan::plan(&[None, None, None], 2, None, 0);
    let go = |panics: bool| {
        let (bins, plan) = (bins.clone(), plan.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                dispatch(
                    &bins,
                    &plan,
                    None,
                    &|| 0.0,
                    &|| false,
                    |b, _, _| {
                        assert!(!(panics && b.spec == "t0"), "a gate defect");
                        Some(Run {
                            case_observations: None,
                            stopped_at: None,
                            hung: None,
                            ok: true,
                            output: String::new(),
                            code: Some(0),
                            spawn_error: None,
                            usage: None,
                        })
                    },
                    |_, _, _| {},
                )
                .iter()
                .flatten()
                .count()
            }));
            tx.send(ran.ok()).ok();
        });
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the dispatch ended")
    };
    assert_eq!(go(false), Some(3));
    assert_eq!(go(true), None, "the panic reached the caller");
}

/// The CPU account counts a binary the run left unstarted apart: it used no CPU, so it is
/// no lost sample. A started binary whose clock booked nothing is still unread, and keeps the
/// account unreconciled. NEGATIVE CONTROL: the old loop counted every run with no reading,
/// unstarted ones included, as unread, so a halted run with every started binary read said
/// `binary_unread: 1`.
#[test]
fn only_a_started_binary_with_no_reading_is_an_unread_sample() {
    let ran = |started: bool, cpu: Option<u64>| {
        let mut ran = Ran::of(Run::default(), Duration::ZERO, Duration::ZERO);
        ran.started = started;
        ran.run.usage = cpu.map(|cpu| exec::TreeUsage {
            cpu: Duration::from_secs(cpu),
            ..exec::TreeUsage::default()
        });
        ran
    };
    let halted = [ran(true, Some(2)), ran(false, None), ran(false, None)];
    assert_eq!(
        BinaryCpu::of(&halted),
        BinaryCpu {
            ns: 2_000_000_000,
            reads: 1,
            unread: 0,
            unstarted: 2,
        }
    );
    assert_eq!(BinaryCpu::of(&halted).observed(), Some(2_000_000_000));
    let lost = [ran(true, Some(2)), ran(true, None), ran(false, None)];
    assert_eq!(
        BinaryCpu::of(&lost),
        BinaryCpu {
            ns: 2_000_000_000,
            reads: 1,
            unread: 1,
            unstarted: 1,
        }
    );
    // Nothing read and something unread is no figure; nothing started is zero.
    assert_eq!(BinaryCpu::of(&[ran(true, None)]).observed(), None);
    assert_eq!(BinaryCpu::of(&[ran(false, None)]).observed(), Some(0));
}

/// THE RUN'S READINGS: the most binaries that ran at once are the most whose spans overlap,
/// one ending as the next starts counted apart, and binaries left unstarted (no execution)
/// not at all; a binary whose CPU over its wall passed its charge is named with the cores it
/// used. NEGATIVE CONTROLS: a third binary starting before the second ends makes three at
/// once, and a binary within its charge is not named.
#[test]
fn the_most_at_once_and_the_overruns_are_read_from_the_runs() {
    let s = Duration::from_secs;
    let ran = |charge: u32, waiting: u64, execution: u64, cpu: u64| {
        let mut ran = Ran::of(
            Run {
                case_observations: None,
                stopped_at: None,
                hung: None,
                ok: true,
                output: String::new(),
                code: Some(0),
                spawn_error: None,
                usage: None,
            },
            s(waiting),
            s(execution),
        );
        ran.planned.charge = charge;
        ran.run.usage = Some(exec::TreeUsage {
            cpu: s(cpu),
            ..exec::TreeUsage::default()
        });
        Some(ran)
    };
    let apart = [ran(1, 0, 2, 2), ran(2, 1, 2, 12), ran(1, 3, 1, 1)];
    assert_eq!(most_at_once(&apart), 2);
    let overlapping = [ran(1, 0, 2, 2), ran(2, 1, 2, 12), ran(1, 1, 2, 1)];
    assert_eq!(most_at_once(&overlapping), 3);
    // Three a halt left unstarted, answered at the dispatch's end with no execution: none ran.
    let halted = [
        ran(1, 0, 2, 2),
        ran(1, 2, 0, 0),
        ran(1, 2, 0, 0),
        ran(1, 2, 0, 0),
    ];
    assert_eq!(most_at_once(&halted), 1);
    let bins: Vec<Binary> = (0..3)
        .map(|i| Binary {
            harness: false,
            header: format!("t{i}"),
            spec: format!("-p x --test t{i}"),
            record: rec(&format!("/r/t{i}"), "x"),
        })
        .collect();
    let over = overruns(&bins, &apart);
    assert!(
        over.contains("-p x --test t1 used 6.0 cores of 2 charged"),
        "{over}"
    );
    assert!(!over.contains("t0") && !over.contains("t2"), "{over}");
    assert_eq!(overruns(&bins[..1], &apart[..1]), "");
}

/// A RUN'S CEILING SPENT halts the dispatch: nothing more starts, and what it left is
/// unanswered (the run reports it as never started), as is one whose run answered that it did
/// not start. NEGATIVE CONTROL: unspent, every binary runs.
#[test]
fn a_spent_ceiling_starts_nothing_more() {
    let bins: Vec<Binary> = (0..4)
        .map(|i| Binary {
            harness: false,
            header: format!("t{i}"),
            spec: format!("-p x --test t{i}"),
            record: rec(&format!("/r/t{i}"), "x"),
        })
        .collect();
    let plan = test_plan::plan(&[None, None, None, None], 1, None, 0);
    let done = AtomicUsize::new(0);
    let ok = || {
        Some(Run {
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: true,
            output: String::new(),
            code: Some(0),
            spawn_error: None,
            usage: None,
        })
    };
    let spent = || done.load(Ordering::SeqCst) >= 2;
    let ran = dispatch(
        &bins,
        &plan,
        None,
        &|| 0.0,
        &spent,
        |_, _, _| ok(),
        |_, _, _| {
            done.fetch_add(1, Ordering::SeqCst);
        },
    );
    assert_eq!(
        ran.iter().map(Option::is_some).collect::<Vec<_>>(),
        [true, true, false, false]
    );
    let late = dispatch(
        &bins,
        &plan,
        None,
        &|| 0.0,
        &|| false,
        |_, p, _| if p.index == 1 { None } else { ok() },
        |_, _, _| {},
    );
    assert_eq!(
        late.iter().map(Option::is_some).collect::<Vec<_>>(),
        [true, false, true, true],
        "one its ceiling met as its turn came is answered as unstarted, and the rest go on"
    );
    let all = dispatch(
        &bins,
        &plan,
        None,
        &|| 0.0,
        &|| false,
        |_, _, _| ok(),
        |_, _, _| {},
    );
    assert!(all.iter().all(Option::is_some));
}

/// Binaries of `charges`, all unseen, each its charge wide, in a plan on `share` cores.
#[cfg(unix)]
fn charged(charges: &[u32], share: u32) -> (Vec<Binary>, Plan) {
    let bins = (0..charges.len())
        .map(|i| Binary {
            harness: false,
            header: format!("t{i}"),
            spec: format!("-p x --test t{i}"),
            record: rec(&format!("/r/t{i}"), "x"),
        })
        .collect();
    let order = charges
        .iter()
        .enumerate()
        .map(|(index, &charge)| Planned {
            index,
            width: charge,
            charge,
            peak: 0,
            predicted: None,
        })
        .collect();
    let plan = Plan {
        share,
        target: Duration::ZERO,
        order,
    };
    (bins, plan)
}

#[test]
fn a_smaller_execution_grant_leaves_local_room_for_a_fitting_binary() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let ledger = aterm_capacity::Ledger::at(tmp.path().join("ledger"));
    let machine = aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, 1));
    let claim = aterm_capacity::admit::try_admit_with(
        &ledger,
        &machine,
        aterm_capacity::Need::new("test execution grant")
            .cores(4)
            .mem(8),
    )
    .unwrap()
    .unwrap();
    assert_eq!(claim.cores(), 1);
    let permit = Ok(BinaryPermit {
        claim: Some(Arc::new(claim)),
        tokens: None,
    });
    let (_, mut plan) = charged(&[4, 3, 1], 4);
    for (binary, peak) in plan.order.iter_mut().zip([8, 4, 1]) {
        binary.peak = peak;
    }
    let state = Mutex::new(Dispatcher::new(&plan, Some(12)));
    let preferred = state.lock().unwrap().next(0.0).unwrap();
    let actual = execution_grant(&state, &permit, preferred);
    assert_eq!((actual.charge, actual.width), (1, 1));
    let mut dispatcher = state.lock().unwrap();
    assert_eq!(dispatcher.held_bytes(), 8, "memory custody is unchanged");
    assert_eq!(
        dispatcher.next(0.0).map(|binary| binary.index),
        Some(1),
        "the ungranted cores must not keep the fitting binary queued"
    );
    assert_eq!((dispatcher.held(), dispatcher.held_bytes()), (4, 12));
    assert!(dispatcher.next(0.0).is_none());
    dispatcher.finish(0).unwrap();
    assert_eq!((dispatcher.held(), dispatcher.held_bytes()), (3, 4));
    assert_eq!(dispatcher.next(0.0).unwrap().index, 2);
    dispatcher.finish(1).unwrap();
    dispatcher.finish(2).unwrap();
    assert_eq!((dispatcher.held(), dispatcher.held_bytes()), (0, 0));
    assert_eq!(plan.order[0].charge, 4, "the preferred plan is preserved");
    drop(permit);
    assert!(ledger.of_pid(std::process::id()).unwrap().is_empty());
}

/// The ledger's core request is elastic. A smaller positive grant must start the binary
/// within that grant, rather than drop its admission and repeat forever. The unchanged
/// four-core plan is the control; a claim with no core authority starts nothing.
#[test]
fn a_binary_runs_within_its_actual_ledger_core_grant() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    let (_, plan) = charged(&[4], 4);
    let planned = plan.order[0];
    for available in 0..=4 {
        let ledger = aterm_capacity::Ledger::at(tmp.path().join(format!("ledger-{available}")));
        let machine =
            aterm_capacity::Fixed::new(aterm_capacity::Reading::of_gib(48, 40, available.max(1)));
        let mut attempt = |need: aterm_capacity::Need, mode| {
            assert_eq!(
                mode, None,
                "ordinary admission retains the ledger's authority"
            );
            assert_eq!(need.cores, 4, "the preferred plan remains unchanged");
            let need = if available == 0 { need.cores(0) } else { need };
            aterm_capacity::admit::try_admit_with(&ledger, &machine, need)
        };
        let permit = binary_permit_with(Some(&ctx), None, &planned, &mut attempt);
        if available == 0 {
            assert!(permit.is_none(), "zero granted cores cannot start a binary");
        } else {
            let permit = permit
                .expect("a positive grant must make progress")
                .expect("the ledger admits the request");
            assert_eq!(permit.claim.as_ref().unwrap().cores(), available);
            let actual = permit.granted(planned);
            assert_eq!((actual.charge, actual.width), (available, available));
            drop(permit);
        }
        assert!(ledger.of_pid(std::process::id()).unwrap().is_empty());
    }
}

/// UNDER THE RELEASE'S POOL a binary holds its charge in the pool's tokens while it runs. On
/// four slots with the release's build holding two, two binaries charged two each fit the
/// share together, yet run one at a time on the two tokens left: while each runs, no token
/// is free, and every token comes back. NEGATIVE CONTROL: a dispatch that takes no tokens
/// runs a binary with two free.
#[cfg(unix)]
#[test]
fn a_pooled_binary_holds_its_charge_in_the_releases_tokens_while_it_runs() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let pool = aterm_capacity::Jobserver::new(&tmp.path().join("jobs"), 4).expect("a pool");
    let build: Vec<_> = (0..2)
        .map(|_| pool.try_acquire().expect("reads").expect("a token"))
        .collect();
    let (bins, plan) = charged(&[2, 2], 4);
    let running = AtomicUsize::new(0);
    let most = AtomicUsize::new(0);
    let ran = dispatch_pooled(
        &bins,
        &plan,
        None,
        Some(&pool),
        &|| 0.0,
        &|| false,
        |_, _, _| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            assert!(
                pool.try_acquire().expect("reads").is_none(),
                "the build's two and this binary's two hold every token"
            );
            running.fetch_sub(1, Ordering::SeqCst);
            Some(Run {
                case_observations: None,
                ok: true,
                code: Some(0),
                ..Run::default()
            })
        },
        |_, _, _| {},
    );
    assert!(ran.iter().all(Option::is_some), "both ran");
    assert_eq!(
        most.load(Ordering::SeqCst),
        1,
        "on two tokens, one at a time"
    );
    drop(build);
    let back: Vec<_> = std::iter::from_fn(|| pool.try_acquire().expect("reads")).collect();
    assert_eq!(back.len(), 4, "every token came back");
}

/// An incomplete pool attempt returns its memory and partial tokens before trying a
/// smaller peer. The peer runs while the earlier binary stays queued; once the build
/// returns its token the larger binary acquires its exact recorded memory and all slots.
/// NEGATIVE CONTROL: retaining the failed attempt's memory or any token fails conservation.
#[cfg(unix)]
#[test]
fn a_blocked_pool_reservation_keeps_no_memory_or_partial_tokens_and_a_peer_starts() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    SCRATCH.set(Some((
        aterm_capacity::Ledger::at(tmp.path().join("ledger")),
        Vec::new(),
    )));
    let mut ctx = crate::Ctx::new(
        tmp.path().to_path_buf(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        tmp.path().join("scratch"),
    );
    ctx.memory = Some(crate::memory::Book::at(
        None,
        Some(48 << 30),
        Some(40 << 30),
    ));
    let pool = aterm_capacity::Jobserver::new(&tmp.path().join("jobs"), 4).unwrap();
    let build = pool.try_acquire().unwrap().unwrap();
    let (_, mut plan) = charged(&[4, 1], 4);
    plan.order[0].peak = 512 << 20;
    plan.order[1].peak = 32 << 20;
    let mut dispatcher = Dispatcher::new(&plan, None);
    let mut permit = None;
    let attempt = &mut |need, mode: Option<aterm_capacity::Admit>| {
        scratch_admit(need, mode.unwrap_or(aterm_capacity::Admit::Wait)).map(Some)
    };
    let b = dispatcher
        .next_with(0.0, &mut |b| {
            permit = binary_permit_with(Some(&ctx), Some(&pool), b, attempt);
            permit.is_some()
        })
        .unwrap();
    assert_eq!(b.index, 1);
    assert_eq!(dispatcher.phase(0), Some(test_plan::Phase::Queued));
    let permit = permit.unwrap().unwrap();
    assert_eq!(permit.claim.as_ref().unwrap().record().need.mem, 32 << 20);
    SCRATCH.with_borrow(|s| {
        let records = s.as_ref().unwrap().0.of_pid(std::process::id()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].need.mem, 32 << 20);
    });
    let free: Vec<_> = std::iter::from_fn(|| pool.try_acquire().unwrap()).collect();
    assert_eq!(
        free.len(),
        2,
        "only the build and admitted peer hold tokens"
    );
    drop((free, permit, build));
    dispatcher.finish(1).unwrap();
    let mut permit = None;
    assert_eq!(
        dispatcher
            .next_with(0.0, &mut |b| {
                permit = binary_permit_with(Some(&ctx), Some(&pool), b, attempt);
                permit.is_some()
            })
            .unwrap()
            .index,
        0
    );
    let permit = permit.unwrap().unwrap();
    assert_eq!(permit.claim.as_ref().unwrap().record().need.mem, 512 << 20);
    assert!(pool.try_acquire().unwrap().is_none());
    drop(permit);
    dispatcher.finish(0).unwrap();
    let back: Vec<_> = std::iter::from_fn(|| pool.try_acquire().unwrap()).collect();
    assert_eq!(back.len(), 4);
    SCRATCH.with_borrow(|s| {
        assert!(
            s.as_ref()
                .unwrap()
                .0
                .of_pid(std::process::id())
                .unwrap()
                .is_empty()
        )
    });
    SCRATCH.set(None);
}

/// LOST TOKENS. On a pool of four slots a client died holding two, so two tokens are all the
/// pool will ever hold. A binary charged four, with no binary of the dispatch running and no
/// child of the run alive, starts on those two: its charge and its width are two, it holds both
/// while it runs, and both come back. NEGATIVE CONTROL: while a child of the run is alive the
/// same binary waits for its whole charge, and the stop leaves it unstarted.
#[cfg(unix)]
#[test]
fn a_lone_binary_starts_on_the_tokens_a_pool_that_lost_some_still_holds() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let pool = aterm_capacity::Jobserver::new(&tmp.path().join("jobs"), 4).expect("a pool");
    // The dead client's two: taken and never given back while the dispatch runs.
    let lost: Vec<_> = (0..2)
        .map(|_| pool.try_acquire().expect("reads").expect("a token"))
        .collect();
    let (bins, plan) = charged(&[4], 4);
    let ran_as = Mutex::new(None);
    let ran = dispatch_resources(
        &bins,
        &plan,
        None,
        Some(&pool),
        None,
        None,
        &|| 0.0,
        &|| false,
        &|| true,
        |_, planned, _| {
            *ran_as.lock().unwrap() = Some(*planned);
            assert!(
                pool.try_acquire().expect("reads").is_none(),
                "it holds every token the pool had"
            );
            Some(Run {
                case_observations: None,
                ok: true,
                code: Some(0),
                ..Run::default()
            })
        },
        |_, _, _| {},
    );
    let planned = ran_as.lock().unwrap().expect("it ran");
    assert_eq!(
        (planned.charge, planned.width),
        (2, 2),
        "within its two tokens"
    );
    assert_eq!(
        ran[0].as_ref().expect("it answered").planned.charge,
        2,
        "the report names what it held"
    );
    let back: Vec<_> = std::iter::from_fn(|| pool.try_acquire().expect("reads")).collect();
    assert_eq!(back.len(), 2, "its two came back");
    drop(back);

    let asked = (Mutex::new(false), Condvar::new());
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let started = AtomicUsize::new(0);
    let waited = std::thread::scope(|scope| {
        scope.spawn(|| {
            let (seen, waited) = asked
                .1
                .wait_timeout_while(
                    asked.0.lock().unwrap(),
                    aterm_spec::wait::MUST_HAPPEN,
                    |seen| !*seen,
                )
                .unwrap();
            assert!(*seen && !waited.timed_out(), "the dispatch never asked");
            stopped.store(true, Ordering::SeqCst);
        });
        dispatch_resources(
            &bins,
            &plan,
            None,
            Some(&pool),
            None,
            None,
            &|| 0.0,
            &|| stopped.load(Ordering::SeqCst),
            &|| {
                *asked.0.lock().unwrap() = true;
                asked.1.notify_all();
                false
            },
            |_, _, _| {
                started.fetch_add(1, Ordering::SeqCst);
                Some(Run::default())
            },
            |_, _, _| {},
        )
    });
    assert_eq!(
        started.load(Ordering::SeqCst),
        0,
        "a live child may give a token back: it waits"
    );
    assert!(waited[0].is_none(), "the stop left it unstarted");
    drop(lost);
    let back: Vec<_> = std::iter::from_fn(|| pool.try_acquire().expect("reads")).collect();
    assert_eq!(back.len(), 4, "every token is in the pool again");
}

/// The permit of a lone binary on a pool that holds none of its tokens is still a wait: with
/// nothing to run on it starts nothing, and one that holds its whole charge runs as planned.
#[cfg(unix)]
#[test]
fn a_lone_binary_with_no_token_waits_and_a_whole_charge_runs_as_planned() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let pool = aterm_capacity::Jobserver::new(&tmp.path().join("jobs"), 2).expect("a pool");
    let (_, plan) = charged(&[2], 2);
    let b = plan.order[0];
    let attempt = &mut |_, _| -> Result<Option<aterm_capacity::Claim>, aterm_capacity::Error> {
        unreachable!("no run, so no memory claim")
    };
    let whole = binary_permit_noting(None, Some(&pool), &b, attempt, true, &mut None)
        .expect("admitted")
        .expect("no FIFO error");
    assert_eq!(whole.granted(b), b, "its whole charge: as planned");
    let mut waiting = None;
    assert!(
        binary_permit_noting(None, Some(&pool), &b, attempt, true, &mut waiting).is_none(),
        "no token is left to start on"
    );
    assert!(waiting.is_some(), "and the wait names the pool");
    drop(whole);
    let back: Vec<_> = std::iter::from_fn(|| pool.try_acquire().expect("reads")).collect();
    assert_eq!(back.len(), 2);
}

/// A binary whose tokens the stop ends the wait for never runs and is left unstarted, as the
/// stop leaves every binary, and the part of its charge it took goes back. The build holds
/// one of two slots; the binary, charged both, is admitted (its `live` read), and the stop
/// comes while it waits for the second. NEGATIVE CONTROL: a dispatch that takes no tokens runs
/// it once the share admits it.
#[cfg(unix)]
#[test]
fn a_binary_whose_tokens_never_come_is_left_unstarted_and_never_run() {
    let tmp = aterm_tempfile::tempdir().expect("scratch");
    let pool = aterm_capacity::Jobserver::new(&tmp.path().join("jobs"), 2).expect("a pool");
    let build = pool.try_acquire().expect("reads").expect("a token");
    let (bins, plan) = charged(&[2], 2);
    let admitted = (Mutex::new(false), Condvar::new());
    let stopped = std::sync::atomic::AtomicBool::new(false);
    let live = || {
        *admitted.0.lock().unwrap() = true;
        admitted.1.notify_all();
        0.0
    };
    let started = AtomicUsize::new(0);
    let ran = std::thread::scope(|scope| {
        scope.spawn(|| {
            let (seen, waited) = admitted
                .1
                .wait_timeout_while(
                    admitted.0.lock().unwrap(),
                    aterm_spec::wait::MUST_HAPPEN,
                    |seen| !*seen,
                )
                .unwrap();
            assert!(
                *seen && !waited.timed_out(),
                "the binary was never admitted"
            );
            stopped.store(true, Ordering::SeqCst);
        });
        dispatch_pooled(
            &bins,
            &plan,
            None,
            Some(&pool),
            &live,
            &|| stopped.load(Ordering::SeqCst),
            |_, _, _| {
                started.fetch_add(1, Ordering::SeqCst);
                Some(Run::default())
            },
            |_, _, _| {},
        )
    });
    assert_eq!(started.load(Ordering::SeqCst), 0, "it never ran");
    assert!(ran[0].is_none(), "the stop left it unstarted");
    let free = pool.try_acquire().expect("reads");
    assert!(free.is_some(), "the token it took went back");
    assert!(
        pool.try_acquire().expect("reads").is_none(),
        "the build's is its own"
    );
    drop((free, build));
}

/// THE LONG POLES: the run's own wall, then its slowest binaries longest
/// first, at most [`POLES_SHOWN`] of them, each by cargo's re-run spec.
#[test]
fn the_long_poles_name_the_slowest_binaries_longest_first() {
    let secs = [3, 41, 0, 7, 12, 1, 2, 5, 9, 4];
    let bins: Vec<Binary> = (0..secs.len())
        .map(|i| Binary {
            harness: false,
            header: format!("     Running tests/t{i}.rs (target/debug/deps/t{i}-1)"),
            spec: format!("-p x --test t{i}"),
            record: rec(&format!("/r/t{i}"), "x"),
        })
        .collect();
    let walls: Vec<Duration> = secs.iter().map(|&s| Duration::from_secs(s)).collect();
    let line = poles(&bins, &walls, Duration::from_secs(44));
    assert!(line.contains("the run took 44s"), "{line}");
    let at = |i: usize| line.find(&format!("--test t{i} ")).map(|p| (p, i));
    let named: Vec<usize> = {
        let mut found: Vec<(usize, usize)> = (0..secs.len()).filter_map(at).collect();
        found.sort_unstable();
        found.into_iter().map(|(_, i)| i).collect()
    };
    assert_eq!(named, [1, 4, 8, 3, 7, 9, 0, 6], "{line}");
}

/// A BINARY THE RUN'S CEILING LEFT NO TIME TO START is a FAIL in cargo's
/// shape — its header, the run's TIMEOUT words, cargo's re-run
/// line — one finding for the row, which says so.
#[test]
fn a_binary_the_ceiling_left_no_time_for_is_a_fail() {
    let b = Binary {
        harness: false,
        header: "     Running tests/late.rs (target/debug/deps/late-1)".into(),
        spec: "-p x --test late".into(),
        record: rec("/r/late", "x"),
    };
    let run = not_started(&b, Duration::from_secs(10_800));
    assert!(!run.ok);
    assert!(run.output.starts_with(&b.header), "{}", run.output);
    assert!(
        run.output.contains(crate::findings::CEILING_KILL),
        "{}",
        run.output
    );
    let all = combined(std::slice::from_ref(&b), std::slice::from_ref(&run));
    assert!(!all.ok);
    let found = crate::findings::test_findings(
        "targo test --tests",
        &all.output,
        &crate::findings::Rerun::default(),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].rerun, None, "a whole-row finding: {found:?}");
    assert!(
        found[0]
            .message
            .iter()
            .any(|l| l.contains(crate::findings::CEILING_KILL)),
        "{found:?}"
    );
}

/// THE COMBINED LOG IS CARGO'S, so every reader of cargo's reads it: the
/// failed tests come back with the re-run spec cargo would have printed, a
/// crash (no libtest 101) says how the process ended, and a binary whose
/// every failure is the machine's still reads as the machine's.
#[test]
fn the_combined_log_reads_as_cargos_to_every_reader_of_cargos() {
    let bins: Vec<Binary> = ["a", "b", "c"]
        .iter()
        .map(|n| Binary {
            harness: false,
            header: format!("     Running tests/{n}.rs (target/debug/deps/{n}-1)"),
            spec: format!("-p x --test {n}"),
            record: rec(&format!("/r/target/debug/deps/{n}-1"), "x"),
        })
        .collect();
    let pass = |b: &Binary| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: true,
        output: format!(
            "{}\n\nrunning 1 test\ntest ok_one ... ok\n\ntest result: ok. 1 passed; 0 \
             failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
            b.header
        ),
        code: Some(0),
        spawn_error: None,
        usage: None,
    };
    let fail = |b: &Binary| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!(
            "{}\n\nrunning 1 test\ntest t::broke ... FAILED\n\nfailures:\n\n\
             ---- t::broke stdout ----\n\nthread 't::broke' panicked at src/x.rs:3:5:\n\
             assertion failed: false\n\nfailures:\n    t::broke\n\ntest result: FAILED. 0 \
             passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
            b.header
        ),
        code: Some(101),
        spawn_error: None,
        usage: None,
    };
    let runs = [pass(&bins[0]), fail(&bins[1]), pass(&bins[2])];
    let all = combined(&bins, &runs);
    assert!(!all.ok);
    let tests = crate::libtest::failed_tests(&all.output).expect("accounted for");
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].spec, "-p x --test b");
    assert_eq!(tests[0].name, "t::broke");
    let (outcome, event, _) = binary_event(
        &bins[1],
        &Ran::of(runs[1].clone(), Duration::ZERO, Duration::ZERO),
        true,
        None,
        false,
    );
    assert_eq!(outcome, Outcome::Fail(Severity::GateFailed), "a proven red");
    let event: aterm_json::Value =
        aterm_json::from_str(event.strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(event["failures"][0]["case"].as_str(), Some("t::broke"));
    assert_eq!(
        event["failures"][0]["output"].as_str(),
        Some(tests[0].block.as_str())
    );
    let findings = crate::findings::test_findings(
        "targo test --tests",
        &all.output,
        &crate::findings::Rerun::default(),
    );
    assert_eq!(findings[0].id, "-p x --test b -- t::broke");
    assert!(
        all.output
            .ends_with("error: 1 target failed:\n    `-p x --test b`\n")
    );
    let a = all.output.find("tests/a.rs").unwrap();
    let b = all.output.find("tests/b.rs").unwrap();
    let c = all.output.find("tests/c.rs").unwrap();
    assert!(a < b && b < c, "declared order");

    let crashed = Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!("{}\n\nrunning 1 test\n", bins[1].header),
        code: None,
        spawn_error: None,
        usage: None,
    };
    let all = combined(&bins, &[pass(&bins[0]), crashed, pass(&bins[2])]);
    assert!(all.output.contains(
        "error: test failed, to rerun pass `-p x --test b`\n\nCaused by:\n  process didn't \
         exit successfully: `/r/target/debug/deps/b-1 --skip slow::` (killed by a signal)\n"
    ));
    assert_eq!(
        crate::libtest::failed_tests(&all.output),
        None,
        "a crash leaves the log unaccounted, as cargo's did: the row is one finding"
    );

    let sentinel = crate::libtest::COULD_NOT_RUN_SENTINEL;
    let refused = Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: fail(&bins[1])
            .output
            .replace("assertion failed: false", &format!("{sentinel} — strays")),
        code: Some(101),
        spawn_error: None,
        usage: None,
    };
    let all = combined(&bins, &[pass(&bins[0]), refused, pass(&bins[2])]);
    let why = all
        .environment_failure()
        .expect("the machine's, not a finding");
    assert!(
        why.ends_with("— strays"),
        "the row names the refusal's why: {why}"
    );
    assert!(combined(&bins, &[pass(&bins[0]), pass(&bins[1]), pass(&bins[2])]).ok);
}

/// A test executable runs with its own package's binaries, named as Cargo names them while
/// it compiles the test (`CARGO_BIN_EXE_<name>`), which a test reads at run time
/// (`aterm_tempfile::bin_exe`); a value its record already holds gives way. NEGATIVE CONTROL:
/// package y's binary reaches no test of package x, and x's none of y's.
#[test]
fn a_test_executable_runs_with_its_packages_binaries() {
    let of = |pkg: &str, exe: &str, kind: &str, name: &str, test: bool| {
        artifact(exe, kind, name, test).replace("/r/crates/x#", &format!("/r/crates/{pkg}#"))
    };
    let mut log = [
        of("x", "/r/target/debug/deps/it-1", "test", "it", true),
        of("x", "/r/target/debug/x-tool", "bin", "x-tool", false),
        of("y", "/r/target/debug/y-tool", "bin", "y-tool", false),
        of("y", "/r/target/debug/deps/yt-2", "test", "yt", true),
    ]
    .concat();
    let exes = take_test_executables(&mut log).expect("readable");
    let named = |env: &[(OsString, OsString)]| {
        env.iter()
            .filter(|(k, _)| k.to_string_lossy().starts_with("CARGO_BIN_EXE_"))
            .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        named(&exes[0].bin_env),
        ["CARGO_BIN_EXE_x-tool=/r/target/debug/x-tool"]
    );
    assert_eq!(
        named(&exes[1].bin_env),
        ["CARGO_BIN_EXE_y-tool=/r/target/debug/y-tool"]
    );
    let mut record = rec("/r/target/debug/deps/it-1", "x");
    record
        .env
        .push(("CARGO_BIN_EXE_x-tool".into(), "/stale/x-tool".into()));
    let header = "     Running tests/it.rs (target/debug/deps/it-1)\n";
    let bins = plan(vec![record], header, &exes).expect("a plan");
    assert_eq!(
        named(&binary_cmd(&bins[0], 1, 1, false, Watch::default()).envs),
        ["CARGO_BIN_EXE_x-tool=/r/target/debug/x-tool"],
        "the compile's binary, once, in place of the stale one"
    );
}

/// A binary verify ended unrun (or could not start) is its own COULD NOT RUN row, naming its
/// spec, and the rest decide the run's row as before: a pass beside it and a failed test
/// itemized under cargo's rerun spec. The combined log keeps its output and names it no
/// failed target. Negative control: a binary killed by a signal with no `spawn_error` is
/// still part of the one finding. With every binary unrun, only their rows; with no binary at
/// all (a resume carried every one), the run's one row as before.
#[test]
fn a_binary_verify_ended_unrun_is_its_own_could_not_run_row() {
    let bins: Vec<Binary> = ["a", "b", "c"]
        .iter()
        .map(|n| Binary {
            harness: false,
            header: format!("     Running tests/{n}.rs (target/debug/deps/{n}-1)"),
            spec: format!("-p x --test {n}"),
            record: rec(&format!("/r/target/debug/deps/{n}-1"), "x"),
        })
        .collect();
    let pass = |b: &Binary| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: true,
        output: format!(
            "{}\n\nrunning 1 test\ntest ok_one ... ok\n\ntest result: ok. 1 passed; 0 \
             failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
            b.header
        ),
        code: Some(0),
        spawn_error: None,
        usage: None,
    };
    let fail = |b: &Binary| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!(
            "{}\n\nrunning 1 test\ntest t::broke ... FAILED\n\nfailures:\n\n\
             ---- t::broke stdout ----\n\nthread 't::broke' panicked at src/x.rs:3:5:\n\
             assertion failed: false\n\nfailures:\n    t::broke\n\ntest result: FAILED. 0 \
             passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n",
            b.header
        ),
        code: Some(101),
        spawn_error: None,
        usage: None,
    };
    let why = "the run was interrupted before this child started";
    let unrun = |b: &Binary| Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!("{}\naterm-verify: cannot run {}: {why}", b.header, b.spec),
        code: None,
        spawn_error: Some(why.to_string()),
        usage: None,
    };
    let label = "targo test --tests";
    let decided = |runs: &[Run]| {
        let mut r = Report::new("test");
        r.raw(combined(&bins, runs).output);
        decide_runs(&mut r, &bins, runs, label, |r, rest| {
            r.decide_test_child(rest, label, &crate::findings::Rerun::default());
        });
        r
    };
    let cnr = Outcome::Fail(Severity::CouldNotRun);
    let gf = Outcome::Fail(Severity::GateFailed);

    let r = decided(&[pass(&bins[0]), unrun(&bins[1]), fail(&bins[2])]);
    let rows: Vec<_> = r.outcomes().collect();
    let b_row = format!("{label} — -p x --test b: {why}");
    assert_eq!(rows, [(cnr, b_row.as_str()), (gf, label)], "{}", r.render());
    let ids: Vec<String> = r.findings().into_iter().map(|f| f.id).collect();
    assert_eq!(ids, ["-p x --test c -- t::broke"], "{}", r.render());
    let block = r.render();
    assert!(
        block.contains("error: 1 target failed:\n    `-p x --test c`\n")
            && !block.contains("rerun pass `-p x --test b`")
            && !block.contains("could not execute"),
        "the log names no unrun binary a failed target:\n{block}"
    );

    let crashed = Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: format!("{}\n\nrunning 1 test\n", bins[1].header),
        code: None,
        spawn_error: None,
        usage: None,
    };
    let r = decided(&[pass(&bins[0]), crashed, fail(&bins[2])]);
    assert_eq!(
        r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
        [gf],
        "{}",
        r.render()
    );

    let r = decided(&[unrun(&bins[0]), unrun(&bins[1]), unrun(&bins[2])]);
    assert!(
        r.outcomes()
            .all(|(o, l)| o == cnr && !l.contains(crate::libtest::RAN_NO_TEST)),
        "{}",
        r.render()
    );
    assert_eq!(r.outcomes().count(), 3, "{}", r.render());

    let mut r = Report::new("test");
    decide_runs(&mut r, &[], &[], label, |r, rest| {
        r.decide_test_child(rest, label, &crate::findings::Rerun::default());
    });
    assert_eq!(r.outcomes().count(), 1, "{}", r.render());
}

/// The compile's artifacts carry their manifest's harness: a `[[test]]` that says
/// `harness = false` is false and its autodiscovered sibling true, read from the
/// `manifest_path` each message names. NEGATIVE CONTROL: an artifact with no manifest path
/// is false.
#[test]
fn each_test_executable_carries_its_manifests_harness() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let manifest = dir.path().join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"x\"\n[[test]]\nname = \"custom\"\nharness = false\n",
    )
    .expect("a manifest");
    let with = |line: String| {
        line.replacen(
            "\"reason\":\"compiler-artifact\",",
            &format!(
                "\"reason\":\"compiler-artifact\",\"manifest_path\":{},",
                aterm_json::json!(manifest.to_str().expect("utf-8"))
            ),
            1,
        )
    };
    let mut log = [
        with(artifact("/r/t/custom-1", "test", "custom", true)),
        with(artifact("/r/t/plain-1", "test", "plain", true)),
        artifact("/r/t/bare-1", "test", "bare", true),
    ]
    .concat();
    let exes = take_test_executables(&mut log).expect("read");
    assert_eq!(
        exes.iter()
            .map(|e| (e.flag.as_str(), e.harness))
            .collect::<Vec<_>>(),
        [
            ("--test custom", false),
            ("--test plain", true),
            ("--test bare", false)
        ]
    );
}

/// A TARGET'S HARNESS IS ITS MANIFEST'S WORD: `harness = false` under `[lib]`, `[[test]]` or
/// `[[bench]]` keeps libtest's flags from it; an autodiscovered target, and one whose entry
/// says nothing, runs the harness; a name two entries share, and a manifest that could not be
/// read, are false, so a JSON flag never reaches a custom main. NEGATIVE CONTROL: the
/// entries' siblings, harness unsaid, read true.
#[test]
fn a_targets_harness_is_its_manifests_word() {
    let manifest = aterm_toml::from_str::<aterm_toml::Value>(
        "[package]\nname = \"x\"\n[lib]\nharness = false\n\
         [[test]]\nname = \"custom\"\nharness = false\n\
         [[test]]\nname = \"plain\"\n\
         [[test]]\npath = \"tests/stem.rs\"\nharness = false\n\
         [[bench]]\nname = \"speed\"\nharness = false\n\
         [[bin]]\nname = \"twice\"\n[[bin]]\nname = \"twice\"\n",
    )
    .expect("a manifest");
    let m = Some(&manifest);
    assert!(!harness_of(m, &["lib"], "x"));
    assert!(!harness_of(m, &["rlib", "lib"], "x"));
    assert!(!harness_of(m, &["test"], "custom"));
    assert!(harness_of(m, &["test"], "plain"));
    assert!(!harness_of(m, &["test"], "stem"));
    assert!(harness_of(m, &["test"], "autodiscovered"));
    assert!(!harness_of(m, &["bench"], "speed"));
    assert!(harness_of(m, &["bench"], "other"));
    assert!(!harness_of(m, &["bin"], "twice"), "an ambiguous name");
    assert!(harness_of(m, &["bin"], "once"));
    assert!(!harness_of(None, &["test"], "plain"), "an unread manifest");
    let bare =
        aterm_toml::from_str::<aterm_toml::Value>("[package]\nname = \"y\"\n").expect("a manifest");
    assert!(harness_of(Some(&bare), &["lib"], "y"));
}

#[test]
fn the_replayed_command_is_the_record_with_its_thread_count() {
    let mut b = Binary {
        header: "     Running tests/a.rs (target/debug/deps/a-1)".into(),
        spec: "-p x --test a".into(),
        record: rec("/r/target/debug/deps/a-1", "x"),
        harness: true,
    };
    b.record
        .env
        .push((crate::stages::CHECKERS_ENV.into(), "1".into()));
    b.record
        .env
        .push((aterm_capacity::RELEASE_RUN_ENV.into(), "stale".into()));
    b.record
        .env
        .push(("ATERM_RELEASE_INPUTS_SEAL".into(), "/owner/seal".into()));
    let cmd = binary_cmd(&b, 7, 7, false, Watch::default());
    assert!(
        cmd.envs
            .iter()
            .all(|(key, _)| !key.as_encoded_bytes().starts_with(b"ATERM_RELEASE_"))
    );
    let json = binary_cmd(
        &b,
        7,
        7,
        false,
        Watch {
            json: true,
            stop_at_red: true,
            seed: None,
        },
    );
    assert_eq!(
        json.argv(),
        [
            "/r/target/debug/deps/a-1",
            "-Z",
            "unstable-options",
            "--format",
            "json",
            "--report-time",
            "--skip",
            "slow::"
        ]
    );
    assert_eq!(
        json.test_cases,
        Some(crate::exec::CaseWatch {
            spec: "-p x --test a".into(),
            binary: "/r/target/debug/deps/a-1".into(),
            json: true,
            stop_at_red: true,
            hang_after: crate::exec::TEST_BINARY_HANG,
        })
    );
    b.harness = false;
    assert_eq!(
        binary_cmd(
            &b,
            7,
            7,
            false,
            Watch {
                json: true,
                stop_at_red: true,
                seed: None,
            }
        )
        .argv(),
        cmd.argv(),
        "no JSON flag reaches a custom main"
    );
    b.harness = true;
    let armed = Watch {
        json: true,
        stop_at_red: true,
        seed: None,
    };
    for json in [false, true] {
        let seeded = Watch {
            json,
            seed: Some(17),
            ..armed
        };
        let command = binary_cmd(&b, 3, 3, false, seeded);
        assert!(command.args.iter().any(|arg| arg == "--shuffle-seed=17"));
        assert!(
            command
                .args
                .iter()
                .any(|arg| arg.to_string_lossy().starts_with("-Z"))
        );
        assert!(
            command
                .envs
                .iter()
                .any(|(k, v)| k == "RUST_TEST_THREADS" && v == "3")
        );
        let mut custom = b.clone();
        custom.harness = false;
        assert_eq!(binary_cmd(&custom, 3, 3, false, seeded).args, cmd.args);
    }
    for var in ["RUST_TEST_SHUFFLE", "RUST_TEST_SHUFFLE_SEED"] {
        let mut shuffled = b.clone();
        shuffled.record.env.push((var.into(), "1".into()));
        assert!(
            !binary_cmd(&shuffled, 7, 7, false, armed)
                .argv()
                .iter()
                .any(|a| a == "-Z"),
            "{var} is read only once -Z unstable-options is passed"
        );
    }
    let mut unstable = b.clone();
    unstable.record.argv.push("-Zunstable-options".into());
    unstable.record.argv.push("--shuffle".into());
    assert_eq!(
        binary_cmd(&unstable, 7, 7, false, armed).argv().len(),
        unstable.record.argv.len(),
        "a record that already passes -Z keeps its own flags alone"
    );
    for required in [false, true] {
        let cmd = binary_cmd(&b, 7, 7, required, Watch::default());
        let policy: Vec<_> = cmd
            .envs
            .iter()
            .filter(|(k, _)| k == crate::stages::CHECKERS_ENV)
            .map(|(_, v)| v.to_str().unwrap())
            .collect();
        assert_eq!(policy, [if required { "1" } else { "0" }]);
    }
    assert_eq!(cmd.argv(), ["/r/target/debug/deps/a-1", "--skip", "slow::"]);
    assert_eq!(cmd.cwd.as_deref(), Some(Path::new("/r/crates/x")));
    assert!(cmd.exact_env);
    assert_eq!(
        cmd.envs
            .iter()
            .filter(|(k, _)| k == "RUST_TEST_THREADS")
            .map(|(_, v)| v.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["7"],
        "one value, the run's — never the recorded 14 beside it"
    );
    assert!(
        cmd.envs
            .iter()
            .any(|(k, v)| k == "CARGO" && v == "/store/bin/targo")
    );
    assert_eq!(
        cmd.preamble.as_deref(),
        Some("     Running tests/a.rs (target/debug/deps/a-1)\n")
    );
}

/// Content evidence is made from the command after the full-set plan and JSON probe have
/// fixed it. NEGATIVE CONTROLS: changing either the actual planned width or the probe's final
/// JSON mode changes the key; hard-coding a width or keying the pre-probe command fails this.
#[test]
fn planned_width_and_final_json_mode_are_content_key_inputs() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let root = dir.path().join("workspace");
    let package = root.join("crates/p");
    let scratch = root.join("scratch");
    let executable = root.join("target/debug/deps/p-0000000000000000");
    std::fs::create_dir_all(package.join("src")).unwrap();
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/p\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    std::fs::write(root.join("trust.lock"), "build = 1\n").unwrap();
    std::fs::write(
        package.join("Cargo.toml"),
        "[package]\nname = \"p\"\nversion = \"0.0.0\"\n\
         [package.metadata.aterm.test-inputs]\n\
         bounded = true\ncontent-only = true\nread-only = true\npaths = []\nvolatile-environment = []\n",
    )
    .unwrap();
    std::fs::write(package.join("src/lib.rs"), "pub fn answer() -> u8 { 1 }\n").unwrap();
    std::fs::write(&executable, b"test executable").unwrap();
    let binary = Binary {
        header: format!(
            "     Running unittests src/lib.rs ({})",
            executable.display()
        ),
        spec: "-p p --lib".into(),
        record: Record {
            cwd: package.clone(),
            argv: vec![executable.into_os_string()],
            env: vec![
                ("CARGO_PKG_NAME".into(), "p".into()),
                ("CARGO_MANIFEST_DIR".into(), package.into_os_string()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ],
        },
        harness: true,
    };
    let ctx = crate::Ctx::new(
        root,
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        scratch.clone(),
    );
    let builder = crate::test_content::EvidenceBuilder::new(
        Some("11".repeat(32)),
        Some("22".repeat(32)),
        scratch,
    );
    let keyed = |width, json, seed| {
        let command = final_binary_cmd(
            &ctx,
            &binary,
            width,
            width,
            false,
            Watch {
                json,
                stop_at_red: false,
                seed,
            },
        );
        let command = exec::finalize_exact_command(command, ctx.exec_env())
            .expect("a replayed test command is exact");
        (builder.evidence(&binary, &command), command)
    };
    let (narrow, narrow_command) = keyed(2, false, None);
    let (wide, _) = keyed(7, false, None);
    let (json, json_command) = keyed(2, true, None);
    let (seeded, seeded_command) = keyed(2, false, Some(17));
    assert!(narrow.bounded(), "{narrow:?}");
    assert_ne!(narrow.content_key, wide.content_key, "planned width");
    assert_ne!(narrow.content_key, json.content_key, "final JSON mode");
    assert_ne!(narrow.content_key, seeded.content_key, "shuffle seed");
    assert!(
        narrow_command
            .argv()
            .iter()
            .all(|argument| argument != "json")
            && json_command
                .argv()
                .iter()
                .any(|argument| argument == "json")
    );
    assert!(
        seeded_command
            .argv()
            .iter()
            .any(|argument| argument == "--shuffle-seed=17")
    );
}

/// Carry projects the already-claimed full plan; it never widens or reorders the misses.
/// NEGATIVE CONTROL: asking the planner again after removing the middle binary would not
/// preserve these deliberately distinct widths, charges and predictions.
#[test]
fn carry_projects_the_fixed_full_plan_without_replanning() {
    let binary = |name: &str| Binary {
        header: String::new(),
        spec: format!("-p {name} --lib"),
        record: rec(&format!("/r/target/debug/deps/{name}-1"), name),
        harness: true,
    };
    let full = vec![binary("a"), binary("carried"), binary("b")];
    let plan = Plan {
        share: 8,
        target: Duration::from_secs(9),
        order: vec![
            Planned {
                index: 2,
                width: 7,
                charge: 8,
                peak: 700,
                predicted: Some(Duration::from_secs(3)),
            },
            Planned {
                index: 1,
                width: 5,
                charge: 6,
                peak: 500,
                predicted: Some(Duration::from_secs(5)),
            },
            Planned {
                index: 0,
                width: 2,
                charge: 3,
                peak: 200,
                predicted: Some(Duration::from_secs(7)),
            },
        ],
    };
    let kept = vec![full[0].clone(), full[2].clone()];
    assert_eq!(
        project_plan(&plan, &full, &kept),
        Plan {
            share: 8,
            target: Duration::from_secs(9),
            order: vec![
                Planned {
                    index: 1,
                    width: 7,
                    charge: 8,
                    peak: 700,
                    predicted: Some(Duration::from_secs(3)),
                },
                Planned {
                    index: 0,
                    width: 2,
                    charge: 3,
                    peak: 200,
                    predicted: Some(Duration::from_secs(7)),
                },
            ],
        }
    );
}

#[test]
fn plan_prefers_exact_exe_match_before_file_name_fallback() {
    let records = vec![rec("/r/target/debug/deps/a-1", "x")];
    let log = "     Running tests/a.rs (target/debug/deps/a-1)\n";
    let exes = vec![
        TestExe {
            harness: false,
            exe: PathBuf::from("/other/target/debug/deps/a-1"),
            flag: "--lib".to_string(),
            bin_env: Vec::new(),
        },
        TestExe {
            harness: false,
            exe: PathBuf::from("/r/target/debug/deps/a-1"),
            flag: "--test a".to_string(),
            bin_env: Vec::new(),
        },
    ];
    let bins = plan(records, log, &exes).expect("plan");
    assert_eq!(bins[0].spec, "-p x --test a");
}

#[test]
fn record_decode_rejects_duplicate_cwd() {
    let mut bytes = rec("/r/target/debug/deps/a-1", "x").encode();
    bytes.extend_from_slice(b"cwd\0/other\0");
    assert!(Record::decode(&bytes).is_none());
}

/// Pool layout initialized by `testrun` matches `aterm_spec::subprocess_lane`'s file-lock
/// protocol and bounds concurrent exclusive leases across callers.
/// NEGATIVE CONTROL: a third thread contending on a two-slot `testrun` pool cannot acquire an
/// exclusive slot before its timeout while two holders remain active.
#[cfg(any(unix, windows))]
#[test]
fn subprocess_pool_protocol_matches_aterm_spec_lane() {
    use aterm_spec::subprocess_lane;

    assert_eq!(SUBPROCESS_SLOTS_ENV, subprocess_lane::SUBPROCESS_SLOTS_ENV);
    for cores in [0, 1, 2, 5, 6, 9, 14, 32] {
        let slots = subprocess_slots_for_cores(cores);
        assert_eq!(slots, subprocess_lane::slots_for_cores(cores));
        assert_eq!(
            subprocess_boot_slots(slots),
            subprocess_lane::boot_slots_for_pool(slots)
        );
    }

    let tmp = aterm_tempfile::tempdir().expect("scratch pool");
    let dir = tmp.path().join("pool");
    std::fs::create_dir(&dir).expect("owned pool directory");
    init_subprocess_pool_dir(&dir, 2).expect("init 2-slot pool");
    assert!(
        init_subprocess_pool_dir(&dir, 2).is_err(),
        "a populated fixed layout must not be rewritten"
    );

    let (release_a, wait_a) = std::sync::mpsc::channel::<()>();
    let (release_b, wait_b) = std::sync::mpsc::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();

    std::thread::scope(|scope| {
        let dir_ref = &dir;
        let ready_tx_a = ready_tx.clone();
        let h_a = scope.spawn(move || {
            let _lease = subprocess_lane::acquire_with_pool(Some(dir_ref), Duration::from_secs(60))
                .expect("stack admission");
            let _boot = subprocess_lane::acquire_boot_permit_with_pool(
                Some(dir_ref),
                Duration::from_secs(60),
            )
            .expect("boot admission");
            ready_tx_a.send(true).expect("send a");
            let _ = wait_a.recv();
        });
        let h_b = scope.spawn(move || {
            let lease = subprocess_lane::acquire_with_pool(Some(dir_ref), Duration::from_secs(60));
            ready_tx.send(lease.is_ok()).expect("send b");
            let _ = wait_b.recv();
        });
        assert!(
            ready_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("a acquired")
        );
        assert!(
            ready_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("b acquired")
        );

        let third = scope
            .spawn(move || {
                subprocess_lane::acquire_with_pool(Some(dir_ref), Duration::ZERO)
                    .err()
                    .map(|e| e.kind())
            })
            .join()
            .expect("third joined");
        assert_eq!(third, Some(std::io::ErrorKind::TimedOut));

        drop(release_a);
        drop(release_b);
        h_a.join().expect("join a");
        h_b.join().expect("join b");
    });
}

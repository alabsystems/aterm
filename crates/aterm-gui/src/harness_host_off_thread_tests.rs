// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's reads that talk to the rest of the machine run off the threads that decide: a read
//! held past its bound holds neither the host's pass nor a worker's loop, and lands as it ends.

use super::*;

use aterm_agent::harness::upgrade::StandingLimit;
use aterm_agent::supervise::policy::turn_end::Restart;

const NAMED: &str = "0badf00d-1111-2222-3333-444455556666";

/// The conversation a worker's kept snapshot named before the read.
const BEFORE: &str = "0badf00d-0000-2222-3333-444455556666";

/// A worker of `agent` whose relaunch snapshot read the returned gate holds, waited for `bound` at
/// most, with `tweak` applied to its hooks.
fn gated_idle(
    agent: Program,
    bound: Duration,
    tweak: impl FnOnce(&mut Hooks, &Arc<GatedRead>),
) -> (Arc<WorkerIdle>, Arc<GatedRead>) {
    let world = Arc::new(World::default());
    let mut h = hooks(&world, parking_body(&world));
    let held = Arc::new(GatedRead::default());
    h.acts.snapshot = held.act(NAMED);
    h.acts.point_read = bound;
    tweak(&mut h, &held);
    (Arc::new(idle_of(h, agent, None)), held)
}

/// The transcript, its tail and the standing limit all say the conversation the snapshot names.
fn transcribed(h: &mut Hooks, _: &Arc<GatedRead>) {
    h.acts.transcript_tail = Arc::new(|snap| snap.session.clone());
    h.acts.transcript = Arc::new(|snap| {
        snap.session
            .clone()
            .map(|text| (PathBuf::from("t.jsonl"), text))
    });
    h.acts.standing_limit = Arc::new(|_, snap| {
        snap.session.clone().map(|text| StandingLimit {
            text,
            until: None,
            window: None,
            at: None,
            row: None,
        })
    });
}

#[test]
fn a_slow_note_behind_never_holds_the_host_pass() {
    let a = Arc::new(Acting::default());
    a.busy.store(true, Ordering::SeqCst);
    for sid in ["s-a", "s-b", "s-c"] {
        a.open.lock().unwrap().insert(sid.to_string());
    }
    a.set(&[("s-a", Program::Claude), ("s-b", Program::Claude)]);
    let mut hooks = a.hooks();
    let held = Arc::new(GatedRead::default());
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let (gate, q) = (Arc::clone(&held), Arc::clone(&asked));
    hooks.acts.behind = Arc::new(move |sid, _| {
        let first = {
            let mut asked = q.lock().unwrap();
            asked.push(sid.to_string());
            asked.iter().filter(|s| *s == sid).count() == 1
        };
        if sid == "s-a" && first {
            gate.pass();
            return Behind::Busy;
        }
        Behind::Noted
    });
    let asks_of = |sid: &str| asked.lock().unwrap().iter().filter(|s| *s == sid).count();
    let host = HostHandle::start(on(), false, false, hooks);
    until("both workers", || {
        let live = host.live();
        ["s-a", "s-b"]
            .iter()
            .all(|sid| live.contains(&(*sid).to_string()))
    });
    host.note_activation();
    until("s-a's note is out", || {
        held.began.load(Ordering::SeqCst) == 1
    });
    a.set(&[
        ("s-a", Program::Claude),
        ("s-b", Program::Claude),
        ("s-c", Program::Claude),
    ]);
    until("s-c's worker starts while s-a's note is out", || {
        host.live().contains(&"s-c".to_string())
    });
    until("s-b's note is asked beside it", || asks_of("s-b") == 1);
    a.passes();
    assert_eq!(asks_of("s-a"), 1, "one ask of s-a's note in flight");
    // CONTROL: the ask that comes back lands, and what it said is used: busy, so asked again.
    held.let_go();
    until("s-a's busy note is asked again", || asks_of("s-a") == 2);
    host.shutdown_and_join();
}

#[test]
fn a_slow_relaunch_read_never_holds_the_turn_end_reading() {
    type Reader = fn(&WorkerIdle) -> Option<String>;
    let readers: [(&str, Reader); 3] = [
        ("standing_limit", |idle| {
            idle.standing_limit().map(|l| l.text)
        }),
        ("transcript_tail", |idle| idle.transcript_tail()),
        ("transcript", |idle| idle.transcript().map(|(_, text)| text)),
    ];
    let named_before = |idle: &WorkerIdle| {
        with_kept_of(&idle.kept, |k| {
            k.snapshot.as_mut().unwrap().session = Some(BEFORE.to_string());
        });
    };
    for (what, read) in readers {
        // Its agent holds the tab: the kept snapshot is read while the read is out, as the idle
        // point goes on with it.
        let (idle, held) = gated_idle(Program::Claude, Duration::ZERO, |h, held| {
            transcribed(h, held);
            h.acts.follow = Arc::new(|_, _| Foreground::Agent);
        });
        named_before(&idle);
        let at = Arc::clone(&idle);
        let reading = std::thread::spawn(move || read(&at));
        until(&format!("{what} returns while its read is held"), || {
            reading.is_finished()
        });
        assert_eq!(
            reading.join().unwrap().as_deref(),
            Some(BEFORE),
            "{what}: the kept snapshot, its agent in the tab"
        );
        until("the read began", || held.began.load(Ordering::SeqCst) == 1);
        held.let_go();
        until("the read lands", || {
            kept_session(&idle).as_deref() == Some(NAMED)
        });
        until("no read is out", || !read_out(&idle));

        // Its agent left the tab: nothing is read until a read lands.
        let (idle, held) = gated_idle(Program::Claude, Duration::ZERO, transcribed);
        named_before(&idle);
        let at = Arc::clone(&idle);
        let reading = std::thread::spawn(move || read(&at));
        until(&format!("{what} returns while its read is held"), || {
            reading.is_finished()
        });
        assert_eq!(
            reading.join().unwrap(),
            None,
            "{what}: no read landed, and a kept snapshot whose agent left the tab is not read"
        );
        until("the read began", || held.began.load(Ordering::SeqCst) == 1);
        assert_eq!(read(&idle), None, "{what}: still out");
        assert_eq!(
            held.began.load(Ordering::SeqCst),
            1,
            "{what}: the second reading joined the read out and started none"
        );
        held.let_go();
        until("the read lands", || {
            kept_session(&idle).as_deref() == Some(NAMED)
        });
        until("no read is out", || !read_out(&idle));

        // CONTROL: a read back within the bound is waited for, and what it landed is read.
        let (idle, held) = gated_idle(Program::Claude, aterm_spec::wait::MUST_HAPPEN, transcribed);
        named_before(&idle);
        let at = Arc::clone(&idle);
        let reading = std::thread::spawn(move || read(&at));
        until("the read began", || held.began.load(Ordering::SeqCst) == 1);
        assert!(!reading.is_finished(), "{what}: the control waits its read");
        held.let_go();
        assert_eq!(
            reading.join().unwrap().as_deref(),
            Some(NAMED),
            "{what}: the control reads what its read landed"
        );
    }
}

#[test]
fn a_slow_relaunch_read_never_holds_the_step_after_an_act() {
    let stepping = |held: Option<Arc<GatedRead>>| {
        move |h: &mut Hooks, _: &Arc<GatedRead>| {
            h.acts.due = Arc::new(|_| Due::Yes);
            h.acts.step = Arc::new(move |_, _, _| {
                if let Some(held) = &held {
                    held.hold();
                }
                "current".to_string()
            });
        }
    };

    let (idle, held) = gated_idle(Program::Agy, Duration::ZERO, stepping(None));
    let at = Arc::clone(&idle);
    let point = std::thread::spawn(move || at.at_idle_on(IdlePoint::default()));
    until("the step returns while its reads are held", || {
        point.is_finished()
    });
    let step = point.join().unwrap().expect("the upgrade's step");
    assert_eq!(step.line, "upgrade step=current");
    until("the point's read began", || {
        held.began.load(Ordering::SeqCst) == 1
    });
    assert!(
        with_kept_of(&idle.kept, |k| k
            .point_read
            .as_ref()
            .is_some_and(|out| out.again.load(Ordering::SeqCst))),
        "the read after the act waits on the one out, followed, and started none beside it"
    );
    held.let_go();
    until(
        "a read begins after the one that began before the act",
        || held.began.load(Ordering::SeqCst) == 2,
    );
    until("no read is out", || !read_out(&idle));
    assert_eq!(kept_session(&idle).as_deref(), Some(NAMED));

    // CONTROL: the read after the act, back within the bound, is waited for and lands.
    let (idle, held) = gated_idle(
        Program::Agy,
        aterm_spec::wait::MUST_HAPPEN,
        move |h, held| {
            stepping(Some(Arc::clone(held)))(h, held);
            let read = Arc::clone(held);
            h.acts.snapshot = Arc::new(move |sid| {
                read.pass();
                let n = read.began.load(Ordering::SeqCst);
                Some(Snapshot {
                    session: Some(format!("read-{n}")),
                    ..snap(sid)
                })
            });
        },
    );
    let at = Arc::clone(&idle);
    let point = std::thread::spawn(move || at.at_idle_on(IdlePoint::default()));
    until("the point's read began", || {
        held.began.load(Ordering::SeqCst) == 1
    });
    held.let_go();
    until("the read after the act began", || {
        held.began.load(Ordering::SeqCst) == 2
    });
    assert!(
        !point.is_finished(),
        "the control waits its read after the act"
    );
    held.let_go();
    let step = point.join().unwrap().expect("the control's step");
    assert_eq!(step.line, "upgrade step=current");
    assert_eq!(
        kept_session(&idle).as_deref(),
        Some("read-2"),
        "the control's step returned with its read after the act landed"
    );

    // The restart in place, its act the agent's restart.
    let restarting = |h: &mut Hooks, _: &Arc<GatedRead>| {
        h.acts.restart = Arc::new(|_, _, _, _, _| RestartSaid::step("restarted"));
    };
    let (idle, held) = gated_idle(Program::Claude, Duration::ZERO, restarting);
    let at = Arc::clone(&idle);
    let restart = std::thread::spawn(move || at.restart_once(&Restart::Memory));
    until("the restart returns while its read is held", || {
        restart.is_finished()
    });
    assert_eq!(restart.join().unwrap().as_deref(), Some("restarted"));
    until("its read began", || held.began.load(Ordering::SeqCst) == 1);
    held.let_go();
    until("the read lands", || {
        kept_session(&idle).as_deref() == Some(NAMED)
    });

    // CONTROL: back within the bound, the restart returns with its read landed.
    let (idle, held) = gated_idle(Program::Claude, aterm_spec::wait::MUST_HAPPEN, restarting);
    let at = Arc::clone(&idle);
    let restart = std::thread::spawn(move || at.restart_once(&Restart::Memory));
    until("its read began", || held.began.load(Ordering::SeqCst) == 1);
    assert!(!restart.is_finished(), "the control waits its read");
    held.let_go();
    assert_eq!(restart.join().unwrap().as_deref(), Some("restarted"));
    assert_eq!(kept_session(&idle).as_deref(), Some(NAMED));
}

/// THE RESTART'S OWN REPORT IS ITS EVIDENCE WHILE THE KEPT SNAPSHOT LAGS IT
/// ([`IdleHost::restart_left`], ATERM-KI-1101-3): a restart in place whose report names the
/// process it started returns while the snapshot read after its act is still held, so the kept
/// snapshot still names the process before; the worker names the report's process as the one the
/// restart left. The next restart, answered with no process named, keeps none: never the process
/// an earlier restart named. Negative control by mutation: the report's pid dropped in
/// `WorkerIdle::said`: no process left is named, and the loop could go by the lagging snapshot
/// alone.
#[test]
fn a_restart_left_names_its_reports_process_while_the_snapshot_lags() {
    let (idle, held) = gated_idle(Program::Claude, Duration::ZERO, |h, _| {
        let asks = Arc::new(AtomicUsize::new(0));
        h.acts.restart = Arc::new(move |_, _, _, _, _| {
            if asks.fetch_add(1, Ordering::SeqCst) == 0 {
                RestartSaid {
                    step: "adopted".to_string(),
                    pid: Some(5151),
                    claim: RestartClaim::Unclaimed,
                }
            } else {
                RestartSaid::step("wait:resume")
            }
        });
    });
    let before = idle.agent_pid();
    assert!(before.is_some() && before != Some(5151), "{before:?}");
    assert_eq!(
        idle.restart_once(&Restart::Memory).as_deref(),
        Some("adopted")
    );
    assert_eq!(
        idle.restart_left(),
        Some((Some(5151), None)),
        "the report's process"
    );
    assert_eq!(idle.agent_pid(), before, "the kept snapshot lags it");
    assert_eq!(
        idle.restart_once(&Restart::Memory).as_deref(),
        Some("wait:resume")
    );
    assert_eq!(
        idle.restart_left(),
        None,
        "a report that names none keeps none"
    );
    held.let_go();
}

/// THE HOST ANSWERS THE CLAIM ITS RELAUNCH SAID (ATERM-KI-1101-3): the loop keeps a move's run
/// past its other acts, and dates the move, by what the host's answer read of the conversation's
/// record ([`RestartAnswer::claim`]), which the relaunch says once, from the record it left
/// (`relaunch::restart_as`); the worker passes each claim on as said, the same word
/// (`wait:resume`) said with each. Negative control by mutation: the claim dropped in
/// `WorkerIdle::said`: every answer reads no record, and the loop asks the move again as another
/// run, which restarts over the relaunch its ask landed.
#[test]
fn a_restart_answers_the_claim_its_relaunch_said() {
    let (idle, held) = gated_idle(Program::Claude, Duration::ZERO, |h, _| {
        let asks = Arc::new(AtomicUsize::new(0));
        h.acts.restart = Arc::new(move |_, _, _, _, _| RestartSaid {
            step: "wait:resume".to_string(),
            pid: None,
            claim: RestartClaim::ALL[asks.fetch_add(1, Ordering::SeqCst) % RestartClaim::ALL.len()],
        });
    });
    let fallback = Restart::Model {
        to: "claude-sonnet-5-5".to_string(),
    };
    let ask = RestartAsk::of(&fallback, 0);
    let answers: Vec<Option<RestartAnswer>> = (0..RestartClaim::ALL.len())
        .map(|_| IdleHost::restart(idle.as_ref(), &fallback, &ask))
        .collect();
    let said: Vec<Option<RestartAnswer>> = RestartClaim::ALL
        .into_iter()
        .map(|claim| {
            Some(RestartAnswer {
                step: "wait:resume".to_string(),
                claim,
            })
        })
        .collect();
    assert_eq!(answers, said, "each answer's claim as its relaunch said it");
    held.let_go();
}

#[test]
fn a_slow_landed_read_is_given_up_at_a_stop_or_once_unwanted() {
    let landing = || {
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        let held = Arc::new(GatedRead::default());
        h.acts.snapshot = held.act(NAMED);
        h.acts.point_read = Duration::from_millis(10);
        let (wanted, waiting) = (
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(true)),
        );
        let w = Arc::clone(&wanted);
        h.still_wanted = Arc::new(move |_, _| w.load(Ordering::SeqCst));
        let kept: Arc<Mutex<Kept>> = Arc::default();
        let (k, h, still) = (Arc::clone(&kept), Arc::new(h), Arc::clone(&waiting));
        let reading = std::thread::spawn(move || {
            read_landed("s-l", &k, &h, 0, &|| still.load(Ordering::SeqCst))
        });
        (reading, held, [wanted, waiting], kept)
    };
    let landed = |kept: &Mutex<Kept>| {
        with_kept_of(kept, |k| {
            k.snapshot.as_ref().and_then(|s| s.session.clone())
        })
    };

    // Unwanted, then a stop while the session is still wanted: either gives the read up.
    for (cut, what) in [(0, "the session is unwanted"), (1, "a stop came")] {
        let (reading, held, flags, kept) = landing();
        until("the read began", || held.began.load(Ordering::SeqCst) == 1);
        flags[cut].store(false, Ordering::SeqCst);
        until(&format!("the read is given up once {what}"), || {
            reading.is_finished()
        });
        assert!(!reading.join().unwrap(), "{what}: no landing read");
        held.let_go();
        until("the read lands as it ends", || {
            landed(&kept).as_deref() == Some(NAMED)
        });
    }

    // CONTROL: while the session is wanted the read is waited for, and its landing is the answer.
    let (reading, held, _flags, kept) = landing();
    until("the read began", || held.began.load(Ordering::SeqCst) == 1);
    assert!(!reading.is_finished(), "the control waits its read");
    held.let_go();
    assert!(reading.join().unwrap(), "the control's read landed");
    assert_eq!(landed(&kept).as_deref(), Some(NAMED));
}

#[test]
fn a_slow_read_never_holds_the_attach() {
    let attaching = |bound: Duration| {
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        let read = Arc::new(GatedRead::default());
        h.acts.snapshot = read.act(NAMED);
        let note = Arc::new(GatedRead::default());
        let n = Arc::clone(&note);
        h.acts.behind = Arc::new(move |_, _| {
            n.pass();
            Behind::Noted
        });
        h.acts.due = Arc::new(|_| Due::Yes);
        h.acts.point_read = bound;
        let job = Arc::new(left_job("s-y", Program::Agy, &on()));
        let (j, h) = (Arc::clone(&job), Arc::new(h));
        let attached = std::thread::spawn(move || attach(&j, &h));
        (attached, job, read, note)
    };
    let landed = |job: &WorkerJob| with_kept(job, |k| k.snapshot.clone()?.session);

    let (attached, job, read, note) = attaching(Duration::ZERO);
    until("attach returns while its reads are held", || {
        attached.is_finished()
    });
    attached.join().unwrap();
    assert!(
        job.park.load(Ordering::SeqCst),
        "its first idle point looks"
    );
    until("the note and the read began", || {
        note.began.load(Ordering::SeqCst) == 1 && read.began.load(Ordering::SeqCst) == 1
    });
    note.let_go();
    read.let_go();
    until("the note lands as it ends", || {
        job.note.lock().unwrap().is_none()
    });
    until("the read lands as it ends", || {
        landed(&job).as_deref() == Some(NAMED)
    });

    // CONTROL: back within the bound, attach waits for both, and returns with both landed.
    let (attached, job, read, note) = attaching(aterm_spec::wait::MUST_HAPPEN);
    until("the note began", || note.began.load(Ordering::SeqCst) == 1);
    assert!(!attached.is_finished(), "the control waits its note");
    note.let_go();
    until("the read began", || read.began.load(Ordering::SeqCst) == 1);
    assert!(!attached.is_finished(), "the control waits its read");
    read.let_go();
    attached.join().unwrap();
    assert!(job.note.lock().unwrap().is_none(), "noted: nothing owed");
    assert_eq!(landed(&job).as_deref(), Some(NAMED));
    assert!(
        job.park.load(Ordering::SeqCst),
        "the upgrade's look is owed"
    );
}

#[test]
fn a_read_begun_before_an_act_is_read_as_after_it_by_no_one() {
    let reading = |act: bool| {
        let acted = Arc::new(AtomicBool::new(false));
        let a = Arc::clone(&acted);
        let (idle, held) = gated_idle(
            Program::Claude,
            aterm_spec::wait::MUST_HAPPEN,
            move |h, held| {
                transcribed(h, held);
                let read = Arc::clone(held);
                h.acts.snapshot = Arc::new(move |sid| {
                    // What the record says as the read begins.
                    let session = if a.load(Ordering::SeqCst) {
                        NAMED
                    } else {
                        BEFORE
                    };
                    read.pass();
                    Some(Snapshot {
                        session: Some(session.to_string()),
                        ..snap(sid)
                    })
                });
            },
        );
        let first = start_point_read(&idle.sid, &idle.kept, &idle.hooks, None);
        until("the read before the act began", || {
            held.began.load(Ordering::SeqCst) == 1
        });
        if act {
            acted.store(true, Ordering::SeqCst);
            assert_eq!(
                reread(
                    &idle.sid,
                    &idle.kept,
                    &idle.hooks,
                    Some(Instant::now()),
                    Duration::ZERO
                ),
                Reread::Out,
                "the act's own read goes on past its bound"
            );
        }
        let at = Arc::clone(&idle);
        let reader = std::thread::spawn(move || at.transcript_tail());
        // The test's, the slot's, the read thread's, and the reader's.
        until("the reader joined the read out", || {
            Arc::strong_count(&first) >= 4 || reader.is_finished()
        });
        held.let_go();
        (reader.join().unwrap(), held.began.load(Ordering::SeqCst))
    };

    let (after, reads) = reading(true);
    assert_eq!(
        after,
        Some(NAMED.to_string()),
        "a reader after the act waits for a read begun after it"
    );
    // The queued follow-up can finish before the waiting reader resumes; that reader may
    // then start another fresh read. Either order must leave the old snapshot behind.
    assert!(reads >= 2, "the read begun before the act is not reused");
    // CONTROL: with no act since, the read out is joined, and what it landed is read.
    assert_eq!(reading(false), (Some(BEFORE.to_string()), 1));
}

#[test]
fn a_slow_read_after_an_account_move_never_holds_its_outcome() {
    let moving = |bound: Duration| {
        let a = Accts::new();
        a.spawn("s-acct-a", ACCT_TALK_A, "main");
        a.wall(ACCT_TALK_A, ACCT_NOW - 10);
        let mut idle = a.worker("s-acct-a", &on());
        let held = Arc::new(GatedRead::default());
        let armed = Arc::new(AtomicBool::new(false));
        let (read, gate, arm) = (
            Arc::clone(&idle.hooks.acts.snapshot),
            Arc::clone(&held),
            Arc::clone(&armed),
        );
        // Only the reads after the move's act are held.
        idle.hooks.acts.snapshot = Arc::new(move |sid| {
            if arm.load(Ordering::SeqCst) {
                gate.pass();
            }
            read(sid)
        });
        let (restart, arm) = (Arc::clone(&idle.hooks.acts.restart), Arc::clone(&armed));
        idle.hooks.acts.restart = Arc::new(
            move |sid: &str,
                  grace: u32,
                  why: &Restart,
                  ask: &aterm_agent::supervise::policy::turn_end::RestartAsk,
                  selected: Option<&relaunch::AccountRestart>| {
                let step = restart(sid, grace, why, ask, selected);
                arm.store(true, Ordering::SeqCst);
                step
            },
        );
        idle.hooks.acts.point_read = bound;
        assert_eq!(moves(&idle.account(usage_wall())), Some("work-2"));
        let idle = Arc::new(idle);
        let at = Arc::clone(&idle);
        let step = std::thread::spawn(move || at.restart_once(&move_to("work-2", AccountAt::Wall)));
        (a, idle, held, step)
    };

    let (_a, idle, held, step) = moving(Duration::ZERO);
    until(
        "the move returns while its read after the move is held",
        || step.is_finished(),
    );
    assert_eq!(
        step.join().unwrap().as_deref(),
        Some("wait:account-unverified"),
        "unread: nobody knows, and nothing is taken back"
    );
    until("the read after the move began", || {
        held.began.load(Ordering::SeqCst) == 1
    });
    assert!(
        with_kept_of(&idle.kept, |k| k.account.flying.is_some()),
        "the move is still in flight"
    );
    held.let_go();
    until("no read is out", || !read_out(&idle));
    assert_eq!(
        held.began.load(Ordering::SeqCst),
        1,
        "its outcome joined it"
    );

    // CONTROL: back within the bound, the move's outcome is what its read after the move landed.
    let (_a, idle, held, step) = moving(aterm_spec::wait::MUST_HAPPEN);
    until("the read after the move began", || {
        held.began.load(Ordering::SeqCst) == 1
    });
    assert!(!step.is_finished(), "the control waits its read");
    held.let_go();
    assert_eq!(step.join().unwrap().as_deref(), Some("adopted"));
    assert!(with_kept_of(&idle.kept, |k| k.account.flying.is_none()));
}

/// The rings ([`ring`]) the calling thread makes from now until it ends, sent as it ends.
fn rings_to_its_end() -> std::sync::mpsc::Receiver<u64> {
    struct Watch(u64, std::sync::mpsc::Sender<u64>);
    impl Drop for Watch {
        fn drop(&mut self) {
            let _ = self.1.send(RINGS_HERE.with(std::cell::Cell::get) - self.0);
        }
    }
    thread_local! {
        static WATCH: std::cell::RefCell<Option<Watch>> = const { std::cell::RefCell::new(None) };
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let at = RINGS_HERE.with(std::cell::Cell::get);
    WATCH.with(|w| *w.borrow_mut() = Some(Watch(at, tx)));
    rx
}

#[test]
fn a_note_behind_held_past_its_bound_holds_no_step_and_rings_as_it_lands() {
    type Rings = std::sync::mpsc::Receiver<std::sync::mpsc::Receiver<u64>>;
    type Asked = (Arc<GatedRead>, Rings, std::thread::JoinHandle<()>, Note);
    /// A site that asks the note behind on a worker's thread, begun with its bound and the word.
    type Site = fn(Duration, Behind) -> Asked;
    /// Hooks whose note behind the returned gate holds, then says `word`, waited for `bound` at
    /// most; each ask sends the rings its thread makes.
    fn noting(bound: Duration, word: Behind) -> (Hooks, Arc<GatedRead>, Rings) {
        let world = Arc::new(World::default());
        let mut h = hooks(&world, parking_body(&world));
        let note = Arc::new(GatedRead::default());
        let (rings, rung) = std::sync::mpsc::channel();
        let (n, rings) = (Arc::clone(&note), Mutex::new(rings));
        h.acts.behind = Arc::new(move |_, _| {
            let _ = rings.lock().unwrap().send(rings_to_its_end());
            n.pass();
            word
        });
        h.acts.due = Arc::new(|_| Due::Yes);
        h.acts.point_read = bound;
        (h, note, rung)
    }
    let sites: [(&str, Site); 2] = [
        ("attach", |bound, word| {
            let (h, note, rung) = noting(bound, word);
            let job = Arc::new(left_job("s-y", Program::Agy, &on()));
            let (j, owed) = (Arc::clone(&job), Arc::clone(&job.note));
            let asking = std::thread::spawn(move || attach(&j, &h));
            (note, rung, asking, owed)
        }),
        ("the idle step", |bound, word| {
            let (mut h, note, rung) = noting(bound, word);
            h.acts.due = Arc::new(|_| Due::No);
            let idle = Arc::new(idle_of(h, Program::Agy, Some(NAMED.to_string())));
            owe_note(&idle.note, crate::upgrade_host::now_s());
            let (at, owed) = (Arc::clone(&idle), Arc::clone(&idle.note));
            let asking = std::thread::spawn(move || {
                at.at_idle_on(IdlePoint::default());
            });
            (note, rung, asking, owed)
        }),
    ];
    for (site, ask) in sites {
        let (note, rung, asking, owed) = ask(Duration::ZERO, Behind::Busy);
        until(&format!("{site} returns while its note is held"), || {
            asking.is_finished()
        });
        asking.join().unwrap();
        let rings = rung
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the note asked");
        note.let_go();
        assert!(
            rings
                .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                .expect("the ask ended")
                >= 1,
            "{site}: the busy note, landing past the bound, rings the host to ask it again"
        );
        let o = owed.lock().unwrap().expect("busy: still owed");
        assert!(!o.flying && o.by.is_some(), "{site}: asked again later");

        // CONTROL: back within the bound, the note is waited for, and what it said is used.
        let (note, _rung, asking, owed) = ask(aterm_spec::wait::MUST_HAPPEN, Behind::Noted);
        until("the note began", || note.began.load(Ordering::SeqCst) == 1);
        assert!(!asking.is_finished(), "{site}: the control waits its note");
        note.let_go();
        asking.join().unwrap();
        assert!(
            owed.lock().unwrap().is_none(),
            "{site}: noted, nothing owed"
        );
    }
}

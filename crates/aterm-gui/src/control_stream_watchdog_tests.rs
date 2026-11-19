// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's harness watchdog on a record stream's lane (HX-15e), driven through the same rig as
//! the stream's own tests: a real claim on a real connection, the shipping lane counting its
//! claimant's beats on the rig's frozen awake clock, and the rig's world standing in for the
//! machine's wake, the kernel's stop state, the claimant's life and `quiet=`. The claimant is
//! the shared stand-in, which the rig's world reads as gone, so nothing is ever signalled; no
//! harness state directory is at hand, so the beat restart is told and not watched.

use alab_wire::ChildProcess;
use alab_wire::record::State;
use aterm_uds::process::{Birth, Ended, Identity};

use super::*;
use crate::harness_watchdog::tests::Kernel;
use crate::harness_watchdog::{Act, Ending, Hand, MISSED_AFTER_MS, Why, end_harness};
use crate::session_timeline::StreamedState;

/// The awake clock at the last `beat` every test sends first.
const BEAT: u64 = 1_000;

fn working() -> Body {
    Body::State {
        state: State::Working,
        composer: None,
    }
}

/// `sid`'s published agent word.
fn word(rig: &Rig, sid: &str) -> &'static str {
    rig.service
        .handle(sid)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .agent()
        .word
}

/// Wait until `sid`'s agent word reads `want` (a hang detector).
fn reads(rig: &Rig, sid: &str, want: &str) {
    aterm_spec::wait::until_within(SETTLE, &format!("{sid} reads {want}"), || {
        (word(rig, sid) == want).then_some(())
    });
}

/// Set the awake clock to `ms` and wake `sid`'s stream lane, as its own due look does.
fn at(rig: &Rig, sid: &str, ms: u64) {
    rig.service.clock.store(ms, Ordering::Release);
    rig.service.handle(sid).ctx.watchers.ring_stream();
}

/// Send titles `seq` and `seq + 1` and read both acknowledgements: the lane's look after the
/// first is done before it reads the second.
fn settle(stream: &mut Client, seq: u64) {
    assert_eq!(stream.record(seq, title()), (seq, None));
    assert_eq!(stream.record(seq + 1, title()), (seq + 1, None));
}

/// How many `restart cause=beat` rows `sid`'s timeline holds.
fn beat_restarts(rig: &Rig, sid: &str) -> usize {
    rig.service
        .handle(sid)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .view_copy(None, 0)
        .rows()
        .iter()
        .filter(|row| {
            row.kind == crate::restart_act::RESTART_EVENT && &*row.payload == "cause=beat"
        })
        .count()
}

/// How many `mark agent word=<word> src=aterm` rows `sid`'s log holds.
fn aterm_marks(rig: &Rig, sid: &str, word: &str) -> usize {
    let handle = rig.service.handle(sid);
    let timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    timeline
        .slog()
        .log()
        .window()
        .filter(|r| {
            r.k == aterm_slog::Kind::Agent
                && r.src == aterm_slog::Src::Aterm
                && r.f.get_word(aterm_slog::Key::Word) == Some(word)
        })
        .count()
}

/// The cause of aterm's next `restart` record on `stream`, its other records passed over.
fn heard_restart(stream: &mut Client) -> String {
    loop {
        if let Body::Restart { cause, .. } = stream.own().body {
            return cause;
        }
    }
}

/// Whether `stream` heard a `restart` record of aterm's.
fn heard_any_restart(stream: &Client) -> bool {
    stream
        .heard
        .iter()
        .any(|record| matches!(record.body, Body::Restart { .. }))
}

/// A stream of `sid` open, its state `working` and its last `beat` at [`BEAT`] (records 1, 2).
fn beating(rig: &Rig, sid: &str) -> Client {
    let mut stream = rig.claimant(sid, "state");
    stream.open(sid);
    assert_eq!(stream.record(1, working()), (1, None));
    assert_eq!(stream.record(2, Body::Beat), (2, None));
    assert_eq!(word(rig, sid), "busy");
    stream
}

/// HX-15e EXIT TEST: THREE MISSED BEATS READ UNKNOWN AND RESTART (D16). A stream's claimant beats,
/// then sends no `beat` for `BEATS_MISSED × BEAT_EVERY` of awake time: the tab reads `unknown`,
/// written by aterm (`mark agent word=unknown src=aterm`), over the state the stream fed, and the
/// watchdog writes `restart cause=beat`, which the open stream carries as a record. One restart
/// is written for one miss. A `beat` hands the word back to the stream's state and starts the
/// count again: the next miss is restarted again.
/// NEGATIVE HALF: one millisecond short of the three beat periods nothing is read or written.
#[test]
fn three_missed_beats_read_unknown_and_restart() {
    let rig = Rig::new(2);
    let mut stream = beating(&rig, ONE);

    rig.service
        .clock
        .store(BEAT + MISSED_AFTER_MS - 1, Ordering::Release);
    settle(&mut stream, 3);
    assert_eq!(word(&rig, ONE), "busy");
    assert_eq!(beat_restarts(&rig, ONE), 0);
    assert!(!heard_any_restart(&stream));

    at(&rig, ONE, BEAT + MISSED_AFTER_MS);
    reads(&rig, ONE, "unknown");
    assert_eq!(heard_restart(&mut stream), "beat");
    assert_eq!(beat_restarts(&rig, ONE), 1);
    assert_eq!(aterm_marks(&rig, ONE, "unknown"), 1, "written by aterm");

    // Still no beat: one restart for the one miss.
    at(&rig, ONE, BEAT + 2 * MISSED_AFTER_MS);
    settle(&mut stream, 5);
    assert_eq!(word(&rig, ONE), "unknown");
    assert_eq!(beat_restarts(&rig, ONE), 1);

    // A beat: the stream's state again, and the count starts again from it.
    let beat = BEAT + 2 * MISSED_AFTER_MS;
    assert_eq!(stream.record(7, Body::Beat), (7, None));
    reads(&rig, ONE, "busy");
    at(&rig, ONE, beat + MISSED_AFTER_MS);
    reads(&rig, ONE, "unknown");
    assert_eq!(heard_restart(&mut stream), "beat");
    assert_eq!(beat_restarts(&rig, ONE), 2);
}

/// HX-15e EXIT TEST: SLEEP IS NOT A MISSED BEAT (D16). The count runs on the awake clock, which
/// stands still while the Mac sleeps, and `Power::Woke` starts it again: a harness whose last
/// `beat` came 30 s of awake time before, but that the machine's wake interrupted 10 s ago, has
/// missed nothing; the count from the wake runs out only three beat periods after it. A wake
/// that comes after a miss starts the count again too: the tab reads the stream's state again.
/// NEGATIVE HALF: three beat periods after the wake with no `beat` is a miss, restarted.
#[test]
fn sleep_is_not_a_missed_beat() {
    let rig = Rig::new(2);
    let mut stream = beating(&rig, ONE);
    let woke = BEAT + MISSED_AFTER_MS - 10_000;
    rig.service.woke.store(woke + 1, Ordering::Release);

    at(&rig, ONE, BEAT + MISSED_AFTER_MS);
    settle(&mut stream, 3);
    assert_eq!(word(&rig, ONE), "busy", "the wake started the count again");
    assert_eq!(beat_restarts(&rig, ONE), 0);

    rig.service
        .clock
        .store(woke + MISSED_AFTER_MS - 1, Ordering::Release);
    settle(&mut stream, 5);
    assert_eq!(word(&rig, ONE), "busy");

    at(&rig, ONE, woke + MISSED_AFTER_MS);
    reads(&rig, ONE, "unknown");
    assert_eq!(heard_restart(&mut stream), "beat");

    // The machine sleeps and wakes again: the count starts from this wake, and the miss ends.
    let again = woke + MISSED_AFTER_MS + 5_000;
    rig.service.woke.store(again + 1, Ordering::Release);
    at(&rig, ONE, again + 1);
    reads(&rig, ONE, "busy");
    assert_eq!(beat_restarts(&rig, ONE), 1);
}

/// HX-15e EXIT TEST: A STOPPED CLAIMANT READS STOPPED AND IS NEVER RESTARTED (D16). A claimant
/// the kernel reports stopped (`SIGSTOP`/`SIGTSTP`, `pid_stopped` on its own pid) sends no
/// beats: the tab reads `stopped`, written by aterm, however long it stays stopped, and no
/// restart is ever written for those beats. Once it runs again its count starts from then.
/// NEGATIVE HALF: three beat periods after it resumed with no `beat`, it reads `unknown` and is
/// restarted.
#[test]
fn a_stopped_claimant_reads_stopped_and_is_never_restarted() {
    let rig = Rig::new(2);
    let mut stream = beating(&rig, ONE);
    *rig.service
        .stopped
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(true);

    at(&rig, ONE, BEAT + MISSED_AFTER_MS);
    reads(&rig, ONE, "stopped");
    assert_eq!(aterm_marks(&rig, ONE, "stopped"), 1, "written by aterm");
    let resumed = BEAT + 10 * MISSED_AFTER_MS;
    at(&rig, ONE, resumed);
    settle(&mut stream, 3);
    settle(&mut stream, 5);
    assert_eq!(word(&rig, ONE), "stopped");
    assert_eq!(beat_restarts(&rig, ONE), 0);
    assert!(!heard_any_restart(&stream));

    *rig.service
        .stopped
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(false);
    reads(&rig, ONE, "busy");
    rig.service
        .clock
        .store(resumed + MISSED_AFTER_MS - 1, Ordering::Release);
    settle(&mut stream, 7);
    assert_eq!(word(&rig, ONE), "busy");
    assert_eq!(beat_restarts(&rig, ONE), 0);

    at(&rig, ONE, resumed + MISSED_AFTER_MS);
    reads(&rig, ONE, "unknown");
    assert_eq!(heard_restart(&mut stream), "beat");
}

/// HX-15e EXIT TEST: A CLOSED STREAM READS UNKNOWN THROUGH THE GRACE AND THE WATCHDOG ACTS AFTER
/// (D16, D28). A stream that fed the tab's state closes and its contract 4 claim is kept through
/// the re-exec grace: what the harness does is unknown, so the tab reads `unknown` (written by
/// aterm), and its readers stay down (the state stays fed; the screen's reading is not shown).
/// At the grace's end the lapse hands the claim to the watchdog and the screen's word comes
/// back; the claimant is gone, so the watchdog ends the child the claim kept, by the identity
/// aterm keeps, and the tab reads `unsupervised`.
/// NEGATIVE HALF: the same claimant claiming again inside the grace ends the `unknown`: the
/// stream's state decides again.
#[test]
fn a_closed_stream_reads_unknown_through_the_grace_and_the_watchdog_acts_after() {
    let rig = Rig::new(2);
    let handle = rig.service.handle(ONE);
    let ctx = Arc::clone(&handle.ctx);
    let holder = format!("claude-code-alab@{}", running().pid);
    let child = ChildProcess {
        pid: 4243,
        start: 1_792_000_000_000_000,
    };
    let screen = |word: &'static str| {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_agent(
                word,
                None,
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq },
                    fp: seq,
                },
            )
    };
    let fed = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .agent()
            .stream
            .as_ref()
            .map(|fed| fed.state)
    };
    let close = |client: Client, id: u64| {
        drop(client);
        rig.ended(id);
    };

    let mut first = rig.claimant(ONE, "state");
    let id = first.id();
    first.open(ONE);
    let kept = Body::Claim {
        child: Some(child),
        scope: None,
        version: None,
        vendor: None,
        login: None,
    };
    assert_eq!(first.record(1, kept), (1, None));
    assert_eq!(first.record(2, working()), (2, None));
    assert_eq!(word(&rig, ONE), "busy");
    close(first, id);
    reads(&rig, ONE, "unknown");
    assert_eq!(fed(), Some(StreamedState::Working), "the readers stay down");
    assert!(!screen("idle"), "the screen's reading is not shown");
    assert_eq!(aterm_marks(&rig, ONE, "unknown"), 1, "written by aterm");

    // NEGATIVE: the claimant claims again inside the grace; the stream's state decides again.
    let mut again = rig.claimant(ONE, "state");
    let again_id = again.id();
    again.open(ONE);
    assert_eq!(word(&rig, ONE), "busy");
    assert_eq!(
        again.record(
            3,
            Body::Claim {
                child: Some(child),
                scope: None,
                version: None,
                vendor: None,
                login: None,
            }
        ),
        (3, None)
    );
    close(again, again_id);
    reads(&rig, ONE, "unknown");

    // The grace's end: the lapse hands the contract 4 claim on, and the screen feeds the tab.
    let expiry = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .supervisor_expiry()
        .expect("the grace");
    assert!(
        crate::session_timeline::publish_lapse_supervisor(&ctx, expiry - 1)
            .published()
            .is_none()
    );
    assert_eq!(word(&rig, ONE), "unknown");
    let lapsed = crate::session_timeline::publish_lapse_supervisor(&ctx, expiry)
        .published()
        .expect("the claim lapsed");
    assert_eq!(lapsed.holder, holder);
    let scoped = lapsed.universal.expect("a contract 4 claim");
    let kept = scoped.child.expect("the kept child");
    assert_eq!((kept.pid, kept.birth), (child.pid, Birth(child.start)));
    assert_eq!(fed(), None, "the stand-down ended with the claim");
    assert_eq!(word(&rig, ONE), "idle", "the screen's word");

    // The watchdog acts: its claimant gone, the kept child is ended by its identity.
    let mut kernel = Kernel::reading(child.pid, child.start);
    assert_eq!(
        end_harness(
            &Arc::downgrade(&ctx),
            &holder,
            &scoped,
            Why::Grace,
            rig.service.as_ref(),
            &mut kernel,
        ),
        Ending::Ended {
            child: Some(Ok(Ended {
                termed: true,
                killed: false,
                gone: true,
            }))
        }
    );
    assert_eq!(
        kernel.sent,
        vec![(
            Identity {
                pid: i32::try_from(child.pid).expect("a pid"),
                birth: Birth(child.start),
                idversion: None,
            },
            libc::SIGTERM
        )]
    );
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .unsupervised
            .is_some(),
        "the tab reads unsupervised"
    );
}

/// HX-15e EXIT TEST: A WATCHDOG ACT WAITS FOR A HOLD, ANOTHER HAND AND QUIET (D16). Beats are
/// missed while a person holds the tab, another driver holds its lease and a person typed into
/// it within `quiet=`: the tab reads `unknown`, and the restart waits out each in turn (the hold,
/// then the lease, then the person's input), written only once nothing holds the tab. The
/// watchdog's end of a kept child waits for a hold the same way, and nothing is signalled while
/// it stands.
#[test]
fn a_watchdog_act_waits_for_a_hold_another_hand_and_quiet() {
    let rig = Rig::new(2);
    let handle = rig.service.handle(ONE);
    let ctx = Arc::clone(&handle.ctx);
    let waited = || {
        rig.service
            .waited
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    };
    let waits_for = |want: Vec<(Act, Hand)>| {
        aterm_spec::wait::until_within(SETTLE, &format!("the watchdog waits: {want:?}"), || {
            (waited() == want).then_some(())
        });
    };
    let hold = |on: bool| {
        crate::fabric::apply_hold_for_test(
            ctx.as_ref(),
            on.then(|| crate::fabric::Hold {
                reason: "test".to_string(),
                origin: "local".to_string(),
            }),
        );
    };
    let mut stream = beating(&rig, ONE);
    hold(true);
    *ctx.turn_lease
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(crate::Lease::Drive {
        holder: "lab".to_string(),
        expires_us: u64::MAX,
        conn: None,
        hard: false,
        here: false,
        holder_pid: None,
    });
    crate::app_input::note_person(&ctx, crate::human_input::Gesture::Typed);
    rig.service.quiet_ms.store(600_000, Ordering::Release);

    at(&rig, ONE, BEAT + MISSED_AFTER_MS);
    reads(&rig, ONE, "unknown");
    waits_for(vec![(Act::Restart, Hand::Hold)]);
    hold(false);
    waits_for(vec![
        (Act::Restart, Hand::Hold),
        (Act::Restart, Hand::Another),
    ]);
    *ctx.turn_lease
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
    waits_for(vec![
        (Act::Restart, Hand::Hold),
        (Act::Restart, Hand::Another),
        (Act::Restart, Hand::Person),
    ]);
    assert_eq!(
        beat_restarts(&rig, ONE),
        0,
        "nothing written while a hand held the tab"
    );
    rig.service.quiet_ms.store(0, Ordering::Release);
    assert_eq!(heard_restart(&mut stream), "beat");
    assert_eq!(beat_restarts(&rig, ONE), 1);

    // The end of a kept child waits for a hold too.
    let child = ChildProcess {
        pid: 4243,
        start: 1_792_000_000_000_000,
    };
    assert_eq!(
        stream.record(
            3,
            Body::Claim {
                child: Some(child),
                scope: None,
                version: None,
                vendor: None,
                login: None,
            }
        ),
        (3, None)
    );
    let scoped = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .live_scoped(crate::metrics::now_us())
        .cloned()
        .expect("the claim");
    let holder = format!("claude-code-alab@{}", running().pid);
    rig.service
        .waited
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    hold(true);
    let weak = Arc::downgrade(&ctx);
    let world = rig.service.as_ref();
    std::thread::scope(|scope| {
        let ending = scope.spawn(|| {
            let mut kernel = Kernel::reading(child.pid, child.start);
            let ending = end_harness(&weak, &holder, &scoped, Why::Unobeyed, world, &mut kernel);
            (ending, kernel.sent)
        });
        waits_for(vec![(Act::Child, Hand::Hold)]);
        assert!(!ending.is_finished(), "the end waits while the hold stands");
        hold(false);
        let (ending, sent) = ending.join().expect("the ending");
        assert!(matches!(ending, Ending::Ended { child: Some(Ok(_)) }));
        assert_eq!(
            sent.iter().map(|(_, sig)| *sig).collect::<Vec<_>>(),
            vec![libc::SIGTERM]
        );
    });
}

/// REVIEW OF HX-15e: A CLAIMANT STOPPED AFTER ITS BEAT RESTART READS STOPPED (D16). Beats are
/// missed and the restart is written; then the kernel reports the claimant stopped, with no
/// record and no wake on its stream: the lane keeps looking, and the tab reads `stopped`, written
/// by aterm, and the stream's state once it runs again. The one miss is restarted once.
/// NEGATIVE HALF: before the fix the lane stopped looking once the restart was written, and the
/// tab read `unknown` until a record came.
#[test]
fn a_claimant_stopped_after_its_beat_restart_reads_stopped() {
    let rig = Rig::new(2);
    let mut stream = beating(&rig, ONE);
    at(&rig, ONE, BEAT + MISSED_AFTER_MS);
    reads(&rig, ONE, "unknown");
    assert_eq!(heard_restart(&mut stream), "beat");

    *rig.service
        .stopped
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(true);
    reads(&rig, ONE, "stopped");
    assert_eq!(aterm_marks(&rig, ONE, "stopped"), 1, "written by aterm");
    *rig.service
        .stopped
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(false);
    reads(&rig, ONE, "busy");
    assert_eq!(beat_restarts(&rig, ONE), 1);
}

/// REVIEW OF HX-15e: A CLOSED STREAM THAT FED ANY KIND READS UNKNOWN THROUGH THE GRACE (the plan's
/// "a stream tab reads unknown ...; a tab with no applied kinds keeps its screen reader"). A
/// contract 4 stream granted `walls` alone applies a `usage` record and closes: through the
/// re-exec grace the tab reads `unknown`, written by aterm, over the screen's word.
/// NEGATIVE HALF: a stream that applied no kind a reader stands down for (only `beat`s) keeps the
/// screen's word through its grace. Before the fix only a stream that had applied `state` read `unknown`.
#[test]
fn a_closed_stream_that_fed_any_kind_reads_unknown_through_the_grace() {
    let rig = Rig::new(2);
    let screen = |sid: &str| {
        let _ = rig
            .service
            .handle(sid)
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_agent(
                "idle",
                None,
                None,
                Some(aterm_phase::Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        assert_eq!(word(&rig, sid), "idle");
    };
    let in_grace = |sid: &str| {
        rig.service
            .handle(sid)
            .ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor_expiry()
            .is_some()
    };

    screen(ONE);
    let mut walls = rig.dial("owner");
    assert_eq!(
        walls.request(&format!("claim {ONE} 4 walls login=main")),
        "OK claim"
    );
    let id = walls.id();
    walls.open(ONE);
    let usage = Body::Usage {
        account: "main".to_string(),
        limits: vec![alab_wire::record::Limit {
            name: "session".to_string(),
            used: Some(40),
            resets_ms: None,
        }],
        context_left: Some(70),
    };
    assert_eq!(walls.record(1, usage), (1, None));
    assert_eq!(
        rig.state(ONE).applied.iter().copied().collect::<Vec<_>>(),
        ["usage"]
    );
    assert_eq!(
        word(&rig, ONE),
        "idle",
        "a usage feed leaves the state word"
    );
    drop(walls);
    rig.ended(id);
    assert!(in_grace(ONE));
    reads(&rig, ONE, "unknown");
    assert_eq!(aterm_marks(&rig, ONE, "unknown"), 1, "written by aterm");

    // NEGATIVE: a stream that applied nothing keeps the screen's word through its grace.
    screen(TWO);
    let mut silent = rig.claimant(TWO, "state");
    let id = silent.id();
    silent.open(TWO);
    assert_eq!(silent.record(1, Body::Beat), (1, None));
    assert_eq!(silent.record(2, Body::Beat), (2, None));
    // Beats are taken, and stand no reader of the tab down.
    assert_eq!(
        rig.state(TWO).applied.iter().copied().collect::<Vec<_>>(),
        ["beat"]
    );
    drop(silent);
    rig.ended(id);
    assert!(in_grace(TWO));
    assert_eq!(word(&rig, TWO), "idle");
    assert_eq!(aterm_marks(&rig, TWO, "unknown"), 0);
}

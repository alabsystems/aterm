// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PROVEN SELF-CLAIM'S LANDING REOPENS AT A TURN BEGUN AFTER IT
//! (Autopilot Land, `App::reopen_self_claim_landing`), however short the
//! turn and whenever the status sweep runs, and never at a report that begins
//! no turn after it (the landing turn's own, one written again after a
//! handoff, work running on through the landing). What the program's reports
//! cannot tell (its MCP servers' start, a turn begun under a standing
//! `working`) is the reopen's doc's. Each test drives the real path: codex-alab's OSC 7501 bytes into
//! the session's real engine, its `meta set harness_act landed@v1` through the
//! real `meta` arm (the wakes applied as the app loop applies them), and the
//! real status sweep (`App::observe_session_statuses`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_core::terminal::Terminal;

use crate::App;

/// The bytes codex-alab writes (`terminal_program_status.rs`'s
/// `SetTerminalProgramStatus` and `ClearTerminalProgramStatus`).
const IDLE: &[u8] = b"\x1b]7501;state=idle:app=codex\x1b\\";
const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";

/// One tab under Land whose `codex` holds a proven self-claim, bound to
/// connection 1, beside a host that supervises nothing.
struct Tab {
    app: App,
    host: crate::harness_host::HostHandle,
    session: u64,
    term: Arc<Mutex<Terminal>>,
    ctx: Arc<crate::SessionCtx>,
    /// The sweep's clock: each sweep is past every observation interval.
    t: Instant,
}

impl Tab {
    fn new() -> Self {
        let mut app = App::headless_for_test();
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let session = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(session));
        let (term, ctx) = {
            let pooled = app.pool.get(session).expect("pooled");
            (Arc::clone(&pooled.term), Arc::clone(&pooled.ctx))
        };
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(4242);
            tl.set_program(4242, Some("codex".into()));
        }
        ctx.meta
            .lock()
            .unwrap()
            .set("autopilot", Some("land".into()));
        crate::session_timeline::claim_supervisor_as(
            &ctx,
            &format!("codex-alab@{}", std::process::id()),
            Some(1),
            None,
            Some(crate::self_claim::stand_in::running()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("a proof takes a free session");
        assert!(app.beside_proven_self_claim(session));
        Self {
            app,
            host,
            session,
            term,
            ctx,
            t: Instant::now(),
        }
    }

    /// codex-alab writes `bytes` to its terminal.
    fn says(&self, bytes: &[u8]) {
        crate::term_lock(&self.term).process(bytes);
    }

    /// One status sweep.
    fn sweep(&mut self) {
        let _ = self.app.observe_session_statuses(self.t);
        self.t += Duration::from_secs(1);
    }

    /// The claim's own connection serves `meta set harness_act <act>`: the
    /// wakes the arm posts, not yet applied.
    fn act(&self, act: &str) -> Vec<crate::Wake> {
        let (reply, wakes) = crate::control::meta_wakes_for_test(
            &self.app.store,
            &self.term,
            self.session,
            &self.ctx,
            &format!("set harness_act {act}"),
            Some(1),
        );
        assert_eq!(reply, "OK\n", "{act}");
        wakes
    }

    /// The app loop's arms for the wakes the `meta` arm posts.
    fn apply(&mut self, wakes: Vec<crate::Wake>) {
        for wake in wakes {
            match wake {
                crate::Wake::MetaChanged { session } => self.app.on_meta_changed(session),
                crate::Wake::HarnessFact { sid, fact } => self.app.apply_harness_fact(&sid, fact),
                other => panic!("the meta arm posts no {other:?}"),
            }
        }
    }

    /// codex-alab records its landing, applied at once.
    fn lands(&mut self) {
        let wakes = self.act("landed@v1");
        self.apply(wakes);
        assert_eq!(self.face(), Some(TaskFace::Landed), "the landing shows");
    }

    /// The task's face, as `status task=` reads it.
    fn face(&self) -> Option<TaskFace> {
        self.ctx
            .timeline
            .lock()
            .unwrap()
            .task_face()
            .map(|(f, _)| f)
    }

    fn done(self) {
        self.host.shutdown_and_join();
    }
}

/// THE GAP (`docs/measured/codex-alab-proof-2026-10-20.md`, "Reopen needs a
/// turn the status sweep sees"): after codex-alab's landing, a person's turn
/// that the model answers at once — `working` then `done`, both written
/// between two sweeps — reopens the landing at the next sweep, though no
/// sweep ever read the turn running and the report it reads says `done`, as
/// it did before. NEGATIVE CONTROL: a sweep with no turn since leaves it
/// landed.
#[test]
fn a_turn_begun_and_ended_between_two_sweeps_reopens_the_landing() {
    let mut tab = Tab::new();
    // The land turn, each report swept.
    for bytes in [IDLE, WORKING, DONE] {
        tab.says(bytes);
        tab.sweep();
    }
    tab.lands();
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Landed), "no turn since: landed");
    // A person's turn, answered at once: both reports between two sweeps.
    tab.says(WORKING);
    tab.says(DONE);
    tab.sweep();
    assert_eq!(
        tab.face(),
        Some(TaskFace::Open),
        "a turn begun after the landing reopens it, though no sweep saw it run"
    );
    tab.done();
}

/// THE LANDING TURN'S OWN REPORTS ARE NOT A TURN AFTER IT — A MUTANT GUARD
/// (3a43d9bf20, which reopened only at "running now", passes it too):
/// codex-alab's land turn runs and ends (`working`, `done`) with no sweep
/// between it and the landing — a short turn, a background tab's slow sweep —
/// so the sweep after the landing is the first to read the count those
/// reports moved. The landing was marked with the engine's count as the act
/// was served, so it stays landed; a mark taken from the sweep's last reading
/// would have reopened it on its own turn. CONTROL: the next turn reopens it.
/// [`the_landing_turns_working_first_read_after_it_reopens_nothing`] is the
/// case the reopen before the count failed.
#[test]
fn the_landing_turns_own_reports_read_after_it_reopen_nothing() {
    let mut tab = Tab::new();
    tab.says(IDLE);
    tab.sweep();
    // The land turn, unswept.
    tab.says(WORKING);
    tab.says(DONE);
    tab.lands();
    tab.sweep();
    assert_eq!(
        tab.face(),
        Some(TaskFace::Landed),
        "the landing turn's own reports, read after it, are no turn after it"
    );
    tab.says(WORKING);
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Open), "CONTROL: the next turn");
    tab.done();
}

/// THE LANDING TURN'S `working`, FIRST READ AFTER THE LANDING, IS NO TURN
/// AFTER IT: the land turn begins with no sweep between its `working` and the
/// landing, and that report still stands as the landing is recorded
/// (codex-alab reports `working` while background work the turn started runs
/// on past its end), so the sweep after the landing is the first to read it.
/// The tab stays landed, where a reopen at "running now" (3a43d9bf20's: the
/// shown report moved to `working`) and a mark taken from the sweep's last
/// reading both reopened the landing on its own turn. CONTROL: the work's end
/// (`idle`) keeps it landed, and the turn after that reopens it.
#[test]
fn the_landing_turns_working_first_read_after_it_reopens_nothing() {
    let mut tab = Tab::new();
    tab.says(IDLE);
    tab.sweep();
    // The land turn, unswept, its `working` standing through the landing.
    tab.says(WORKING);
    tab.lands();
    tab.sweep();
    assert_eq!(
        tab.face(),
        Some(TaskFace::Landed),
        "the landing turn's `working`, first read after it, is no turn after it"
    );
    tab.says(IDLE);
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Landed), "the work's end");
    tab.says(WORKING);
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Open), "CONTROL: a turn begun");
    tab.done();
}

/// A TURN BEGUN WHILE THE LANDING IS ADMITTED IS A TURN AFTER IT: codex-alab
/// has decided its landing and sent `landed@v1`, and while aterm admits the
/// act a person's turn begins and its `working` is parsed. Here the
/// admission's own work is the kernel's look at a request on a connection
/// the claim is not bound to (`prove`); on the claim's own connection it is
/// the act's commit under the session's locks. The landing's mark is read
/// before the act is admitted, so that turn is past it and the next sweep
/// reopens the landing; a mark read after the commit held the turn, and the
/// tab read `landed` through it. NEGATIVE CONTROL: the same admission with no
/// turn in it stays landed.
#[test]
fn a_turn_begun_while_the_landing_is_admitted_reopens_it() {
    for turn in [true, false] {
        let mut tab = Tab::new();
        for bytes in [IDLE, WORKING, DONE] {
            tab.says(bytes);
            tab.sweep();
        }
        let term = Arc::clone(&tab.term);
        let proven = crate::self_claim::stand_in::running();
        let prove = move |_: &str| {
            if turn {
                crate::term_lock(&term).process(WORKING);
            }
            Some(proven.clone())
        };
        let (reply, wakes) = crate::control::meta_wakes_proving_for_test(
            &tab.app.store,
            &tab.term,
            tab.session,
            &tab.ctx,
            "set harness_act landed@v1",
            Some(2),
            &prove,
        );
        assert_eq!(reply, "OK\n", "admitted by the kernel's look");
        tab.apply(wakes);
        assert_eq!(tab.face(), Some(TaskFace::Landed), "the landing shows");
        tab.sweep();
        let want = if turn {
            TaskFace::Open
        } else {
            TaskFace::Landed
        };
        assert_eq!(
            tab.face(),
            Some(want),
            "a turn begun in the admission: {turn}"
        );
        tab.done();
    }
}

/// THE SCREEN'S COUNT IS READ BEFORE THE ADMISSION TOO: a read of the screen
/// that finds the agent busy after a read that did not, made while the
/// landing's act is admitted, is a turn begun after the landing — reopened as
/// its face is applied (`App::reopen_self_claim_landing`), where a count read
/// after the commit held it. NEGATIVE CONTROL: the same admission with no busy
/// read in it stays landed.
#[test]
fn a_busy_read_while_the_landing_is_admitted_reopens_it() {
    let stamp = |seq: u64| crate::session_timeline::AgentStamp {
        generation: crate::control::ScreenGen { epoch: 1, seq },
        fp: seq,
    };
    for busy in [true, false] {
        let mut tab = Tab::new();
        tab.ctx.timeline.lock().unwrap().publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            stamp(1),
        );
        let ctx = Arc::clone(&tab.ctx);
        let proven = crate::self_claim::stand_in::running();
        let prove = move |_: &str| {
            if busy {
                ctx.timeline.lock().unwrap().publish_agent(
                    "busy",
                    Some("thinking".to_string()),
                    None,
                    Some(aterm_phase::Program::Claude),
                    stamp(2),
                );
            }
            Some(proven.clone())
        };
        let (reply, wakes) = crate::control::meta_wakes_proving_for_test(
            &tab.app.store,
            &tab.term,
            tab.session,
            &tab.ctx,
            "set harness_act landed@v1",
            Some(2),
            &prove,
        );
        assert_eq!(reply, "OK\n", "admitted by the kernel's look");
        tab.apply(wakes);
        tab.app.reopen_self_claim_landing(tab.session);
        let want = if busy {
            TaskFace::Open
        } else {
            TaskFace::Landed
        };
        assert_eq!(
            tab.face(),
            Some(want),
            "a busy read in the admission: {busy}"
        );
        tab.done();
    }
}

/// A REPORT WRITTEN AGAIN IS NO NEW TURN. codex-alab clears its report when it
/// lets the terminal go (an external editor, Ctrl+Z) and writes its state
/// again when it takes it back, and it reports `working` while background
/// work runs on after a turn's end. So: a landing at rest, then a handoff
/// (`clear`, `done` again), stays landed; a landing recorded while background
/// work keeps the report at `working` stays landed through the sweeps after
/// it, through a handoff (`clear`, `working` again) and through the work's end
/// (`idle`). CONTROL: the turn after that reopens it.
#[test]
fn a_report_written_again_after_a_handoff_reopens_nothing() {
    const CLEAR: &[u8] = b"\x1b]7501;state=clear\x1b\\";
    // At rest.
    let mut tab = Tab::new();
    for bytes in [IDLE, WORKING, DONE] {
        tab.says(bytes);
        tab.sweep();
    }
    tab.lands();
    for bytes in [CLEAR, DONE] {
        tab.says(bytes);
        tab.sweep();
        assert_eq!(tab.face(), Some(TaskFace::Landed), "a handoff at rest");
    }
    tab.done();
    // Background work running on through the landing.
    let mut tab = Tab::new();
    for bytes in [IDLE, WORKING] {
        tab.says(bytes);
        tab.sweep();
    }
    tab.lands();
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Landed), "work running on");
    for bytes in [CLEAR, WORKING, IDLE] {
        tab.says(bytes);
        tab.sweep();
        assert_eq!(
            tab.face(),
            Some(TaskFace::Landed),
            "a handoff mid-work, and the work's end: no turn begun"
        );
    }
    tab.says(WORKING);
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Open), "CONTROL: a turn begun");
    tab.done();
}

/// A TURN BEGUN BETWEEN THE ACT AND ITS FACE: the `meta` arm has served
/// `landed@v1` (the landing marked) but the app loop has not yet applied its
/// wakes when a turn runs and a sweep counts it. That sweep found no landing
/// to reopen, and no later sweep sees the count move again, so the landing is
/// reopened as its face is applied — recorded once, then open. NEGATIVE
/// CONTROL: the same with no turn between stays landed.
#[test]
fn a_turn_counted_before_the_landings_face_reopens_it_as_the_face_lands() {
    let records = |tab: &Tab| {
        let sid = tab.ctx.self_id.as_str();
        let key = format!("harness.landing.{sid}");
        tab.app
            .messages
            .log()
            .records()
            .filter(|r| r.key.as_deref() == Some(key.as_str()))
            .count()
    };
    for turn in [true, false] {
        let mut tab = Tab::new();
        for bytes in [IDLE, WORKING, DONE] {
            tab.says(bytes);
            tab.sweep();
        }
        let wakes = tab.act("landed@v1");
        if turn {
            tab.says(WORKING);
            tab.says(DONE);
        }
        tab.sweep();
        tab.apply(wakes);
        let want = if turn {
            TaskFace::Open
        } else {
            TaskFace::Landed
        };
        assert_eq!(tab.face(), Some(want), "turn between: {turn}");
        assert_eq!(records(&tab), 1, "the landing is recorded either way");
        tab.done();
    }
    // Two RPC lanes commit Landed then Open but can post their wakes in
    // reverse order. Both orders end Open and keep the valid landing's one
    // record; merely dropping a stale Landed hint would lose that record.
    for reversed in [false, true] {
        let mut tab = Tab::new();
        for bytes in [IDLE, WORKING, DONE] {
            tab.says(bytes);
            tab.sweep();
        }
        let landing = tab.act("landed@v1");
        let open = tab.act("continue@v1");
        if reversed {
            tab.apply(open);
            tab.apply(landing);
        } else {
            tab.apply(landing);
            tab.apply(open);
        }
        assert_eq!(
            tab.face(),
            Some(TaskFace::Open),
            "reversed wakes: {reversed}"
        );
        assert_eq!(records(&tab), 1, "a valid landing is recorded once");
        tab.done();
    }
    // A same-proven renewal keeps committed faces. A release and new claim,
    // or the same name under another birth, drops the earlier claim's pending
    // faces: the delayed bell cannot speak for its replacement.
    for change in ["renew", "release", "birth"] {
        let mut tab = Tab::new();
        let landing = tab.act("landed@v1");
        let now = crate::metrics::now_us();
        if change == "release" {
            assert!(crate::session_timeline::release_supervisor(
                &tab.ctx,
                None,
                now,
                aterm_slog::Actor::Ctl
            ));
        }
        let mut proof = crate::self_claim::stand_in::running();
        if change == "birth" {
            proof.start = "Thu Jan  1 00:00:00 1970".to_string();
        }
        crate::session_timeline::claim_supervisor_as(
            &tab.ctx,
            &format!("codex-alab@{}", std::process::id()),
            Some(1),
            None,
            Some(proof),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("the same holder renews or takes the free session");
        tab.apply(landing);
        assert_eq!(
            tab.face(),
            (change == "renew").then_some(TaskFace::Landed),
            "claim change: {change}"
        );
        assert_eq!(records(&tab), usize::from(change == "renew"));
        tab.done();
    }
}

/// THE SCREEN'S PATH IS A TURN BEGUN TOO, never "busy now": a landing
/// recorded while the screen still read the agent busy is not reopened by a
/// later read that moves only the busy verdict's detail; a read that finds it
/// busy again after a read that did not is a turn begun, and reopens it.
#[test]
fn the_screens_busy_reopens_only_at_a_busy_begun_after_the_landing() {
    let mut tab = Tab::new();
    let stamp = |seq: u64| crate::session_timeline::AgentStamp {
        generation: crate::control::ScreenGen { epoch: 1, seq },
        fp: seq,
    };
    let publish = |tab: &Tab, word: &'static str, detail: Option<&str>, seq: u64| {
        tab.ctx.timeline.lock().unwrap().publish_agent(
            word,
            detail.map(str::to_string),
            None,
            Some(aterm_phase::Program::Claude),
            stamp(seq),
        )
    };
    publish(&tab, "busy", Some("thinking"), 1);
    tab.lands();
    assert!(
        publish(&tab, "busy", Some("reading"), 2),
        "the verdict moved"
    );
    tab.app.reopen_self_claim_landing(tab.session);
    assert_eq!(
        tab.face(),
        Some(TaskFace::Landed),
        "busy still, from before the landing: no turn begun"
    );
    publish(&tab, "idle", None, 3);
    tab.app.reopen_self_claim_landing(tab.session);
    assert_eq!(tab.face(), Some(TaskFace::Landed), "at rest");
    publish(&tab, "busy", Some("thinking"), 4);
    tab.app.reopen_self_claim_landing(tab.session);
    assert_eq!(tab.face(), Some(TaskFace::Open), "a busy begun after it");
    tab.done();
}

/// A RE-EXEC AT REST KEEPS THE LANDING: codex-alab lands, then upgrades in
/// place (`meta set harness_act upgrade@v1`, the one act it records at rest
/// after a landing; the next image resumes the same conversation with the
/// landing carried across), and the tab stays landed through the act and the
/// sweep after it. Before, every act but `landed@v1` told `open`, so the
/// re-exec reopened a task nobody had touched. CONTROL: a turn begun after
/// it still reopens the landing.
#[test]
fn a_reexec_onto_a_new_build_at_rest_keeps_the_landing() {
    let mut tab = Tab::new();
    for bytes in [IDLE, WORKING, DONE] {
        tab.says(bytes);
        tab.sweep();
    }
    tab.lands();
    let wakes = tab.act("upgrade@v1");
    tab.apply(wakes);
    tab.sweep();
    assert_eq!(
        tab.face(),
        Some(TaskFace::Landed),
        "a re-exec onto a new build is no new task"
    );
    tab.says(WORKING);
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Open), "CONTROL: the next turn");
    tab.done();
}

/// A PERSON'S OWN EXCHANGE, LEFT BY THE SELF-CLAIM, READS THEIRS: after its
/// landing a person's turn reopens the task, and codex-alab, leaving the
/// point after that turn to the person, records `left-to-person@v1` (never
/// typed): the face is `theirs`, as aterm's own loop tells it at its `LEFT`
/// rule, and a sweep with no turn since keeps it. Under Cruise too: unlike a
/// landing, leaving a person's exchange to them ends nothing. CONTROLS: the
/// holder's next act (`continue@v1`) opens the task again, and the person's
/// rule at another version is an act like any other.
#[test]
fn a_persons_exchange_the_self_claim_left_reads_theirs() {
    let mut tab = Tab::new();
    for bytes in [IDLE, WORKING, DONE] {
        tab.says(bytes);
        tab.sweep();
    }
    tab.lands();
    tab.says(WORKING);
    tab.sweep();
    assert_eq!(
        tab.face(),
        Some(TaskFace::Open),
        "the person's turn reopens"
    );
    tab.says(DONE);
    tab.sweep();
    let wakes = tab.act("left-to-person@v1");
    tab.apply(wakes);
    assert_eq!(tab.face(), Some(TaskFace::Theirs), "left to the person");
    tab.sweep();
    assert_eq!(
        tab.face(),
        Some(TaskFace::Theirs),
        "no turn since: still theirs"
    );
    let wakes = tab.act("continue@v1");
    tab.apply(wakes);
    assert_eq!(tab.face(), Some(TaskFace::Open), "CONTROL: the holder acts");
    tab.ctx
        .meta
        .lock()
        .unwrap()
        .set("autopilot", Some("cruise".into()));
    let wakes = tab.act("left-to-person@v1");
    tab.apply(wakes);
    assert_eq!(
        tab.face(),
        Some(TaskFace::Theirs),
        "theirs under Cruise too"
    );
    let wakes = tab.act("left-to-person@v2");
    tab.apply(wakes);
    assert_eq!(
        tab.face(),
        Some(TaskFace::Open),
        "CONTROL: another version is an act like any other"
    );
    tab.done();
}

/// A POINT LEFT TO A PERSON REOPENS AT THEIR NEXT TURN, as a landing does
/// (`harness_task::holds_until_another_turn`): codex-alab records
/// `left-to-person@v1` once and nothing after it (the person's next turn is
/// theirs; it decides nothing at its end), so the face it told must not stand
/// through the person's next exchange; aterm's own loop reads `open` once a
/// turn it did not start is read after its `LEFT` (`TaskFace::of`, rule 1).
/// The turn counts whether a sweep reads it running or only after it ended,
/// and when the sweep counts it before the act's face is applied. NEGATIVE
/// CONTROLS: a sweep with no turn since keeps `theirs`, as does the act's own
/// turn's report written again.
#[test]
fn a_point_left_to_a_person_reopens_at_their_next_turn() {
    for (case, between) in [("read running", false), ("before the face", true)] {
        for quick in [false, true] {
            let mut tab = Tab::new();
            for bytes in [IDLE, WORKING, DONE] {
                tab.says(bytes);
                tab.sweep();
            }
            let wakes = tab.act("left-to-person@v1");
            if between {
                tab.says(WORKING);
                if quick {
                    tab.says(DONE);
                }
                tab.sweep();
                tab.apply(wakes);
                assert_eq!(tab.face(), Some(TaskFace::Open), "{case}, quick {quick}");
                tab.done();
                continue;
            }
            tab.apply(wakes);
            assert_eq!(tab.face(), Some(TaskFace::Theirs), "left to the person");
            tab.sweep();
            tab.says(DONE);
            tab.sweep();
            assert_eq!(
                tab.face(),
                Some(TaskFace::Theirs),
                "NEGATIVE CONTROL: no turn since, the act's report again: theirs"
            );
            tab.says(WORKING);
            if quick {
                tab.says(DONE);
            }
            tab.sweep();
            assert_eq!(
                tab.face(),
                Some(TaskFace::Open),
                "{case}, quick {quick}: the person's next turn reopens"
            );
            tab.done();
        }
    }

    // A user's submission already reports Working while native startup is
    // waiting behind the holder's publication gate. Its eventual acceptance
    // need not move that count again. Conditional LEFT must not include such
    // a report in the point it leaves to the person. Blocked and clear/reset
    // also retain the previous in-turn count, so only explicit rest admits it.
    const BLOCKED: &[u8] = b"\x1b]7501;state=blocked:app=codex:kind=question\x1b\\";
    const CLEAR: &[u8] = b"\x1b]7501;state=clear:app=codex\x1b\\";
    const ERROR: &[u8] = b"\x1b]7501;state=error:app=codex\x1b\\";
    let bounded = |tab: &Tab, conn, prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>| {
        let until = crate::metrics::now_us() + 5_000_000;
        crate::control::meta_wakes_proving_for_test(
            &tab.app.store,
            &tab.term,
            tab.session,
            &tab.ctx,
            &format!("set harness_act left-to-person@v1 until_us={until}"),
            conn,
            prove,
        )
    };
    for (case, report) in [
        ("pending Working", Some(WORKING)),
        ("Blocked", Some(BLOCKED)),
        ("clear", Some(CLEAR)),
        ("reset", Some(b"\x1bc".as_slice())),
        ("unknown", None),
    ] {
        let mut tab = Tab::new();
        let wakes = tab.act("continue@v1");
        tab.apply(wakes);
        if let Some(report) = report {
            tab.says(IDLE);
            tab.says(WORKING);
            tab.says(report);
        }
        let reported = crate::term_lock(&tab.term).program_state();
        if matches!(case, "clear" | "reset" | "unknown") {
            assert!(reported.state.is_none(), "{case} clears the current report");
        } else {
            use aterm_core::terminal::program_state::ProgramStateWord as W;
            let expected = if case == "Blocked" {
                W::Blocked
            } else {
                W::Working
            };
            assert_eq!(
                reported.state.map(|state| state.state),
                Some(expected),
                "{case}"
            );
        }
        if case != "unknown" {
            assert_eq!(reported.turns, 1, "{case} retains the in-turn count");
        }
        let before = {
            let timeline = tab.ctx.timeline.lock().unwrap();
            (
                timeline.last_act().map(str::to_owned),
                timeline.landing_mark(),
                timeline
                    .since(None)
                    .filter(|row| row.kind == crate::session_timeline::HARNESS_ACT)
                    .count(),
            )
        };
        let (reply, wakes) = bounded(&tab, Some(1), &|_| None);
        assert_eq!(reply, "ERR busy harness_act\n", "{case}");
        assert!(wakes.is_empty(), "a refused {case} posts no wake");
        let timeline = tab.ctx.timeline.lock().unwrap();
        assert_eq!(
            (
                timeline.last_act().map(str::to_owned),
                timeline.landing_mark(),
                timeline
                    .since(None)
                    .filter(|row| row.kind == crate::session_timeline::HARNESS_ACT)
                    .count()
            ),
            before
        );
        drop(timeline);
        assert!(
            tab.ctx
                .timeline
                .lock()
                .unwrap()
                .take_self_tasks()
                .is_empty()
        );
        assert_eq!(tab.face(), Some(TaskFace::Open));
        // Native work repeating the pending report cannot repair an admitted
        // stale face by adding another turn; the refusal keeps it open.
        if case == "pending Working" {
            let turns = crate::term_lock(&tab.term).program_state().turns;
            tab.says(WORKING);
            assert_eq!(crate::term_lock(&tab.term).program_state().turns, turns);
            tab.sweep();
            assert_eq!(tab.face(), Some(TaskFace::Open));
        }
        let (reply, wakes) = bounded(&tab, Some(2), &|_| None);
        assert_eq!(reply, "ERR denied\n", "proof precedes the {case} refusal");
        assert!(wakes.is_empty());
        tab.done();
    }
    for at_rest in [IDLE, DONE, ERROR] {
        let mut tab = Tab::new();
        tab.says(WORKING);
        tab.says(at_rest);
        let (reply, wakes) = bounded(&tab, Some(1), &|_| None);
        assert_eq!(reply, "OK\n", "explicit rest admits conditional LEFT");
        tab.apply(wakes);
        assert_eq!(tab.face(), Some(TaskFace::Theirs));
        // A pending report parsed during the off-lock proof stays beyond the
        // SAME earlier snapshot. Even if the sweep sees it before the wake,
        // the conditional face must reopen when it is applied.
        let wakes = tab.act("continue@v1");
        tab.apply(wakes);
        let term = Arc::clone(&tab.term);
        let proven = crate::self_claim::stand_in::running();
        let (reply, wakes) = bounded(&tab, Some(2), &|_| {
            crate::term_lock(&term).process(WORKING);
            Some(proven.clone())
        });
        assert_eq!(reply, "OK\n");
        tab.sweep();
        tab.apply(wakes);
        assert_eq!(
            tab.face(),
            Some(TaskFace::Open),
            "later counted input reopens the earlier mark"
        );
        tab.done();
    }
}

/// THE RELAND FACE RACE (codex-alab parity-4, `harness_task::landing_turns`):
/// codex-alab records its land act once the land turn has started and
/// `landed@v1` at that turn's yield, both on the control socket, while its
/// TUI writes the turn's OSC 7501 reports to the PTY. When both acts reach
/// aterm before the turn's begin is parsed, that begin — read after the
/// landing — is the landing's own turn, and must not reopen it; the person's
/// turn after it does. As it is when the begin is parsed between the two
/// acts (the program's order and aterm's agree). NEGATIVE CONTROLS: a land
/// act that came in a turn (its begin counted already) owes nothing, so the
/// next turn's begin reopens; and neither does one recorded under a claim
/// since released, whose holder's acts are no landing's for the next.
#[test]
fn a_landing_whose_acts_outran_its_own_turns_begin_stays_landed() {
    let mut wrong = Vec::new();
    for case in ["outran", "begun between", "act in a turn", "claim since"] {
        let mut tab = Tab::new();
        // The turn before, every report swept: one turn begun, at rest.
        for bytes in [IDLE, WORKING, DONE] {
            tab.says(bytes);
            tab.sweep();
        }
        if case == "act in a turn" {
            tab.says(IDLE);
            tab.says(WORKING);
        }
        let wakes = tab.act("land@v1");
        tab.apply(wakes);
        assert_eq!(tab.face(), Some(TaskFace::Open), "{case}: the land act");
        match case {
            "begun between" => tab.says(WORKING),
            "act in a turn" => tab.says(DONE),
            "claim since" => {
                let now = crate::metrics::now_us();
                assert!(crate::session_timeline::release_supervisor(
                    &tab.ctx,
                    None,
                    now,
                    aterm_slog::Actor::Ctl
                ));
                crate::session_timeline::claim_supervisor_as(
                    &tab.ctx,
                    &format!("codex-alab@{}", std::process::id()),
                    Some(1),
                    None,
                    Some(crate::self_claim::stand_in::running()),
                    now,
                    aterm_slog::Actor::Ctl,
                )
                .expect("the holder takes the free session again");
            }
            _ => {}
        }
        tab.sweep();
        tab.lands();
        let owes = matches!(case, "outran" | "begun between");
        if case != "act in a turn" {
            // The land turn's reports, parsed after its landing.
            if case != "begun between" {
                tab.says(IDLE);
                tab.says(WORKING);
            }
            tab.sweep();
            tab.says(DONE);
            tab.sweep();
            let want = if owes {
                TaskFace::Landed
            } else {
                TaskFace::Open
            };
            if tab.face() != Some(want) {
                wrong.push(format!(
                    "{case}: the land turn's begin, read after the landing: {:?}",
                    tab.face()
                ));
            }
        }
        if owes || case == "act in a turn" {
            // The next turn: a person's.
            tab.says(WORKING);
            tab.sweep();
            if tab.face() != Some(TaskFace::Open) {
                wrong.push(format!(
                    "{case}: a turn begun after the landing: {:?}",
                    tab.face()
                ));
            }
        }
        tab.done();
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// AN ACT THAT STARTS NO TURN NAMES NO LANDING'S TURN (review of codex-alab
/// parity-4, R1): after the land turn's reports were all parsed, codex-alab
/// records an act that types nothing at rest — a report mailed, an approval,
/// a box's answer — and then its landing. That act is no turn still to come,
/// so the landing is marked at the engine's count, and a person's turn begun
/// after it reopens the task. NEGATIVE CONTROL: a continuation in the same
/// place does start a turn whose begin is still unparsed when the landing
/// arrives, so that begin stays the landing's own, and the person's turn after
/// it reopens.
#[test]
fn an_act_that_starts_no_turn_leaves_the_landings_turn_as_it_was() {
    let mut wrong = Vec::new();
    for act in [
        "mail-report@v1",
        "approve-all@v1",
        "answer-recommended@v1",
        "continue@v1",
    ] {
        let starts_one = act == "continue@v1";
        let mut tab = Tab::new();
        // The turn before, every report swept: one turn begun, at rest.
        for bytes in [IDLE, WORKING, DONE] {
            tab.says(bytes);
            tab.sweep();
        }
        let wakes = tab.act("land@v1");
        tab.apply(wakes);
        // The land turn, every report swept before its landing.
        tab.says(IDLE);
        tab.says(WORKING);
        tab.sweep();
        tab.says(DONE);
        tab.sweep();
        let wakes = tab.act(act);
        tab.apply(wakes);
        tab.lands();
        if starts_one {
            // The continuation's own turn, parsed after the landing.
            tab.says(IDLE);
            tab.says(WORKING);
            tab.sweep();
            tab.says(DONE);
            tab.sweep();
            if tab.face() != Some(TaskFace::Landed) {
                wrong.push(format!(
                    "{act}: its own turn's begin, read after the landing: {:?}",
                    tab.face()
                ));
            }
        }
        // A person's turn.
        tab.says(WORKING);
        tab.sweep();
        if tab.face() != Some(TaskFace::Open) {
            wrong.push(format!(
                "{act}: a turn begun after the landing: {:?}",
                tab.face()
            ));
        }
        tab.done();
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A SELF-CLAIM'S FACE STANDS ACROSS ITS OWN RE-EXEC (Autopilot Land:
/// `upgrade@v1` moves no face, the landing carried across; seen live in
/// aterm-lab's codex-alab proof, 2026-10-22, where the exchange left to the
/// person and every landed Land leg read `task=open` past the upgrade):
/// codex-alab records `upgrade@v1`, gives its claim back as its image execs,
/// and its next image claims again. In that gap the watching loop's run is
/// cut and its next run starts, telling "unknown" and then its own start
/// face, and the new claim cuts it again: none of those is the session's.
/// The face the self-claim told (`theirs`, `landed`) stands through the gap
/// and past the new claim, until a turn begun past it. NEGATIVE CONTROLS: a
/// claim given back with no upgrade before it (codex-alab gone passive)
/// leaves the session to the watching loop, whose face is told; so does one
/// given back after the new image's claim (the upgrade is spent).
#[test]
fn a_self_claims_face_stands_across_its_own_reexec() {
    for case in ["theirs", "landed", "passive", "passive after the re-exec"] {
        let upgrade = case != "passive";
        let mut tab = Tab::new();
        let sid = tab.ctx.self_id.to_string();
        for bytes in [IDLE, WORKING, DONE] {
            tab.says(bytes);
            tab.sweep();
        }
        let want = if case == "landed" {
            tab.lands();
            TaskFace::Landed
        } else {
            let wakes = tab.act("left-to-person@v1");
            tab.apply(wakes);
            TaskFace::Theirs
        };
        assert_eq!(tab.face(), Some(want), "{case}");
        if upgrade {
            let wakes = tab.act("upgrade@v1");
            tab.apply(wakes);
        }
        // The image execs: its claim is given back while it runs.
        let release = |tab: &Tab| {
            assert!(crate::session_timeline::release_supervisor(
                &tab.ctx,
                Some(1),
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            ));
        };
        // The watching loop's run, cut, tells "unknown"; its next run tells
        // its start face.
        let watching = |tab: &mut Tab| {
            tab.app
                .apply_harness_fact(&sid, crate::HarnessFact::Task(None));
            tab.app
                .apply_harness_fact(&sid, crate::HarnessFact::TaskNoted(TaskFace::Open, 1));
            tab.app
                .apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
            tab.sweep();
        };
        release(&tab);
        watching(&mut tab);
        if !upgrade {
            assert_eq!(
                tab.face(),
                Some(TaskFace::Open),
                "NEGATIVE CONTROL: given back with no upgrade, the watching loop's face"
            );
            tab.done();
            continue;
        }
        assert_eq!(tab.face(), Some(want), "{case}: through the re-exec's gap");
        // The new image claims again, which cuts the watching loop's run.
        crate::session_timeline::claim_supervisor_as(
            &tab.ctx,
            &format!("codex-alab@{}", std::process::id()),
            Some(1),
            None,
            Some(crate::self_claim::stand_in::running()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("the new image claims again");
        tab.app
            .apply_harness_fact(&sid, crate::HarnessFact::Task(None));
        tab.sweep();
        assert_eq!(tab.face(), Some(want), "{case}: past the new claim");
        if case == "passive after the re-exec" {
            release(&tab);
            watching(&mut tab);
            assert_eq!(
                tab.face(),
                Some(TaskFace::Open),
                "NEGATIVE CONTROL: given back again, the upgrade spent"
            );
            tab.done();
            continue;
        }
        // CONTROL: a turn begun past it still opens the task.
        tab.says(WORKING);
        tab.sweep();
        assert_eq!(tab.face(), Some(TaskFace::Open), "{case}: the next turn");
        tab.done();
    }
}

// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `App::reopen_self_claim_landing` on turns started after a proven self-claim landing.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::TaskFace;
use aterm_core::terminal::Terminal;

use crate::App;

/// The bytes codex-alab writes (`terminal_program_status.rs`'s
const IDLE: &[u8] = b"\x1b]7501;state=idle:app=codex\x1b\\";
const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";

/// One tab under Land whose `codex` holds a proven self-claim, bound to
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
#[test]
fn a_turn_begun_and_ended_between_two_sweeps_reopens_the_landing() {
    let mut tab = Tab::new();
    for bytes in [IDLE, WORKING, DONE] {
        tab.says(bytes);
        tab.sweep();
    }
    tab.lands();
    tab.sweep();
    assert_eq!(tab.face(), Some(TaskFace::Landed), "no turn since: landed");
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
#[test]
fn the_landing_turns_own_reports_read_after_it_reopen_nothing() {
    let mut tab = Tab::new();
    tab.says(IDLE);
    tab.sweep();
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
#[test]
fn the_landing_turns_working_first_read_after_it_reopens_nothing() {
    let mut tab = Tab::new();
    tab.says(IDLE);
    tab.sweep();
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
#[test]
fn a_report_written_again_after_a_handoff_reopens_nothing() {
    const CLEAR: &[u8] = b"\x1b]7501;state=clear\x1b\\";
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
#[test]
fn a_landing_whose_acts_outran_its_own_turns_begin_stays_landed() {
    let mut wrong = Vec::new();
    for case in ["outran", "begun between", "act in a turn", "claim since"] {
        let mut tab = Tab::new();
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
        for bytes in [IDLE, WORKING, DONE] {
            tab.says(bytes);
            tab.sweep();
        }
        let wakes = tab.act("land@v1");
        tab.apply(wakes);
        tab.says(IDLE);
        tab.says(WORKING);
        tab.sweep();
        tab.says(DONE);
        tab.sweep();
        let wakes = tab.act(act);
        tab.apply(wakes);
        tab.lands();
        if starts_one {
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
        let release = |tab: &Tab| {
            assert!(crate::session_timeline::release_supervisor(
                &tab.ctx,
                Some(1),
                crate::metrics::now_us(),
                aterm_slog::Actor::Ctl,
            ));
        };
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
        tab.says(WORKING);
        tab.sweep();
        assert_eq!(tab.face(), Some(TaskFace::Open), "{case}: the next turn");
        tab.done();
    }
}

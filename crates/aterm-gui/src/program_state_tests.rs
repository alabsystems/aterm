// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_core::terminal::Terminal;
use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord};

const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
const BLOCKED_PERMISSION: &[u8] = b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\";
const CLEAR: &[u8] = b"\x1b]7501;state=clear\x1b\\";

fn shown(gate: &ProgramStateGate) -> Option<String> {
    gate.shown().map(|(s, _)| s.wire())
}

/// The real engine fed the bytes codex writes, read as the sweep reads it.
#[test]
fn a_codex_in_front_shows_its_report_and_a_clear_takes_it_away() {
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    let t0 = Instant::now();
    assert!(!gate.observe(term.program_state(), true, false, t0));
    term.process(WORKING);
    assert!(gate.observe(term.program_state(), true, false, t0));
    assert_eq!(shown(&gate).as_deref(), Some("working"));
    assert_eq!(wire(gate.shown()), "working");
    assert!(!gate.observe(term.program_state(), true, false, t0));
    term.process(BLOCKED_PERMISSION);
    assert!(gate.observe(term.program_state(), true, false, t0));
    assert_eq!(wire(gate.shown()), "blocked:permission");
    term.process(CLEAR);
    assert!(gate.observe(term.program_state(), true, false, t0));
    assert_eq!(wire(gate.shown()), "-");
}

/// Negative control: the same bytes under a program in front that is not
/// `codex` show nothing.
#[test]
fn a_report_shows_only_while_a_codex_is_in_front() {
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    let t0 = Instant::now();
    term.process(WORKING);
    assert!(!gate.observe(term.program_state(), false, false, t0));
    assert_eq!(shown(&gate), None);
    assert!(gate.observe(term.program_state(), true, false, t0));
    assert_eq!(shown(&gate).as_deref(), Some("working"));
    assert!(gate.observe(term.program_state(), false, false, t0));
    assert_eq!(shown(&gate), None);
}

/// A foreground move clears it: the old program's last report never shows
/// again, while the next program's first one (made before the sweep saw
/// the move) does.
#[test]
fn a_foreground_move_clears_it_and_the_next_programs_report_shows() {
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    let t0 = Instant::now();
    term.process(WORKING);
    assert!(gate.observe(term.program_state(), true, false, t0));
    assert!(gate.observe(term.program_state(), true, true, t0));
    assert_eq!(shown(&gate), None, "the exited program's report is gone");
    assert!(!gate.observe(term.program_state(), true, false, t0));
    term.process(b"\x1b]7501;state=idle:app=codex\x1b\\");
    assert!(gate.observe(term.program_state(), true, true, t0));
    assert_eq!(shown(&gate).as_deref(), Some("idle"));
    let next_start = t0 + std::time::Duration::from_secs(1);
    term.process(b"\x1b]7501;state=idle:app=codex\x1b\\");
    assert!(gate.observe(term.program_state(), true, true, next_start));
    assert_eq!(gate.shown().map(|(_, since)| since), Some(next_start));
    assert!(!gate.observe(term.program_state(), true, false, next_start));
}

/// A foreground move judges the next report from no report: codex A exits
/// mid-turn and codex B is already working when the sweep sees the move,
/// and B's turn begins (its end pairs with its own start); B's first report
/// a `done` after A was working ends nothing.
#[test]
fn a_group_move_judges_the_next_report_from_none() {
    let mut term = Terminal::new(24, 80);
    let t0 = Instant::now();
    let t1 = t0 + std::time::Duration::from_secs(1);
    let done = b"\x1b]7501;state=done:app=codex\x1b\\";
    let mut gate = ProgramStateGate::default();
    term.process(WORKING);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!((gate.turn_starts(), gate.turn_started()), (1, Some(t0)));
    term.process(WORKING);
    assert!(gate.observe(term.program_state(), true, true, t1));
    assert_eq!(
        gate.turn_starts(),
        2,
        "B was working when the move was seen"
    );
    assert_eq!(gate.turn_started(), Some(t1), "B's own start");
    term.process(WORKING);
    gate.observe(term.program_state(), true, false, t1);
    assert_eq!(gate.turn_starts(), 2, "control: no move, the same turn");
    term.process(done);
    assert!(gate.observe(term.program_state(), true, true, t1));
    assert_eq!(gate.turn_ends(), 0, "A's turn did not end in B's report");
    let mut still = ProgramStateGate::default();
    term.process(WORKING);
    still.observe(term.program_state(), true, false, t0);
    term.process(done);
    still.observe(term.program_state(), true, false, t0);
    assert_eq!(still.turn_ends(), 1, "control: no move, a turn end");
}

/// A NEW BOX IS A MOVE: a `blocked` report, then a `working` and a second `blocked` with the
/// same words between two sweeps, moves the gate (the second box began anew); a re-send of
/// the box shown does not. Neither counts a turn the gate did not see.
#[test]
fn a_second_box_between_two_sweeps_is_a_move() {
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    let t0 = Instant::now();
    term.process(WORKING);
    assert!(gate.observe(term.program_state(), true, false, t0));
    term.process(BLOCKED_PERMISSION);
    assert!(gate.observe(term.program_state(), true, false, t0));
    let turns = (gate.turn_starts(), gate.turn_ends());
    term.process(BLOCKED_PERMISSION);
    assert!(
        !gate.observe(term.program_state(), true, false, t0),
        "CONTROL: the box re-sent is no move"
    );
    term.process(&[WORKING, BLOCKED_PERMISSION].concat());
    assert!(
        gate.observe(term.program_state(), true, false, t0),
        "the next box is a move"
    );
    assert_eq!(shown(&gate).as_deref(), Some("blocked:permission"));
    assert_eq!((gate.turn_starts(), gate.turn_ends()), turns);
    assert!(!gate.observe(term.program_state(), true, false, t0));
}

/// A turn end is `working` then `done`|`idle` as the gate showed them:
/// not a `done` with no `working` before it, not `working` → `blocked`,
/// and not a report the gate does not honour.
#[test]
fn only_working_then_done_or_idle_counts_as_a_turn_end() {
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    let t0 = Instant::now();
    let done = b"\x1b]7501;state=done:app=codex\x1b\\";
    let idle = b"\x1b]7501;state=idle:app=codex\x1b\\";
    term.process(done);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!(gate.turn_ends(), 0, "done with no working before it");
    term.process(WORKING);
    gate.observe(term.program_state(), true, false, t0);
    term.process(BLOCKED_PERMISSION);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!(gate.turn_ends(), 0, "working then blocked");
    term.process(WORKING);
    gate.observe(term.program_state(), true, false, t0);
    term.process(done);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!(gate.turn_ends(), 1);
    term.process(WORKING);
    gate.observe(term.program_state(), true, false, t0);
    term.process(idle);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!(gate.turn_ends(), 2);
    assert_eq!(gate.turn_starts(), 2);
    term.process(BLOCKED_PERMISSION);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!(gate.turn_starts(), 3, "idle then blocked");
    term.process(WORKING);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!(gate.turn_starts(), 3, "blocked then working resumes it");
    term.process(done);
    gate.observe(term.program_state(), true, false, t0);
    assert_eq!(gate.turn_ends(), 3);
    let mut other = ProgramStateGate::default();
    for bytes in [WORKING, done.as_slice()] {
        term.process(bytes);
        other.observe(term.program_state(), false, false, t0);
    }
    assert_eq!(other.turn_ends(), 0);
    assert_eq!(other.turn_starts(), 0);
}

/// GROK BUILD'S OWN REPORT THROUGH THE REAL SWEEP (`App::observe_session_statuses`): a real
/// engine fed the bytes Grok Build 1.0.70 writes (its root record, a subagent's child record
/// beside it, and its exit's `clear`) under a published `grok` is `status program_state=` and
/// the hover's `grok …` word; a child record moves neither.
/// NEGATIVE CONTROLS: the same bytes under a published `codex` show nothing (a report never
/// stands for another program), and so do codex's own bytes under a published `grok`.
#[test]
fn grok_builds_own_report_is_shown_only_under_a_grok_in_front() {
    const G_WORKING: &[u8] = b"\x1b]7501;state=working:app=grok-build:title=cGFyc2Vy\x1b\\";
    const G_BLOCKED: &[u8] =
        b"\x1b]7501;state=blocked:kind=permission:app=grok-build:title=cGFyc2Vy\x1b\\";
    const G_CHILD_DONE: &[u8] = b"\x1b]7501;state=done:app=grok-build:id=sub_1\x1b\\";
    const G_CHILD_CLEAR: &[u8] = b"\x1b]7501;state=clear:id=sub_1\x1b\\";
    const G_DONE: &[u8] = b"\x1b]7501;state=done:app=grok-build:title=cGFyc2Vy\x1b\\";
    let shown_through = |program: &str, stream: &[&[u8]]| -> Vec<String> {
        let mut app = crate::App::headless_for_test();
        let sid = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
        let (term, timeline) = {
            let pooled = app.pool.get(sid).expect("pooled");
            (
                std::sync::Arc::clone(&pooled.term),
                std::sync::Arc::clone(&pooled.ctx.timeline),
            )
        };
        {
            let mut tl = timeline.lock().unwrap();
            tl.note_foreground_group(4242);
            tl.set_program(4242, Some(program.into()));
        }
        let step = std::time::Duration::from_secs(1);
        let mut t = Instant::now();
        let mut seen = Vec::new();
        for bytes in stream {
            crate::term_lock(&term).process(bytes);
            let _ = app.observe_session_statuses(t);
            t += step;
            let shown = app.session_status.program_state(sid);
            seen.push(format!(
                "{} {}",
                wire(shown),
                shown.map_or_else(|| "-".into(), |(state, _)| row_word(state))
            ));
        }
        seen
    };
    let grok = [
        G_WORKING,
        G_CHILD_DONE,
        G_BLOCKED,
        G_CHILD_CLEAR,
        G_WORKING,
        G_DONE,
        CLEAR,
    ];
    assert_eq!(
        shown_through("grok", &grok),
        [
            "working grok working",
            "working grok working",
            "blocked:permission grok blocked",
            "blocked:permission grok blocked",
            "working grok working",
            "done grok done",
            "- -",
        ]
    );
    assert!(
        shown_through("codex", &grok).iter().all(|s| s == "- -"),
        "control: Grok Build's report under a codex"
    );
    assert!(
        shown_through("grok", &[WORKING, BLOCKED_PERMISSION, CLEAR])
            .iter()
            .all(|s| s == "- -"),
        "control: codex's report under a Grok Build"
    );
    assert_eq!(
        shown_through("codex", &[WORKING, CLEAR]),
        ["working codex working", "- -"],
        "control: codex's under a codex still shows"
    );
}

/// THE TURN LEDGER AND THE `done` EDGE THROUGH THE REAL SWEEP
/// (`App::observe_session_statuses`), a real engine fed the bytes codex
/// writes under a published `codex`: each honoured move is one
/// display-only `program-state` row, in order, and the first `done`
/// fires an `fx celebrate on=done` arm bound to the session, once (a
/// later `done` finds it spent).
#[test]
fn each_honoured_move_is_a_program_state_row_and_done_fires_an_on_done_arm() {
    use crate::session_timeline::PROGRAM_STATE;
    use aterm_effects::kitty_sing::{CelebrateOn, song_signature};
    const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";
    for codex in [true, false] {
        let mut app = crate::App::headless_for_test();
        app.config = aterm_toml::from_str("cursor_trail = true\ncursor_trail_style = \"rainbow\"")
            .expect("valid toml");
        let sid = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
        let (term, timeline) = {
            let pooled = app.pool.get(sid).expect("pooled");
            (
                std::sync::Arc::clone(&pooled.term),
                std::sync::Arc::clone(&pooled.ctx.timeline),
            )
        };
        {
            let mut tl = timeline.lock().unwrap();
            tl.note_foreground_group(4242);
            tl.set_program(4242, Some(if codex { "codex" } else { "claude" }.into()));
        }
        let armed = app
            .fx_control(crate::FxCtlOp::Celebrate {
                sig: song_signature('C'),
                bars: 1,
                on: CelebrateOn::Done,
            })
            .expect("admitted");
        assert!(armed.starts_with("armed=done "), "{armed}");
        assert!(
            app.fx_control(crate::FxCtlOp::Celebrate {
                sig: song_signature('C'),
                bars: 1,
                on: CelebrateOn::Done,
            })
            .is_err(),
            "a second arm is refused"
        );
        let step = std::time::Duration::from_secs(1);
        let mut t = std::time::Instant::now();
        let mut after: Vec<String> = Vec::new();
        for bytes in [
            WORKING,
            BLOCKED_PERMISSION,
            WORKING,
            DONE,
            WORKING,
            DONE,
            CLEAR,
        ] {
            crate::term_lock(&term).process(bytes);
            let _ = app.observe_session_statuses(t);
            t += step;
            after.push(app.fx_control(crate::FxCtlOp::Status).expect("reads"));
        }
        let waiting = |s: &String| s.starts_with("armed=done ") && s.contains(" live=false ");
        let fired = |s: &String| s.starts_with("armed=none ") && s.contains(" live=true ");
        let rows: Vec<String> = timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == PROGRAM_STATE)
            .map(|e| e.payload.to_string())
            .collect();
        let status = app.fx_control(crate::FxCtlOp::Status).expect("reads");
        if codex {
            assert_eq!(
                rows,
                [
                    "state=working kind=-",
                    "state=blocked kind=permission",
                    "state=working kind=-",
                    "state=done kind=-",
                    "state=working kind=-",
                    "state=done kind=-",
                    "state=- kind=-",
                ],
            );
            assert!(
                status.starts_with("armed=none ") && status.contains(" live=true "),
                "the first done fired the arm: {status}"
            );
            assert!(after[..3].iter().all(waiting), "{after:#?}");
            assert!(after[3..].iter().all(fired), "{after:#?}");
        } else {
            assert!(after.iter().all(waiting), "control: {after:#?}");
            assert!(rows.is_empty(), "control: no row under claude: {rows:?}");
            assert!(
                status.starts_with("armed=done ") && status.contains(" live=false "),
                "control: the arm still waits: {status}"
            );
        }
    }
}

#[test]
fn a_full_reset_clears_it() {
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    let t0 = Instant::now();
    term.process(WORKING);
    gate.observe(term.program_state(), true, false, t0);
    term.process(b"\x1bc");
    assert!(gate.observe(term.program_state(), true, false, t0));
    assert_eq!(shown(&gate), None);
}

/// A REPORT IS HONOURED ONLY UNDER THE PROGRAM IT NAMES: a `codex` report under a codex or a
/// runtime one runs under, a `grok-build` report under a Grok Build. NEGATIVE CONTROLS: each
/// app's report under the other program, under a runtime (no Grok Build runs under one), under
/// a shell or a supervised agent, and under no program. Another program's report
/// (`ProgramApp::Other`) is honoured under a program that is none of the three, leading as a job
/// or as the session's root (`aterm -e kimi`), never with the session's shell in front or under
/// one of the three.
#[test]
fn a_report_is_honoured_only_under_the_program_it_names() {
    let (codex, grok) = (Some(ProgramApp::Codex), Some(ProgramApp::GrokBuild));
    for program in [
        "codex",
        "/opt/x/bin/codex",
        "node",
        "/usr/local/bin/node",
        "bun",
    ] {
        assert!(honoured_for(Some(program), codex, true), "{program}");
        assert!(
            !honoured_for(Some(program), grok, true),
            "{program}: not Grok's"
        );
    }
    for program in ["grok", "/Users//x/.local/share/aterm/agents/grok"] {
        assert!(honoured_for(Some(program), grok, true), "{program}");
        assert!(
            !honoured_for(Some(program), codex, true),
            "{program}: not codex's"
        );
    }
    for program in ["claude", "muse", "agy", "kimi", "zsh", "-zsh", "vim"] {
        assert!(!honoured_for(Some(program), codex, true), "{program}");
        assert!(!honoured_for(Some(program), grok, true), "{program}");
    }
    assert!(!honoured_for(None, codex, true));
    assert!(!honoured_for(None, grok, true));
    assert!(
        !honoured_for(Some("codex"), None, true),
        "no report names no program"
    );
    let other =
        aterm_core::terminal::program_state::AppWord::new("kimi-code").map(ProgramApp::Other);
    for program in ["kimi", "muse", "agy", "vim", "/opt/x/bin/my-agent", "node"] {
        assert!(honoured_for(Some(program), other, true), "{program}");
        assert!(
            honoured_for(Some(program), other, false),
            "{program}: the session's root (`aterm -e {program}`)"
        );
    }
    for shell in ["zsh", "-zsh", "bash", "fish"] {
        assert!(
            !honoured_for(Some(shell), other, false),
            "{shell}: the session's shell in front"
        );
        assert!(
            honoured_for(Some(shell), other, true),
            "{shell}: a shell running as a job (a script, a subshell)"
        );
    }
    for program in ["claude", "codex", "grok"] {
        assert!(
            !honoured_for(Some(program), other, true),
            "{program} reports under its own word"
        );
    }
    assert!(
        !honoured_for(None, other, true),
        "no program named in front"
    );
}

#[test]
fn the_row_word_names_the_reports_program_and_its_state() {
    let blocked = |app| ProgramState {
        state: ProgramStateWord::Blocked,
        kind: Some(ProgramBlockedKind::Question),
        app,
    };
    assert_eq!(row_word(blocked(ProgramApp::Codex)), "codex blocked");
    assert_eq!(row_word(blocked(ProgramApp::GrokBuild)), "grok blocked");
    assert_eq!(row_word(blocked(ProgramApp::ClaudeCode)), "claude blocked");
}

/// THE REAL `status` RECORD AND PRESENCE FACTS of a real session, its real
/// engine fed the bytes codex writes, folded as the sweep folds them: a
/// shown report is `program_state=` and the hover's word, beside an
/// `agent=` it never moves; the group moving clears both.
#[test]
fn a_shown_report_is_status_program_state_and_never_agent() {
    let field = |record: &str, key: &str| {
        record
            .split(' ')
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    for codex in [true, false] {
        let mut app = crate::App::headless_for_test();
        let shown = app
            .front_terminal(crate::WindowId(0))
            .expect("a front terminal");
        let (session, term) = (shown.session, std::sync::Arc::clone(&shown.term));
        let before = app.session_status_record(session).expect("a record");
        crate::term_lock(&term).process(WORKING);
        let report = crate::term_lock(&term).program_state();
        let t0 = Instant::now();
        assert_eq!(
            app.session_status
                .observe_program_state(session, report, codex, false, t0),
            codex
        );
        let record = app.session_status_record(session).expect("a record");
        let want = if codex { "working" } else { "-" };
        assert_eq!(
            field(&record, "program_state").as_deref(),
            Some(want),
            "{record}"
        );
        let age = field(&record, "program_state_ms");
        if codex {
            assert!(
                age.as_deref()
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .is_some(),
                "the shown report's age: {record}"
            );
        } else {
            assert_eq!(age.as_deref(), Some("-"), "none shown, no age: {record}");
        }
        for key in [
            "agent",
            "agent_detail",
            "agent_rev",
            "agent_gen",
            "agent_fp",
        ] {
            assert_eq!(
                field(&record, key),
                field(&before, key),
                "{key} moved: {record}"
            );
        }
        let row = app
            .presence_facts(session)
            .and_then(|facts| facts.program)
            .map(|(word, _)| word);
        assert_eq!(row.as_deref(), codex.then_some("codex working"));
        assert_eq!(field(&record, "last_act").as_deref(), Some("-"));
        app.pool
            .get(session)
            .expect("pooled")
            .ctx
            .timeline
            .lock()
            .unwrap()
            .note_harness_act("continue", "codex-alab@1", aterm_slog::Actor::Ctl);
        let record = app.session_status_record(session).expect("a record");
        assert_eq!(field(&record, "last_act").as_deref(), Some("continue"));
        app.session_status
            .observe_program_state(session, report, codex, true, t0);
        let record = app.session_status_record(session).expect("a record");
        assert_eq!(
            field(&record, "program_state").as_deref(),
            Some("-"),
            "{record}"
        );
        assert_eq!(
            field(&record, "program_state_ms").as_deref(),
            Some("-"),
            "{record}"
        );
    }
}

/// `program_state_ms=` counts through sleep, as the survey's `agent_ms=` does: the gate stamps
/// a shown report on the continuous clock (`aterm_uds::process::continuous_ms`) and ages it
/// there, never by the sweep's `Instant` nor by the process's `Instant` epoch
/// (`aterm_gui_timing::now_ms`, the clock of `status agent_since_ms=`), which both stop while
/// the Mac sleeps. The stamp lies between continuous readings taken around the sweep, far
/// above the process epoch's count; a stamp a minute back that no `Instant` here saw pass
/// reads a minute; a repeat of the shown report keeps its age and a move restarts it.
#[test]
fn the_shown_reports_age_counts_on_the_clock_that_counts_sleep() {
    const MINUTE: u64 = 60_000;
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    assert_eq!(gate.shown_age_ms(), None);
    assert_eq!(
        status_fields(Some(&gate)),
        "program_state=- program_state_ms=-"
    );
    assert_eq!(status_fields(None), "program_state=- program_state_ms=-");
    let swept = Instant::now()
        .checked_sub(std::time::Duration::from_secs(10))
        .expect("an instant ten seconds back");
    term.process(WORKING);
    let before = aterm_uds::process::continuous_ms();
    assert!(gate.observe(term.program_state(), true, false, swept));
    let after = aterm_uds::process::continuous_ms();
    let Some((before, after)) = before.zip(after) else {
        let age = gate.shown_age_ms().expect("a shown report has an age");
        assert!(
            age >= 10_000,
            "no continuous clock here: the sweep's Instant: {age}"
        );
        return;
    };
    let stamp = gate.shown_continuous_ms.expect("a shown report is stamped");
    assert!(
        (before..=after).contains(&stamp),
        "stamped on the continuous clock: {before} <= {stamp} <= {after} (the process \
         epoch reads {})",
        aterm_gui_timing::now_ms()
    );
    let age = gate.shown_age_ms().expect("a shown report has an age");
    assert!(
        age < 10_000,
        "aged from the continuous stamp, not the sweep's Instant: {age}"
    );
    let fields = status_fields(Some(&gate));
    assert!(
        fields.starts_with("program_state=working program_state_ms="),
        "{fields}"
    );
    gate.shown_continuous_ms = Some(stamp.checked_sub(MINUTE).expect("a machine up a minute"));
    let slept = gate.shown_age_ms().expect("a shown report has an age");
    assert!(
        (MINUTE..MINUTE + 10_000).contains(&slept),
        "a minute only the continuous clock saw reads a minute: {slept}"
    );
    assert!(
        !gate.observe(term.program_state(), true, false, Instant::now()),
        "PRECONDITION: a repeat is no move"
    );
    let kept = gate.shown_age_ms().expect("a shown report has an age");
    assert!(kept >= MINUTE, "a repeat keeps the age: {kept}");
    term.process(BLOCKED_PERMISSION);
    assert!(gate.observe(term.program_state(), true, false, Instant::now()));
    let moved = gate.shown_age_ms().expect("a shown report has an age");
    assert!(moved < 10_000, "a move restarts the age: {moved}");
    term.process(CLEAR);
    assert!(gate.observe(term.program_state(), true, false, Instant::now()));
    assert_eq!(gate.shown_age_ms(), None, "a clear takes the age with it");
}

/// THE CLAIM'S OWN COLUMN ON `ls`, through the real sweep (`App::observe_session_statuses`)
/// and the real `sessions` reply: a codex's shown report is its row's `program_state=`, beside
/// `agent=` and its siblings, which it never moves; under claude the same bytes show `-`.
#[test]
fn a_shown_report_is_the_ls_rows_program_state_and_never_agent() {
    let field = |row: &str, key: &str| {
        row.split(' ')
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    for codex in [true, false] {
        let mut app = crate::App::headless_for_test();
        let sid = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
        let (term, timeline) = {
            let pooled = app.pool.get(sid).expect("pooled");
            (
                std::sync::Arc::clone(&pooled.term),
                std::sync::Arc::clone(&pooled.ctx.timeline),
            )
        };
        {
            let mut tl = timeline.lock().unwrap();
            tl.note_foreground_group(4242);
            tl.set_program(4242, Some(if codex { "codex" } else { "claude" }.into()));
        }
        let row = |app: &crate::App| {
            crate::control::cmd_sessions_store(&app.store, None)
                .lines()
                .find(|line| line.split(' ').next() == Some(sid.to_string().as_str()))
                .expect("the session's row")
                .to_string()
        };
        let mut t = Instant::now();
        let _ = app.observe_session_statuses(t);
        let before = row(&app);
        assert_eq!(
            field(&before, "program_state").as_deref(),
            Some("-"),
            "{before}"
        );
        for bytes in [WORKING, BLOCKED_PERMISSION] {
            t += std::time::Duration::from_secs(1);
            crate::term_lock(&term).process(bytes);
            let _ = app.observe_session_statuses(t);
        }
        let after = row(&app);
        let want = if codex { "blocked:permission" } else { "-" };
        assert_eq!(
            field(&after, "program_state").as_deref(),
            Some(want),
            "{after}"
        );
        for key in [
            "agent",
            "agent_detail",
            "agent_rev",
            "agent_gen",
            "agent_fp",
        ] {
            assert_eq!(
                field(&after, key),
                field(&before, key),
                "{key} moved: {after}"
            );
        }
        assert!(
            after.contains(" program_state=") && after.contains(" human_ms="),
            "{after}"
        );
        let (agent_cols, rest) = after.split_once(" program_state=").expect("the column");
        assert!(
            agent_cols.contains(" agent_fp=") && rest.contains(" human_ms="),
            "after the agent fields, before human_ms=: {after}"
        );
    }
}

/// A Claude report is a claim by Claude alone. The other agents and runtimes are
/// negative controls; a cleared report cannot leave a standing status behind.
#[test]
fn claude_reports_are_shown_only_for_claude_and_clear_with_the_root() {
    let mut term = Terminal::new(24, 80);
    let mut gate = ProgramStateGate::default();
    let now = Instant::now();
    term.process(b"\x1b]7501;state=working:app=claude-code\x1b\\");
    let report = term.program_state();
    let app = report.state.map(|state| state.app);
    assert!(honoured_for(Some("claude"), app, true));
    for other in ["codex", "grok", "node", "bun", "zsh", "muse", "agy", "kimi"] {
        assert!(!honoured_for(Some(other), app, true), "{other}");
    }
    assert!(!honoured_for(None, app, true));
    assert!(gate.observe(report, honoured_for(Some("claude"), app, true), false, now));
    let state = gate.shown().expect("standing Claude report").0;
    assert_eq!(wire(gate.shown()), "working");
    assert_eq!(row_word(state), "claude working");
    assert_eq!(survey_word(state), "busy");
    term.process(b"\x1b]7501;state=clear:app=claude-code\x1b\\");
    assert!(gate.observe(term.program_state(), true, false, now));
    assert!(gate.shown().is_none());
}

/// ANOTHER PROGRAM'S WORD DECIDES NOTHING (HX-15n), through the real sweep
/// (`App::observe_session_statuses`): a real engine fed `app=kimi-code` reports while a job (a
/// Kimi Code) leads the terminal says them in the words (the hover's `kimi-code <state>` and the
/// survey word, `busy`, `question`, `idle`) and moves nothing a decision reads: the engine's known
/// report and turn count, the gate's shown report (`status program_state=`), the timeline's
/// shown report (the program-claim need's, the A/B book's and the session log's), its
/// `program-state` rows, and the gate's turn counts; a Kimi Code that is the session's root
/// (`aterm -e kimi`) is said the same way. NEGATIVE CONTROLS: codex's own reports under a codex
/// move every one of them; the same `kimi-code` reports with the session's shell in front are not
/// even said.
#[cfg(unix)]
#[test]
fn an_osc_7501_app_word_decides_nothing() {
    use crate::session_timeline::PROGRAM_STATE;
    const SHELL: i32 = 40_000;
    const JOB: i32 = 40_001;
    const K_WORKING: &[u8] = b"\x1b]7501;state=working:app=kimi-code\x1b\\";
    const K_QUESTION: &[u8] = b"\x1b]7501;state=blocked:app=kimi-code:kind=question\x1b\\";
    const K_DONE: &[u8] = b"\x1b]7501;state=done:app=kimi-code\x1b\\";
    const DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";
    /// Each step's `words|decided`, and what the run counted.
    struct Run {
        steps: Vec<String>,
        rows: usize,
        turns: (u64, u64, u64),
    }
    let run = |program: &str, job: bool, stream: &[&[u8]]| -> Run {
        let mut app = crate::App::headless_for_test();
        app.session_factory.fg_probe = |master| master;
        let sid = app.next_session_id;
        let mut session = crate::stub_session(sid);
        let front = if job { JOB } else { SHELL };
        session.master = front;
        session.pid = SHELL;
        app.push_stub_tab(crate::WindowId(0), session);
        let (term, timeline) = {
            let pooled = app.pool.get(sid).expect("pooled");
            (
                std::sync::Arc::clone(&pooled.term),
                std::sync::Arc::clone(&pooled.ctx.timeline),
            )
        };
        {
            let mut tl = timeline.lock().unwrap();
            tl.note_foreground_group(front);
            tl.set_program(front, Some(program.into()));
        }
        let step = std::time::Duration::from_secs(1);
        let mut t = Instant::now();
        let mut steps = Vec::new();
        for bytes in stream {
            crate::term_lock(&term).process(bytes);
            let _ = app.observe_session_statuses(t);
            t += step;
            let said = app
                .session_status
                .program_said(sid)
                .map_or_else(|| "-".into(), |(state, _)| row_word(state));
            let tl = timeline.lock().unwrap();
            let survey = tl
                .program_said()
                .map_or("-", |shown| survey_word(shown.state));
            let engine = crate::term_lock(&term).program_state();
            steps.push(format!(
                "{said} {survey}|{} {} {}",
                wire(app.session_status.program_state(sid)),
                column(tl.program_shown()),
                engine.state.map_or_else(|| "-".to_string(), |s| s.wire()),
            ));
        }
        let rows = timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == PROGRAM_STATE)
            .count();
        let turns = (
            crate::term_lock(&term).program_state().turns,
            app.session_status.program_turn_starts(sid),
            app.session_status.program_turn_ends(sid),
        );
        Run { steps, rows, turns }
    };
    let kimi = run(
        "kimi",
        true,
        &[K_WORKING, K_QUESTION, K_WORKING, K_DONE, CLEAR],
    );
    assert_eq!(
        kimi.steps,
        [
            "kimi-code working busy|- - -",
            "kimi-code blocked question|- - -",
            "kimi-code working busy|- - -",
            "kimi-code done idle|- - -",
            "- -|- - -",
        ],
        "said in the words, and nothing a decision reads moved"
    );
    assert_eq!((kimi.rows, kimi.turns), (0, (0, 0, 0)), "no row, no turn");
    let codex = run(
        "codex",
        true,
        &[WORKING, BLOCKED_PERMISSION, WORKING, DONE, CLEAR],
    );
    assert_eq!(
        codex.steps,
        [
            "codex working busy|working working working",
            "codex blocked prompt|blocked:permission blocked:permission blocked:permission",
            "codex working busy|working working working",
            "codex done idle|done done done",
            "- -|- - -",
        ],
        "CONTROL: codex's reports under a codex move what decides"
    );
    assert_eq!(
        (codex.rows, codex.turns),
        (5, (1, 1, 1)),
        "CONTROL: rows and a turn"
    );
    let rooted = run("kimi", false, &[K_WORKING, K_DONE]);
    assert_eq!(
        rooted.steps,
        ["kimi-code working busy|- - -", "kimi-code done idle|- - -"],
        "a Kimi Code the session runs as its root (`aterm -e kimi`) is said"
    );
    assert_eq!(
        (rooted.rows, rooted.turns),
        (0, (0, 0, 0)),
        "and decides nothing"
    );
    let shell = run("zsh", false, &[K_WORKING, K_DONE]);
    assert_eq!(
        shell.steps,
        ["- -|- - -", "- -|- - -"],
        "CONTROL: with the session's shell in front another program's report is not said"
    );
}

/// ANOTHER PROGRAM'S WORD NEVER STANDS FOR THE NEXT JOB: a report the job that left wrote, which
/// no sweep read before the foreground move, is not said under the job the move brought; that
/// job's own next report is. NEGATIVE CONTROL: with no move, the same unread report is said.
#[test]
fn another_programs_word_from_before_a_move_is_never_said() {
    const K_WORKING: &[u8] = b"\x1b]7501;state=working:app=kimi-code\x1b\\";
    const K_DONE: &[u8] = b"\x1b]7501;state=done:app=kimi-code\x1b\\";
    let said = |gate: &ProgramStateGate| gate.said().map(|(s, _)| s.wire());
    for moved in [true, false] {
        let mut term = Terminal::new(24, 80);
        let mut gate = ProgramStateGate::default();
        let t0 = Instant::now();
        term.process(K_WORKING);
        assert!(gate.observe_other(term.program_state().other, true, false, t0));
        term.process(K_DONE);
        gate.observe_other(term.program_state().other, true, moved, t0);
        if moved {
            assert_eq!(said(&gate), None, "the job that left wrote it");
            term.process(K_WORKING);
            assert!(gate.observe_other(term.program_state().other, true, false, t0));
            assert_eq!(
                said(&gate).as_deref(),
                Some("working"),
                "the next job's own"
            );
        } else {
            assert_eq!(said(&gate).as_deref(), Some("done"), "CONTROL: no move");
        }
    }
}

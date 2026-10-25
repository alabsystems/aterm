// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex};

use crate::session_timeline::SessionTimeline;

fn assert_stamp_rides_last(record: &str) {
    let stamp = record
        .rsplit_once(" seq=")
        .expect("the record ends with the stamp")
        .1;
    let (seq, hash) = stamp.split_once(" hash=").expect("seq= then hash=");
    assert!(
        seq == "-" || seq.chars().all(|c| c.is_ascii_digit()),
        "seq={seq:?} in {record}"
    );
    assert!(
        hash == "-" || (hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit())),
        "hash={hash:?} in {record}"
    );
}
use super::*;

const QUIET: Duration = Duration::from_millis(5_000);
const DWELL: Duration = Duration::from_millis(750);

fn policy() -> StatusPolicy {
    StatusPolicy {
        quiet_after: QUIET,
        dwell: DWELL,
    }
}

#[test]
fn stale_claude_sample_cannot_restart_a_stopped_footer_watch() {
    let now = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(50));
    let timeline = Arc::new(Mutex::new(SessionTimeline::for_test()));
    {
        let mut current = timeline.lock().unwrap();
        current.note_foreground_group(42);
        current.set_program(42, Some("claude".into()));
        current.set_program(42, Some("zsh".into()));
    }
    observer.request_footer_if_due(
        7,
        &timeline,
        Some("claude"),
        42,
        now,
        &crate::claude_footer::LogWake::default(),
        false,
    );
    assert!(!observer.footer_asked.contains_key(&7));

    {
        let mut current = timeline.lock().unwrap();
        current.note_foreground_group(43);
        current.set_program(43, Some("claude".into()));
    }
    observer.request_footer_if_due(
        7,
        &timeline,
        Some("claude"),
        42,
        now,
        &crate::claude_footer::LogWake::default(),
        false,
    );
    assert!(!observer.footer_asked.contains_key(&7));
}

#[test]
fn an_exec_into_a_store_or_native_claude_asks_for_the_footer_again() {
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

    use crate::holder_image::{EdgeKind, HolderImage};

    let now = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(50));
    let timeline = Arc::new(Mutex::new(SessionTimeline::for_test()));
    {
        let mut current = timeline.lock().unwrap();
        current.note_foreground_group(42);
        current.set_program(42, Some("claude".into()));
    }
    let publish = |exe: &str, source: ImageSource, build: u64| {
        let image = HolderImage::of(
            42,
            Some(5),
            Some(std::path::PathBuf::from(exe)),
            ImageBuild {
                program: ProgramTag::new("claude"),
                build,
                source,
            },
            1,
        );
        timeline
            .lock()
            .unwrap()
            .offer_holder_image(Arc::new(image), EdgeKind::Exec);
    };
    let mut visit = |ms: u64, edged: bool| {
        observer.request_footer_if_due(
            7,
            &timeline,
            Some("claude"),
            42,
            now + Duration::from_millis(ms),
            &crate::claude_footer::LogWake::default(),
            edged,
        );
        crate::claude_footer::CAPTURED_ASKS.with(|asks| asks.borrow().as_ref().map_or(0, Vec::len))
    };
    crate::claude_footer::CAPTURED_ASKS.with(|asks| *asks.borrow_mut() = Some(Vec::new()));
    publish("/p/agents/claude", ImageSource::Launcher, 0);
    assert_eq!(visit(0, true), 1, "the first ask, in the shim");
    assert_eq!(visit(1, true), 1, "the shim's image is no drawer");
    publish(
        "/p/pkg/store/claude/1000002000001000304/bin/claude",
        ImageSource::Store,
        1_000_002_000_001_000_304,
    );
    assert_eq!(visit(2, false), 1, "a visit off the edge does not look");
    assert_eq!(visit(3, true), 2, "the exec edge asks inside RECHECK");
    assert_eq!(visit(4, true), 2, "once per image");
    publish(
        "/h/.local/share/claude/versions/2.1.305",
        ImageSource::Native,
        1_000_002_000_001_000_305,
    );
    assert_eq!(visit(5, true), 3, "a Native drawer too");
    publish(
        "/p/pkg/store/claude/1000002000001000306/bin/claude",
        ImageSource::StoreRecord,
        1_000_002_000_001_000_306,
    );
    assert_eq!(visit(6, true), 4, "an ALab build from the store too");
    publish("/opt/elsewhere/claude", ImageSource::Unknown, 0);
    assert_eq!(visit(7, true), 4, "an unclassified claude asks nothing");
    crate::claude_footer::CAPTURED_ASKS.with(|asks| *asks.borrow_mut() = None);
}

fn blank(seq: u64) -> ActivitySample {
    ActivitySample {
        alt_screen: false,
        content_seq: seq,
        last_output: None,
    }
}

fn done(id: u64, exit_code: i32) -> Option<CompletedBlock> {
    Some(CompletedBlock {
        id,
        exit_code: Some(exit_code),
        started_ms: None,
        ended_ms: None,
    })
}

fn evidence(activity: ActivitySample) -> Evidence {
    Evidence {
        shell: None,
        completed_block: None,
        lifecycle: None,
        foreground_job: None,
        activity,
    }
}

fn settle(fsm: &mut StatusFsm, ev: &Evidence, at: Instant) -> Instant {
    fsm.observe(ev, at);
    let later = at + DWELL;
    fsm.observe(ev, later);
    later
}

#[test]
fn a_prompt_only_shell_running_a_job_reads_running() {
    let t0 = Instant::now();
    let run = |shell| {
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = shell;
        ev.foreground_job = Some(true);
        fsm.observe(&ev, t0);
        ev.activity.content_seq = 2;
        ev.activity.last_output = Some(t0 + Duration::from_millis(10));
        settle(&mut fsm, &ev, t0 + Duration::from_millis(10));
        fsm.status().clone()
    };
    let yielded = prompt_yields_to_job(Some(ShellEvidence::Entering), false, Some(true));
    assert_eq!(yielded, None, "the prompt yields to the job");
    let status = run(yielded);
    assert_eq!(status.phase, Phase::Running, "{status:?}");
    assert_eq!(status.reasons.first(), Some(&Reason::ForegroundJob));

    let unyielded = run(Some(ShellEvidence::Entering));
    assert_eq!(
        (unyielded.phase, unyielded.reasons),
        (Phase::Idle, vec![Reason::ShellBlock]),
        "CONTROL: the prompt block alone reads the running job as idle"
    );
}

#[test]
fn only_a_prompt_that_never_reported_a_command_yields_to_a_job() {
    for shell in [ShellEvidence::Prompt, ShellEvidence::Entering] {
        assert_eq!(
            prompt_yields_to_job(Some(shell), true, Some(true)),
            Some(shell),
            "a shell that reports commands keeps its prompt"
        );
        for job in [Some(false), None] {
            assert_eq!(
                prompt_yields_to_job(Some(shell), false, job),
                Some(shell),
                "no live job: the prompt stands ({job:?})"
            );
        }
    }
    for shell in [
        ShellEvidence::Executing,
        ShellEvidence::Complete { exit_code: Some(0) },
    ] {
        assert_eq!(
            prompt_yields_to_job(Some(shell), false, Some(true)),
            Some(shell)
        );
    }
    assert_eq!(prompt_yields_to_job(None, false, Some(true)), None);

    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = prompt_yields_to_job(Some(ShellEvidence::Prompt), true, Some(true));
    ev.foreground_job = Some(true);
    fsm.observe(&ev, t0);
    ev.activity.content_seq = 2;
    settle(&mut fsm, &ev, t0 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Idle);
    assert_eq!(fsm.status().reasons, vec![Reason::ShellBlock]);
}

#[test]
fn prompt_marks_alone_never_report_a_command() {
    use crate::quit_safety::shell_reports_commands;

    let mut cmd = aterm_core::terminal::Terminal::new(24, 80);
    cmd.process(b"\x1b]133;A\x07C:\\> \x1b]133;B\x07ping -n 30 127.0.0.1\r\nReply\r\n");
    assert!(!shell_reports_commands(&cmd));
    assert_eq!(shell_evidence(&cmd), Some(ShellEvidence::Entering));
    cmd.process(b"\x1b]133;A\x07C:\\> \x1b]133;B\x07");
    assert!(
        !shell_reports_commands(&cmd),
        "a second prompt is still no command"
    );

    let mut pwsh = aterm_core::terminal::Terminal::new(24, 80);
    pwsh.process(b"\x1b]133;A\x07PS> \x1b]133;B\x07");
    assert!(
        !shell_reports_commands(&pwsh),
        "a first prompt has reported nothing yet"
    );
    pwsh.process(b"Start-Sleep 1\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    assert!(shell_reports_commands(&pwsh));
}

#[test]
fn every_phase_is_reachable_and_has_a_wire_spelling() {
    let spellings = [
        (Phase::Unknown, "unknown"),
        (Phase::Starting, "starting"),
        (Phase::Idle, "idle"),
        (Phase::Running, "running"),
        (Phase::Quiet, "quiet"),
        (Phase::Exited, "exited"),
    ];
    for (phase, wire) in spellings {
        assert_eq!(phase.as_str(), wire);
    }

    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    assert_eq!(fsm.status().phase, Phase::Starting, "seeded");

    let mut ev = evidence(blank(1));
    settle(&mut fsm, &ev, t0);
    assert_eq!(fsm.status().phase, Phase::Unknown, "no evidence");
    assert_eq!(fsm.status().confidence, Confidence::Unknown);
    assert_eq!(fsm.status().reasons, vec![Reason::NoEvidence]);

    ev.foreground_job = Some(false);
    let t1 = settle(&mut fsm, &ev, t0 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Idle);

    ev.foreground_job = Some(true);
    ev.activity.content_seq = 2;
    let t2 = settle(&mut fsm, &ev, t1 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Running);

    ev.activity.last_output = None;
    settle(&mut fsm, &ev, t2 + QUIET * 2);
    assert_eq!(fsm.status().phase, Phase::Quiet);
    assert_eq!(fsm.status().confidence, Confidence::Heuristic);
}

#[test]
fn alt_screen_transition_is_a_resync_not_activity() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(ActivitySample {
        alt_screen: false,
        content_seq: 900,
        last_output: None,
    });
    ev.foreground_job = Some(true);
    fsm.observe(&ev, t0);

    ev.activity = ActivitySample {
        alt_screen: true,
        content_seq: 3,
        last_output: None,
    };
    settle(&mut fsm, &ev, t0 + Duration::from_millis(10));

    assert_eq!(
        fsm.status().phase,
        Phase::Quiet,
        "an alt-screen sequence restart must not count as movement"
    );
}

#[test]
fn executing_names_output_only_movement_as_output_activity() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Executing);
    fsm.observe(&ev, t0);
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t0);
    let t1 = settle(&mut fsm, &ev, t0 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Running);
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock, Reason::ContentActivity],
        "the work started with the grid moving"
    );

    let t2 = t1 + QUIET;
    ev.activity.last_output = Some(t2);
    fsm.observe(&ev, t2);
    assert_eq!(fsm.status().phase, Phase::Running);
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock, Reason::OutputActivity],
        "bytes arrived and the grid never moved"
    );

    ev.activity.content_seq = 3;
    ev.activity.last_output = Some(t2 + Duration::from_millis(10));
    fsm.observe(&ev, t2 + Duration::from_millis(10));
    assert_eq!(fsm.status().phase, Phase::Running);
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock, Reason::ContentActivity],
        "a moved grid is content activity"
    );
}

#[test]
fn output_that_moves_no_cell_sustains_running_but_never_starts_it() {
    for (shell, job, rest) in [
        (None, Some(true), Phase::Quiet),
        (Some(ShellEvidence::Executing), Some(true), Phase::Quiet),
        (None, None, Phase::Unknown),
    ] {
        let t0 = Instant::now();
        let mut fsm = StatusFsm::new(policy(), t0);
        let mut ev = evidence(blank(1));
        ev.shell = shell;
        ev.foreground_job = job;
        let t1 = settle(&mut fsm, &ev, t0);
        let case = format!("{shell:?} {job:?}");
        assert_eq!(fsm.status().phase, rest, "{case}: at rest");
        assert_eq!(fsm.owed_wake(), None, "{case}");

        for n in 1..=60 {
            let at = t1 + Duration::from_secs(n);
            ev.activity.last_output = Some(at);
            assert!(!fsm.observe(&ev, at), "{case}: nothing to publish");
            assert_eq!(fsm.status().phase, rest, "{case}");
            assert_eq!(fsm.owed_wake(), None, "{case}: no sweep is owed");
        }

        let t2 = t1 + Duration::from_secs(61);
        ev.activity.content_seq = 2;
        ev.activity.last_output = Some(t2);
        fsm.observe(&ev, t2);
        assert_eq!(fsm.owed_wake(), Some(t2 + DWELL), "{case}: the dwell");
        fsm.observe(&ev, t2 + DWELL);
        assert_eq!(fsm.status().phase, Phase::Running, "{case}");
        assert_eq!(fsm.owed_wake(), Some(t2 + QUIET), "{case}: its retirement");
    }
}

#[test]
fn executing_without_a_foreground_job_is_flagged_as_conflicting() {
    let t0 = Instant::now();
    let mut fsm = StatusFsm::new(policy(), t0);
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Executing);
    ev.foreground_job = Some(false);

    settle(&mut fsm, &ev, t0);

    assert!(
        fsm.status().conflict,
        "contradictory evidence must be visible"
    );
}

fn published(phase: Phase, last_outcome: Outcome) -> Status {
    Status {
        phase,
        last_outcome,
        since: Instant::now(),
        detail: None,
        confidence: Confidence::Strong,
        reasons: Vec::new(),
        conflict: false,
    }
}

#[test]
fn a_ctrl_c_exit_reads_as_interrupted() {
    let text = |phase, exit_code| {
        summary_text(&published(phase, Outcome::Failure { exit_code })).expect("text")
    };
    assert_eq!(
        WINDOWS_CONTROL_C_EXIT,
        i32::from_ne_bytes(0xC000_013A_u32.to_ne_bytes())
    );
    assert_eq!(text(Phase::Idle, WINDOWS_CONTROL_C_EXIT), "interrupted");
    assert_eq!(
        text(Phase::Exited, WINDOWS_CONTROL_C_EXIT),
        "exited (interrupted)"
    );
    let bash_ctrl_c = if cfg!(windows) {
        "interrupted"
    } else {
        "failed (exit 130)"
    };
    assert_eq!(text(Phase::Idle, 130), bash_ctrl_c);
    assert_eq!(text(Phase::Idle, 1), "failed (exit 1)");
    assert_eq!(
        text(Phase::Idle, -1_073_741_819),
        "failed (exit -1073741819)"
    );
    assert_eq!(text(Phase::Exited, 2), "exited (exit 2)");
    assert!(
        Outcome::Failure {
            exit_code: WINDOWS_CONTROL_C_EXIT
        }
        .is_failure()
    );
}

#[test]
fn indicators_map_work_and_failure_without_claiming_unsaved_state() {
    use crate::tab_model::TabIndicators;

    let cases = [
        (Phase::Running, Outcome::None, (true, false)),
        (Phase::Exited, Outcome::None, (false, false)),
        (
            Phase::Idle,
            Outcome::Failure { exit_code: 1 },
            (false, true),
        ),
        (Phase::Exited, Outcome::Signal { signal: 9 }, (false, true)),
        (
            Phase::Running,
            Outcome::Failure { exit_code: 2 },
            (true, true),
        ),
        (Phase::Idle, Outcome::Success, (false, false)),
        (Phase::Quiet, Outcome::None, (true, false)),
        (Phase::Starting, Outcome::None, (false, false)),
        (Phase::Unknown, Outcome::None, (false, false)),
    ];
    for (phase, outcome, (busy, attention)) in cases {
        assert_eq!(
            status_indicators(&published(phase, outcome)),
            TabIndicators {
                dirty: false,
                busy,
                attention: false,
                status_attention: attention,
            },
            "{phase:?} with {outcome:?}"
        );
    }
}

#[test]
fn a_background_pane_going_busy_marks_its_tab() {
    use crate::tab_model::TabIndicators;

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let focused = app.split_active_stub_tab(wid);
    assert_ne!(focused, 0, "the split spawns a second session");

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    assert!(!app.session_status.observe(0, &ev, t0), "dwell holds first");
    let t1 = t0 + DWELL;
    ev.activity.last_output = Some(t1);
    assert!(app.session_status.observe(0, &ev, t1), "Running publishes");

    app.refresh_session_status_chrome(0);

    let tab = app.windows[&wid]
        .tab_set
        .active()
        .expect("active tab")
        .presentation
        .indicators;
    assert_eq!(
        tab,
        TabIndicators {
            dirty: false,
            busy: true,
            attention: false,
            status_attention: false,
        }
    );
    assert!(
        app.tab_strip_metadata(wid).first().expect("one tab").busy,
        "the bits both strip renderers read must carry it"
    );
}

#[test]
fn a_cleared_failure_releases_the_tab_while_a_native_sibling_keeps_its_own_mark() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let native = app
        .view_store
        .insert_native(crate::tab_model::AppInstanceId::from_stored(7))
        .expect("native view identity space");
    let split = app
        .windows
        .get_mut(&wid)
        .and_then(|window| window.tab_set.active_mut())
        .is_some_and(|tab| tab.split_focused(crate::tab_model::SplitAxis::Vertical, native));
    assert!(split, "mixed terminal + native tab");
    let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(
        attention_of(&app, wid, tab_id),
        "a failed command marks the tab"
    );

    app.windows
        .get_mut(&wid)
        .and_then(|window| window.tab_set.active_mut())
        .expect("active tab")
        .presentation
        .indicators
        .attention = true;

    ev.shell = Some(ShellEvidence::Executing);
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t0 + Duration::from_millis(10));
    settle_observer(
        &mut app.session_status,
        0,
        &ev,
        t0 + Duration::from_millis(10),
    );
    assert_eq!(
        app.session_status.status(0).map(|s| s.last_outcome),
        Some(Outcome::None),
        "the classifier itself has let go of the failure"
    );
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(
        attention_of(&app, wid, tab_id),
        "the NATIVE sibling's out-of-band mark survives"
    );

    app.windows
        .get_mut(&wid)
        .and_then(|window| window.tab_set.active_mut())
        .expect("active tab")
        .presentation
        .indicators
        .attention = false;
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(
        !attention_of(&app, wid, tab_id),
        "the terminal's own bit must be clearable, not self-sustaining"
    );
}

fn attention_of(app: &crate::App, wid: crate::WindowId, tab_id: crate::tab_model::TabId) -> bool {
    app.windows[&wid]
        .tab_set
        .get(tab_id)
        .expect("tab")
        .presentation
        .indicators
        .wants_attention()
}

fn settle_observer(
    observer: &mut StatusObserver,
    session: u64,
    ev: &Evidence,
    at: Instant,
) -> Instant {
    observer.observe(session, ev, at);
    let later = at + DWELL;
    observer.observe(session, ev, later);
    later
}

#[test]
fn the_status_sweep_records_one_status_change_per_published_transition() {
    let mut app = crate::App::headless_for_test();
    let front = app
        .front_terminal(crate::WindowId(0))
        .expect("front terminal");
    let (session, term) = (front.session, front.term.clone());
    let timeline = app.pool.get(session).expect("pooled").ctx.timeline.clone();
    let status_events =
        |timeline: &std::sync::Arc<std::sync::Mutex<crate::session_timeline::SessionTimeline>>| {
            timeline
                .lock()
                .unwrap()
                .since(None)
                .filter(|e| e.kind == STATUS_CHANGE)
                .map(|e| e.payload.to_string())
                .collect::<Vec<_>>()
        };
    const NEXT_PROMPT: &[u8] = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    let run = |exit: &str| {
        let mut bytes = format!("\x1b]133;C\x1b\\\x1b]133;D;{exit}\x1b\\").into_bytes();
        bytes.extend_from_slice(NEXT_PROMPT);
        term.lock().unwrap().process(&bytes);
    };
    term.lock().unwrap().process(NEXT_PROMPT);
    run("7");
    let t0 = Instant::now();
    let _ = app.observe_session_statuses(t0);
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(1));
    let events = status_events(&timeline);
    assert_eq!(
        events,
        vec!["phase=idle outcome=failure exit_code=7 signal=- confidence=strong".to_string()],
        "one event for the one published transition"
    );
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(2));
    assert_eq!(
        status_events(&timeline).len(),
        1,
        "no event without a transition"
    );
    assert_eq!(
        crate::subscribe::timeline_wire_kind(STATUS_CHANGE),
        Some("status"),
        "the events digest pushes it"
    );

    let attention = |app: &crate::App| {
        app.session_status
            .indicators(session)
            .expect("published")
            .status_attention
    };
    assert!(attention(&app), "an unseen failure marks the tab");
    assert!(app.session_status.note_seen(session), "looking quiets it");
    assert!(
        !app.session_status.note_seen(session),
        "looking again quiets nothing"
    );
    assert!(
        app.session_status
            .status(session)
            .is_some_and(|s| s.last_outcome.is_failure()),
        "the RECORD keeps the failure; only the mark went quiet"
    );
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(3));
    assert!(!attention(&app), "the same failure stays seen");
    run("7");
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(4));
    assert!(attention(&app), "a new failing block re-arms the mark");
    assert_eq!(
        status_events(&timeline).len(),
        1,
        "the second failure is not a status change"
    );

    run("0");
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(5));
    assert_eq!(
        status_events(&timeline).last().map(String::as_str),
        Some("phase=idle outcome=success exit_code=- signal=- confidence=strong"),
    );
    assert!(!attention(&app), "a success carries no mark");
}

#[test]
fn a_prompt_reports_the_block_it_closed_and_no_older_one() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    let prompt = b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\";
    term.process(prompt);
    assert_eq!(
        completed_block(&term),
        None,
        "a first prompt closed nothing"
    );

    term.process(b"\x1b]133;C\x1b\\");
    assert_eq!(
        completed_block(&term),
        None,
        "a running command reports nothing"
    );
    term.process(b"\x1b]133;D;3\x1b\\");
    let failed = completed_block(&term).expect("the current block completed");
    assert_eq!(failed.exit_code, Some(3));
    term.process(prompt);
    assert_eq!(
        completed_block(&term),
        Some(failed),
        "the prompt reports the block it closed"
    );
    assert_eq!(shell_evidence(&term), Some(ShellEvidence::Entering));

    term.process(prompt);
    assert_eq!(
        completed_block(&term),
        None,
        "a prompt that closed an empty block reports no older result"
    );

    term.process(b"\x1b]133;C\x1b\\");
    term.process(prompt);
    assert_eq!(completed_block(&term), None);
}

#[test]
fn a_scrollback_erase_never_leaves_the_status_on_an_earlier_exit() {
    let mut app = crate::App::headless_for_test();
    let front = app
        .front_terminal(crate::WindowId(0))
        .expect("front terminal");
    let (session, term) = (front.session, front.term.clone());
    const PROMPT: &[u8] = b"\x1b]133;A\x1b\\$ \x1b]133;B\x1b\\";
    let run = |command: &str, body: &[u8], exit: i32| {
        let mut bytes = format!("{command}\r\n\x1b]133;C\x1b\\").into_bytes();
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(format!("\x1b]133;D;{exit}\x1b\\").as_bytes());
        bytes.extend_from_slice(PROMPT);
        term.lock().unwrap().process(&bytes);
    };
    let t0 = Instant::now();
    let mut at = 0;
    let mut published = |app: &mut crate::App| {
        for _ in 0..2 {
            at += 1;
            let _ = app.observe_session_statuses(t0 + Duration::from_secs(at));
        }
        let status = app.session_status.status(session).expect("published");
        status_event_payload(status)
    };
    term.lock().unwrap().process(PROMPT);
    run("sh -c 'exit 6'", b"", 6);
    assert_eq!(
        published(&mut app),
        "phase=idle outcome=failure exit_code=6 signal=- confidence=strong"
    );
    run("sh -c 'printf \"\\033[3J\"; exit 5'", b"\x1b[3J", 5);
    assert_eq!(
        published(&mut app),
        "phase=idle outcome=failure exit_code=5 signal=- confidence=strong",
        "ED 3 while the command ran"
    );
    run("clear; sh -c 'exit 4'", b"\x1b[H\x1b[2J\x1b[3J", 4);
    assert_eq!(
        published(&mut app),
        "phase=idle outcome=failure exit_code=4 signal=- confidence=strong",
        "clear while the command ran"
    );
    let mut body = b"line\r\n".repeat(40);
    body.extend_from_slice(b"\x1b[H\x1b[2J");
    run("seq 40; printf '\\033[H\\033[2J'; exit 3", &body, 3);
    term.lock().unwrap().clear_scrollback();
    assert!(
        term.lock().unwrap().last_completed_block().is_none(),
        "PRECONDITION: the Cmd-K took the finished block with its rows"
    );
    assert_eq!(
        published(&mut app),
        "phase=idle outcome=failure exit_code=3 signal=- confidence=strong",
        "Cmd-K at the prompt took the finished block before a sweep read it"
    );

    run("sh -c 'exit 2'", b"", 2);
    run("reset", b"\x1bc", 0);
    assert_eq!(
        term.lock()
            .unwrap()
            .last_finished_block()
            .map(|b| b.exit_code),
        Some(Some(2)),
        "PRECONDITION: the last D's copy holds an exit other than the published one"
    );
    assert_eq!(
        published(&mut app),
        "phase=idle outcome=failure exit_code=3 signal=- confidence=strong",
        "a RIS swallows the D, and the copy of a block this prompt did not close is not read"
    );
}

#[test]
fn a_session_without_shell_integration_arms_its_mark_on_the_published_edge() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(250));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    observer.observe(1, &ev, t0);
    let exit = Lifecycle::Exited { exit_code: Some(3) };
    observer.note_exit(1, exit, &ev, t0 + Duration::from_millis(10));
    assert!(observer.indicators(1).expect("published").status_attention);
    assert!(observer.note_seen(1));
    observer.observe(1, &ev, t0 + Duration::from_millis(400));
    assert!(
        !observer.indicators(1).expect("published").status_attention,
        "the held exit is one failure, seen once"
    );
}

#[test]
fn rapid_heuristic_transitions_append_at_most_one_status_event_per_dwell() {
    let fast = StatusPolicy {
        quiet_after: Duration::from_millis(100),
        dwell: DWELL,
    };
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(fast, Duration::from_millis(0));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    let mut events = Vec::new();
    let span = Duration::from_secs(10);
    let step = Duration::from_millis(50);
    let mut at = t0;
    let mut seq = 1;
    while at < t0 + span {
        // Output for 150 ms, then 150 ms of silence: Running and Quiet
        // candidates alternate every few samples.
        if (at - t0).as_millis() % 300 < 150 {
            seq += 1;
            ev.activity.content_seq = seq;
            ev.activity.last_output = Some(at);
        }
        if observer.observe(1, &ev, at)
            && let Some(payload) = observer.take_status_event(1)
        {
            events.push(payload);
        }
        at += step;
    }
    let bound = (span.as_millis() / DWELL.as_millis()) as usize + 1;
    assert!(
        events.len() <= bound,
        "{} status events in {span:?} (bound {bound}): {events:?}",
        events.len()
    );
    let mut steady = evidence(blank(1));
    steady.foreground_job = Some(true);
    steady.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    steady.completed_block = done(4, 1);
    settle_observer(&mut observer, 1, &steady, at);
    let payload = observer
        .take_status_event(1)
        .expect("a surviving transition");
    assert_eq!(
        payload,
        "phase=idle outcome=failure exit_code=1 signal=- confidence=strong"
    );
    assert_eq!(
        observer.take_status_event(1),
        None,
        "one event per transition"
    );
}

#[test]
fn an_exited_pane_stays_exited_instead_of_reverting_to_ready() {
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    app.session_status.observe(0, &ev, t0);
    ev.activity.content_seq += 1;
    settle_observer(&mut app.session_status, 0, &ev, t0);
    assert_eq!(
        app.session_status.status(0).map(|s| s.phase),
        Some(Phase::Running)
    );

    app.note_session_exit(0);
    assert_eq!(
        app.session_status.status(0).map(|s| s.phase),
        Some(Phase::Exited),
        "exit is Exact and bypasses dwell"
    );
    assert_eq!(
        app.session_status.status(0).map(|s| s.last_outcome),
        Some(Outcome::None),
        "an uncollectable status is unknown, NEVER success"
    );

    let mut after = evidence(blank(2));
    after.foreground_job = Some(false);
    let t1 = t0 + DWELL * 4;
    settle_observer(&mut app.session_status, 0, &after, t1);
    assert_eq!(
        app.session_status.status(0).map(|s| s.phase),
        Some(Phase::Exited),
        "a dead pane must not claim to be a shell at a prompt"
    );
}

#[test]
fn a_collected_exit_code_becomes_the_outcome() {
    let t0 = Instant::now();
    let cases = [
        (
            Lifecycle::Exited { exit_code: Some(0) },
            Outcome::Success,
            false,
        ),
        (
            Lifecycle::Exited { exit_code: Some(7) },
            Outcome::Failure { exit_code: 7 },
            true,
        ),
        (Lifecycle::Exited { exit_code: None }, Outcome::None, false),
        (
            Lifecycle::Signalled { signal: 9 },
            Outcome::Signal { signal: 9 },
            true,
        ),
    ];
    for (lifecycle, outcome, attention) in cases {
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
        let ev = evidence(blank(1));
        assert!(observer.note_exit(1, lifecycle, &ev, t0), "{lifecycle:?}");
        let status = observer.status(1).expect("published");
        assert_eq!(status.phase, Phase::Exited, "{lifecycle:?}");
        assert_eq!(status.last_outcome, outcome, "{lifecycle:?}");
        assert_eq!(status.confidence, Confidence::Exact, "{lifecycle:?}");
        assert_eq!(status.reasons, vec![Reason::LifecycleExit], "{lifecycle:?}");
        assert_eq!(
            status_indicators(status).status_attention,
            attention,
            "{lifecycle:?}"
        );
    }
}

#[test]
fn a_policy_edit_reaches_live_sessions_and_reopens_the_deadline() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(250));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    settle_observer(&mut observer, 1, &ev, t0);

    ev.foreground_job = Some(false);
    let t1 = t0 + DWELL;
    observer.observe(1, &ev, t1);
    let before = observer
        .next_wake()
        .expect("a pending candidate owes a wake");
    assert_eq!(before, t1 + DWELL);

    assert!(
        observer.reconfigure(
            StatusPolicy {
                quiet_after: QUIET,
                dwell: Duration::from_millis(10),
            },
            Duration::from_millis(10),
            t1,
        ),
        "a real change reports that it moved"
    );
    let after = observer.next_wake().expect("still owed");
    assert_eq!(
        after,
        t1 + Duration::from_millis(10),
        "the LIVE session's dwell moved, not just the observer's field"
    );
    assert!(
        observer.due(1, t1 + Duration::from_millis(10)),
        "the shortened interval takes effect now, not after one old interval"
    );
    assert!(
        !observer.reconfigure(
            StatusPolicy {
                quiet_after: QUIET,
                dwell: Duration::from_millis(10),
            },
            Duration::from_millis(10),
            t1,
        ),
        "a no-op edit costs no fan-out"
    );
}

#[test]
fn turning_tab_status_off_stops_classifying_and_retires_the_records() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    app.refresh_tab_status_indicators(wid, tab_id);
    assert!(attention_of(&app, wid, tab_id));

    app.config.tab_status = Some(false);
    app.reconfigure_session_status();
    assert!(
        app.session_status.status(0).is_none(),
        "a stale phase must not sit on a tab after the classifier stops"
    );
    assert!(
        !attention_of(&app, wid, tab_id),
        "and the chrome settles in the same pass"
    );
    assert!(
        app.observe_session_statuses(t0 + DWELL * 4).is_empty(),
        "the sweep returns before it touches the pool"
    );
    app.note_session_exit(0);
    assert!(
        app.session_status.status(0).is_none(),
        "not even the exit edge classifies while the subsystem is off"
    );
}

#[test]
fn the_badge_switch_quiets_chrome_without_stopping_classification() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    let tab_id = app.windows[&wid].tab_set.active().expect("active tab").id;
    app.config.tab_status_badge = Some(false);

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(1) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    app.refresh_tab_status_indicators(wid, tab_id);

    assert!(!attention_of(&app, wid, tab_id), "no mark on the tab");
    assert_eq!(
        app.session_status.status(0).map(|s| s.last_outcome),
        Some(Outcome::Failure { exit_code: 1 }),
        "the record is still classified and still readable"
    );
}

#[test]
fn the_status_record_and_the_roster_carry_land_at_and_task() {
    use aterm_agent::harness::acts::TaskFace;
    let mut app = crate::App::headless_for_test();
    let status = |app: &crate::App| app.session_status_record(0).expect("live session");
    let roster = |app: &crate::App| {
        crate::control::cmd_sessions_store(&app.store, None)
            .lines()
            .nth(1)
            .expect("one session")
            .to_string()
    };
    let field = |line: &str, key: &str| {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} in {line}"))
            .to_string()
    };
    let record = status(&app);
    assert!(
        record.contains(
            " flaps=0 land_at=- task=- pause=- land_held=- land_nudges=- land_since=- end=- end_seen=- \
             end_cheered=- task_state=- task_rev=0 log_seq=1 log_ep=1 run=- \
             turn=- step=- need=- need_since_ms=- rollout=- rollout_asks=0 editing=0 bells=0 bells_sounded=0 last_bell=- gen="
        ),
        "{record}"
    );
    assert_stamp_rides_last(&record);
    assert!(
        roster(&app).ends_with(" upgrade=- land_at=- task=- pause=- task_state=- task_rev=0"),
        "{}",
        roster(&app)
    );

    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    ctx.meta.lock().unwrap().land_at = Some(1_792_249_200_999);
    assert_eq!(field(&status(&app), "land_at="), "-", "no agent runs here");
    assert_eq!(field(&roster(&app), "land_at="), "-", "no agent runs here");
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    assert_eq!(
        field(&status(&app), "land_at="),
        "1792249200",
        "unix seconds"
    );
    assert_eq!(field(&roster(&app), "land_at="), "1792249200");

    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    assert_eq!(
        ctx.timeline.lock().unwrap().task_face().map(|(f, _)| f),
        Some(TaskFace::Landed)
    );
    assert_eq!(field(&status(&app), "task="), "-", "no live claim here");
    assert_eq!(field(&roster(&app), "task="), "-", "no live claim here");
    crate::session_timeline::claim_supervisor(
        &ctx,
        crate::autopilot::own_holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    assert_eq!(field(&status(&app), "task="), "landed");
    assert_eq!(field(&roster(&app), "task="), "landed");
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Theirs)));
    assert_eq!(field(&status(&app), "task="), "theirs");
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(None));
    assert_eq!(field(&status(&app), "task="), "-", "unknown again");
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert_eq!(field(&status(&app), "task="), "open");
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    crate::session_timeline::claim_supervisor(
        &ctx,
        "aterm-supervise:9",
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    assert_eq!(field(&status(&app), "task="), "-", "another holder's claim");
    assert_eq!(field(&roster(&app), "task="), "-", "another holder's claim");
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
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
    ctx.timeline
        .lock()
        .unwrap()
        .queue_self_task(TaskFace::Landed);
    app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(TaskFace::Landed));
    assert_eq!(field(&status(&app), "task="), "landed", "the self-claim's");
    assert_eq!(field(&roster(&app), "task="), "landed", "the self-claim's");
    app.apply_harness_fact(&sid, crate::HarnessFact::Task(Some(TaskFace::Open)));
    assert_eq!(
        field(&status(&app), "task="),
        "landed",
        "the hosted loop tells no face beside a proven self-claim"
    );
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(field(&status(&app), "task="), "-", "no live claim");
    assert_eq!(field(&roster(&app), "task="), "-", "no live claim");
    app.apply_harness_fact(
        "no-such-sid",
        crate::HarnessFact::Task(Some(TaskFace::Open)),
    );
    let record = status(&app);
    assert_stamp_rides_last(&record);
}

#[test]
fn the_status_record_and_the_roster_carry_the_pause_after_task() {
    const AT: i64 = 1_792_249_260;
    let mut app = crate::App::headless_for_test();
    let status = |app: &crate::App| app.session_status_record(0).expect("live session");
    let roster = |app: &crate::App| {
        crate::control::cmd_sessions_store(&app.store, None)
            .lines()
            .nth(1)
            .expect("one session")
            .to_string()
    };
    let field = |line: &str, key: &str| {
        line.split_whitespace()
            .find_map(|t| t.strip_prefix(key))
            .unwrap_or_else(|| panic!("{key} in {line}"))
            .to_string()
    };
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    let pause = crate::HarnessFact::Pause(Some(crate::PausePlan {
        at_unix: AT,
        short: 3,
    }));
    crate::session_timeline::claim_supervisor(
        &ctx,
        crate::autopilot::own_holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    app.apply_harness_fact(&sid, pause);
    assert_eq!(field(&status(&app), "pause="), "-", "no agent runs here");
    {
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    let record = status(&app);
    assert!(
        record.contains(&format!(
            " task=- pause={AT} land_held=- land_nudges=- land_since=- end=- end_seen=- end_cheered=- \
             task_state=- task_rev=0 log_seq="
        )),
        "{record}"
    );
    assert_stamp_rides_last(&record);
    assert!(
        roster(&app).ends_with(&format!(" task=- pause={AT} task_state=- task_rev=0")),
        "{}",
        roster(&app)
    );
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(None));
    assert_eq!(field(&status(&app), "pause="), "-", "the pause is over");
    assert_eq!(field(&roster(&app), "pause="), "-");
    app.apply_harness_fact(&sid, pause);
    assert_eq!(field(&status(&app), "pause="), AT.to_string());
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(field(&status(&app), "pause="), "-", "no live claim here");
    crate::session_timeline::claim_supervisor(
        &ctx,
        "aterm-supervise:9",
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
    assert_eq!(
        field(&status(&app), "pause="),
        "-",
        "another holder's claim"
    );
    assert_eq!(
        field(&roster(&app), "pause="),
        "-",
        "another holder's claim"
    );
}

#[test]
fn the_status_record_carries_the_effective_autopilot_beside_supervisor() {
    let mut app = crate::App::headless_for_test();
    let field = |app: &crate::App| {
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(" supervisor=- supervisor_proven=0 autopilot="),
            "{record}"
        );
        record
            .split_whitespace()
            .find_map(|t| t.strip_prefix("autopilot="))
            .unwrap()
            .to_string()
    };
    assert_eq!(field(&app), "-", "no agent");
    {
        let ctx = app.pool.get(0).unwrap().ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("claude".into()));
    }
    assert_eq!(field(&app), "off", "an agent no host supervises");
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    assert_eq!(field(&app), "cruise", "the table's default");
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    ctx.meta
        .lock()
        .unwrap()
        .set("autopilot", Some("land".into()));
    assert_eq!(field(&app), "land", "the session's own word");
    let observed = {
        let mut ev = evidence(blank(1));
        ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
        settle_observer(&mut app.session_status, 0, &ev, Instant::now());
        app.session_status_record(0).expect("live session")
    };
    assert!(observed.contains(" observed=true "), "{observed}");
    assert!(
        observed.contains(" supervisor=- supervisor_proven=0 autopilot=land human_ms="),
        "{observed}"
    );
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    assert_eq!(field(&app), "off", "the master switch wins over the word");
    host.shutdown_and_join();
}

#[test]
fn the_status_record_judges_the_launch_witness_by_its_own_count() {
    use crate::launch_witness::{Launch, Why};
    let app = crate::App::headless_for_test();
    let ctx = app.pool.get(0).expect("live session").ctx.clone();
    ctx.launch.mark_for_test(Launch {
        id: 3,
        seq: 0,
        word: Some("claude"),
        why: Why::Person,
    });
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" human_seq=0 launch=person launch_seq=0 path_evidence="),
        "{record}"
    );
    assert_stamp_rides_last(&record);
    crate::app_input::note_person(&ctx, crate::human_input::Gesture::Other);
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" human_seq=1 launch=since launch_seq=0 path_evidence="),
        "{record}"
    );
    assert_stamp_rides_last(&record);
}

#[test]
fn half_a_frame_is_no_screen_to_stamp() {
    let app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("live session").term.clone();
    crate::term_lock(&term).process(b"\x1b[?2026h\x1b[H\x1b[2Jready\x1b[?2026l");
    crate::term_lock(&term).process(b"\x1b[?2026h\x1b[H\x1b[2Jhalf");
    let record = app.session_status_record(0).expect("live session");
    assert!(record.ends_with(" seq=- hash=-"), "{record}");
    let generation = crate::control::screen_gen(&crate::term_lock(&term)).to_string();
    assert!(record.contains(&format!(" gen={generation} ")), "{record}");

    crate::term_lock(&term).process(b" and whole\x1b[?2026l");
    let record = app.session_status_record(0).expect("live session");
    let (seq, hash) = {
        let t = crate::term_lock(&term);
        (t.content_seq(), crate::frame_memo::ScreenText::of(&t).fnv())
    };
    assert!(
        record.ends_with(&format!(" seq={seq} hash={hash:016x}")),
        "{record}"
    );
}

#[test]
fn status_reports_editing_count() {
    use crate::control_edit::hold::{self, Hold};
    use amacs::protocol::edit::Ticket;

    let mut app = crate::App::headless_for_test();
    let sid = 1 << 44;
    app.next_session_id = sid;
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
    let editing = |app: &crate::App| {
        let record = app.session_status_record(sid).expect("live session");
        assert_stamp_rides_last(&record);
        record
            .split(' ')
            .find_map(|word| word.strip_prefix("editing="))
            .map(str::to_string)
            .unwrap_or_else(|| panic!("no editing= in {record}"))
    };
    assert_eq!(editing(&app), "0", "nothing holds it");
    hold::set(
        sid,
        Hold {
            ticket: Ticket(1),
            view: 10,
        },
    );
    assert_eq!(editing(&app), "1");
    hold::set(
        sid,
        Hold {
            ticket: Ticket(2),
            view: 20,
        },
    );
    assert_eq!(editing(&app), "2", "two edits, counted apart");
    hold::clear(sid, Ticket(2));
    assert_eq!(editing(&app), "1", "the first resolve leaves the other");
    hold::set_expiry(sid, Ticket(1), Some(Instant::now()));
    assert_eq!(editing(&app), "0", "an orphan's hold lapsed");
    hold::clear(sid, Ticket(1));
}

#[test]
fn fabric_batch_matches_single_status_fields_for_each_session() {
    let mut app = crate::App::headless_for_test();
    let second = crate::stub_session(1);
    crate::App::register_session(&app.store, &second, None);
    app.pool.insert(second);

    let assert_same = |app: &crate::App| {
        let batch = app.session_statuses_record();
        let mut lines = batch.lines();
        assert_eq!(lines.next(), Some("OK 2"), "{batch}");
        for local in [0, 1] {
            let h = app
                .store
                .read()
                .unwrap()
                .by_local(local)
                .expect("registered session")
                .clone();
            let row = lines.next().expect("one row per live session");
            let prefix = format!("{local} {} {} ", h.sid.as_str(), h.nonce.to_hex());
            let fields = row.strip_prefix(&prefix).expect("stable sid and nonce");
            let single = app.session_status_record(local).expect("live session");
            for key in ["sid=", "revision=", "hold=", "detail=", "agent="] {
                let batch_value = fields
                    .split_whitespace()
                    .find(|part| part.starts_with(key))
                    .expect("batch field present");
                let single_value = single
                    .split_whitespace()
                    .find(|part| part.starts_with(key))
                    .expect("single field present");
                assert_eq!(batch_value, single_value, "{key}: {row}");
            }
        }
        assert!(lines.next().is_none(), "framed count is exact: {batch}");
    };

    let locked_term = app.pool.get(0).unwrap().term.clone();
    let guard = locked_term.lock().unwrap();
    assert_same(&app);
    drop(guard);
    assert!(
        app.fabric_status_fields(0)
            .unwrap()
            .contains(" hold=0 hold_origin=- ")
    );

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(&mut app.session_status, 0, &ev, t0);
    assert!(
        app.session_status
            .set_detail(0, Some("Claude Code".to_string()))
    );
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    crate::fabric::apply_hold_for_test(
        &ctx,
        Some(crate::fabric::Hold {
            reason: "test".to_string(),
            origin: "fleet".to_string(),
        }),
    );
    let guard = locked_term.lock().unwrap();
    assert_same(&app);
    assert!(
        app.fabric_status_fields(0)
            .unwrap()
            .contains("detail=Claude%20Code")
    );
    drop(guard);
    assert!(
        app.fabric_status_fields(0)
            .unwrap()
            .contains(" hold=1 hold_origin=fleet ")
    );

    let ctx = app.pool.get(1).unwrap().ctx.clone();
    crate::fabric::apply_hold_for_test(
        &ctx,
        Some(crate::fabric::Hold {
            reason: "mine".to_string(),
            origin: "local".to_string(),
        }),
    );
    assert_same(&app);
    assert!(
        app.fabric_status_fields(1)
            .unwrap()
            .contains(" hold=1 hold_origin=local ")
    );
}

#[test]
fn fabric_batch_does_not_wait_for_a_background_timeline_writer() {
    use std::sync::mpsc;

    let app = crate::App::headless_for_test();
    let timeline = app.pool.get(0).expect("session").ctx.timeline.clone();
    {
        let mut guard = timeline.lock().unwrap();
        guard.publish_agent(
            "prompt",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _guard = timeline.lock().unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(60)).is_ok()
    });
    held_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the writer took the timeline lock");

    let batch = app.session_statuses_record();
    let _ = release_tx.send(());
    assert!(
        worker.join().unwrap(),
        "a Fabric roster read waited on the main thread for the background \
         timeline writer to let go"
    );
    assert!(batch.contains("agent_deferred=1"), "{batch}");
    assert!(
        !batch.contains("agent=-"),
        "a busy writer is not no agent: {batch}"
    );

    let settled = app.session_statuses_record();
    assert!(settled.contains("agent=prompt"), "{settled}");
}

#[test]
fn the_status_record_keeps_one_field_set_on_both_arms() {
    crate::fabric::with_link_reset(the_status_record_keeps_one_field_set_on_both_arms_body);
}

fn the_status_record_keeps_one_field_set_on_both_arms_body() {
    let keys = |record: &str| {
        record
            .split(' ')
            .map(|word| {
                word.split_once('=')
                    .map_or(word, |(key, _)| key)
                    .to_string()
            })
            .collect::<Vec<_>>()
    };
    let mut app = crate::App::headless_for_test();
    let unobserved = app.session_status_record(0).expect("live session");
    assert!(unobserved.starts_with("schema=1 sid=0 "), "{unobserved}");
    for field in [" observed=false ", " phase=unknown ", " revision=0 "] {
        assert!(unobserved.contains(field), "{field}: {unobserved}");
    }

    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(3) });
    settle_observer(&mut app.session_status, 0, &ev, Instant::now());
    let observed = app.session_status_record(0).expect("live session");
    for field in [
        " observed=true phase=idle ",
        " outcome=failure exit_code=3 signal=- ",
        " confidence=strong reasons=shell_block ",
    ] {
        assert!(observed.contains(field), "{field}: {observed}");
    }
    assert_eq!(
        keys(&unobserved),
        keys(&observed),
        "both arms, one field set"
    );
    let conpty = if cfg!(windows) { " conpty" } else { "" };
    assert_eq!(
        keys(&observed).join(" "),
        format!(
            "schema sid subject subject_source observed phase since_ms outcome exit_code signal \
             detail confidence reasons attribution fs_consent conflict revision enabled hold \
             hold_advisory fabric fabric_rtt_ms fabric_link_age_ms fleet hand hand_pid level story why \
             needs needs_n needs_told hold_by typed_by typed_ms path program agent agent_detail agent_rev \
             agent_since_ms agent_gen agent_fp program_state program_state_ms \
             last_act os_appearance theme_side reported_scheme scheme_from bg_dark \
             session_theme theme_origin{conpty} integration input input_bytes input_wait_ms fg_rss_mb \
             disk disk_free_mb disk_room_mb disk_age_ms job job_quiet_ms program_check syspolicyd \
             syspolicyd_workers syspolicyd_full_ms occupancy occupancy_hold ready slots \
             idle_ready_min waits waits_ready supervisor supervisor_proven autopilot \
             human_ms human_seq launch launch_seq path_evidence copy upgrade history_lost \
             journal journal_off integration_rev born render \
             resizes flaps land_at task pause land_held land_nudges land_since end end_seen \
             end_cheered task_state task_rev \
             log_seq log_ep run turn step need need_since_ms rollout rollout_asks editing bells \
             bells_sounded last_bell gen seq hash"
        ),
        "{observed}"
    );
    for record in [&unobserved, &observed] {
        assert!(!record.contains('\n'), "one line: {record}");
        assert_stamp_rides_last(record);
    }

    let ctx = app.pool.get(0).expect("live session").ctx.clone();
    crate::fabric::apply_hold_for_test(
        &ctx,
        Some(crate::fabric::Hold {
            reason: "main%20broken".to_string(),
            origin: "fleet".to_string(),
        }),
    );
    let pooled = &mut app.pool.sessions.get_mut(&0).expect("session 0").session;
    pooled.handoff_local_id = Some(11);
    app.config.tab_status = Some(false);
    let record = app.session_status_record(0).expect("live session");
    for field in [
        " hold=1 ",
        " attribution=adopted fs_consent=unknown ",
        " enabled=false ",
    ] {
        assert!(record.contains(field), "{field}: {record}");
    }
    assert!(
        app.session_status_record(9999).is_err(),
        "an unknown session is an error, not a blank record"
    );
}

#[test]
fn consent_at_risk_rides_the_reasons_token_only_on_the_conjunction() {
    assert_eq!(Reason::ConsentPrompt.as_str(), "consent_at_risk");

    let mut app = crate::App::headless_for_test();
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Complete { exit_code: Some(0) });
    settle_observer(&mut app.session_status, 0, &ev, t0);

    let record = app.session_status_record(0).expect("live session");
    assert!(
        !record.contains("consent_at_risk"),
        "no cwd reported, so no token: {record}"
    );

    #[cfg(unix)]
    {
        let policy = app.consent.policy();
        let Some(root) = policy.protected_roots().first() else {
            aterm_spec::host::could_not_run("no $HOME resolves here, so nothing is protected");
        };
        let cwd = root.join("aterm-consent-proof");
        {
            let term = app.pool.get(0).expect("session 0").term.clone();
            let mut t = crate::term_lock(&term);
            t.process(format!("\x1b]7;file://localhost{}\x07", cwd.display()).as_bytes());
        }
        let record = app.session_status_record(0).expect("live session");
        assert!(
            record.contains(",consent_at_risk "),
            "a protected cwd with an uncovered fs_consent arms the token: {record}"
        );
        assert!(
            record.contains(" fs_consent=unknown "),
            "and the token never upgrades the verdict it rides on: {record}"
        );
    }
}

#[test]
fn movement_at_a_prompt_publishes_nothing_and_owes_no_wake() {
    for shell in [ShellEvidence::Entering, ShellEvidence::Prompt] {
        let t0 = Instant::now();
        let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
        let mut ev = evidence(blank(1));
        ev.shell = Some(shell);
        let t1 = t0 + DWELL + Duration::from_millis(10);
        observer.observe(1, &ev, t0);
        observer.observe(1, &ev, t1);
        let status = observer.status(1).expect("published");
        assert_eq!(status.phase, Phase::Idle, "{shell:?}: a displayed prompt");
        assert_eq!(
            observer.next_wake(),
            None,
            "{shell:?}: a settled prompt parks"
        );

        ev.activity.content_seq = 2;
        let t2 = t1 + Duration::from_millis(10);
        assert!(
            !observer.observe(1, &ev, t2),
            "{shell:?}: movement at a prompt is not a published change"
        );
        let status = observer.status(1).expect("published");
        assert_eq!(status.phase, Phase::Idle, "{shell:?}");
        assert_eq!(
            status.reasons,
            vec![Reason::ShellBlock],
            "{shell:?}: no activity reason rides an idle prompt"
        );
        assert_eq!(
            observer.next_wake(),
            None,
            "{shell:?}: nothing ages out of an idle prompt, so nothing is owed"
        );
    }
}

#[test]
fn output_without_movement_arms_the_retire_wake_off_the_freshest_clock() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    observer.observe(1, &ev, t0);
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t0 + DWELL);
    let t1 = settle_observer(&mut observer, 1, &ev, t0 + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

    let t2 = t1 + QUIET * 2;
    ev.activity.last_output = Some(t2);
    observer.observe(1, &ev, t2);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));
    let owed = observer.next_wake().expect("Running owes its retirement");
    assert_eq!(
        owed,
        t2 + QUIET,
        "the wake is the earliest instant classify could change its verdict"
    );
    assert!(owed > t2, "an owed wake is never already in the past");

    observer.observe(1, &ev, owed);
    observer.observe(1, &ev, owed + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Quiet));
}

#[test]
fn an_armed_wake_never_precedes_its_own_observation_gate() {
    let t0 = Instant::now();
    let interval = Duration::from_millis(250);
    let mut observer = StatusObserver::new(policy(), interval);
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(false);
    observer.observe(1, &ev, t0);

    let t1 = t0 + DWELL - Duration::from_millis(50);
    observer.observe(1, &ev, t1);
    let gate = observer.sessions.get(&1).expect("slot").next_due;
    assert_eq!(gate, t1 + interval);
    assert!(
        t0 + DWELL < gate,
        "the raw owed wake precedes the gate — the shape under test"
    );
    let owed = observer
        .next_wake()
        .expect("a pending candidate owes a wake");
    assert_eq!(owed, gate, "the armed wake is clamped to the gate");
    assert!(
        observer.due(1, owed),
        "an armed wake at which observation is forbidden is a contradiction"
    );

    assert!(observer.observe(1, &ev, owed), "the transition lands");
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Idle));
}

#[test]
fn a_refused_sample_still_advances_the_observation_gate() {
    let t0 = Instant::now();
    let interval = Duration::from_millis(250);
    let mut observer = StatusObserver::new(policy(), interval);

    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    observer.observe(1, &ev, t0);
    let moved_at = t0 + Duration::from_millis(10);
    ev.activity.content_seq = 2;
    observer.observe(1, &ev, moved_at);
    observer.observe(1, &ev, moved_at + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

    let now = t0 + QUIET + Duration::from_secs(30);
    assert!(
        observer.next_wake().is_some_and(|wake| wake < now),
        "the shape under test: an owed wake already deep in the past"
    );

    observer.note_skipped(1, now);
    observer.note_swept(observer.swept_pool_epoch(), Vec::new());
    let wake = observer.next_wake().expect("the slot still owes a wake");
    assert!(
        wake >= now + interval,
        "a refused sample charges its interval, so the re-arm is in the future"
    );
    assert!(
        !observer.due(1, now),
        "and the gate that refused it is closed for that interval"
    );
    assert!(
        !observer.any_due(now),
        "the O(1) gate agrees, so the sweep itself stops running per turn"
    );
    assert!(
        observer.due(1, wake),
        "the armed wake is admissible when it fires"
    );

    observer.note_skipped(404, now);
    assert!(
        !observer.knows(404),
        "a refusal never invents a classification"
    );
}

#[test]
fn command_detail_keeps_the_program_and_an_allowlisted_subcommand_only() {
    let cases: &[(&str, Option<&str>)] = &[
        ("claude --dangerously-skip-permissions", Some("claude")),
        ("$ sleep 30", Some("sleep")),
        ("❯ codex", Some("codex")),
        ("% ", None),
        ("codex", Some("codex")),
        ("targo --unverified test -p x", Some("targo test")),
        ("cargo build --release", Some("cargo build")),
        ("git status", Some("git status")),
        (
            "env -S 'git status --porcelain /secret/repo'",
            Some("git status"),
        ),
        ("env -S 'git /secret/tok'", Some("git")),
        ("git -C /secret/repo status", Some("git")),
        ("git -C aterm status", Some("git")),
        ("kubectl -n prod get pods", Some("kubectl")),
        ("docker --context customer-acme ps", Some("docker")),
        ("npm --prefix web run build", Some("npm")),
        ("git customer-acme", Some("git")),
        ("cargo xtask release", Some("cargo")),
        ("ssh me@host", Some("ssh")),
        ("/usr/local/bin/python3 secret.py", Some("python3")),
        ("./run.sh --token abc", Some("run.sh")),
        (
            "\"C:\\Program Files\\Git\\usr\\bin\\less.exe\" README.md",
            Some("less"),
        ),
        (
            "& 'C:\\Program Files\\Git\\usr\\bin\\less.exe' C:\\Windows\\win.ini",
            Some("less"),
        ),
        (
            "& C:\\PROGRA~1\\Git\\usr\\bin\\less.exe C:\\Windows\\win.ini",
            Some("less"),
        ),
        ("'/opt/my tools/claude' --resume", Some("claude")),
        ("C:\\Windows\\System32\\cmd.exe /c dir", Some("cmd")),
        ("C:\\tools\\build.CMD release", Some("build")),
        ("my\\ tool --flag", Some("my")),
        ("\"git\" status", Some("git status")),
        ("python3.11 -m http.server", Some("python3.11")),
        ("/usr/bin/a.out", Some("a.out")),
        ("FOO=1 cmd --secret", Some("cmd")),
        ("FOO=1 BAR=two targo test", Some("targo test")),
        ("sudo -u root targo --unverified test", Some("targo test")),
        ("sudo apt install x", Some("apt")),
        ("env FOO=1 codex", Some("codex")),
        ("/usr/bin/env FOO=1 codex", Some("codex")),
        (
            "/usr/bin/sudo -u root targo --unverified test",
            Some("targo test"),
        ),
        ("time make deploy-prod", Some("make")),
        ("make test", Some("make test")),
        ("just fmt", Some("just fmt")),
        ("aterm ctl ls", Some("aterm ctl")),
        ("atpkg install foo", Some("atpkg install")),
        ("nice -n 5 kubectl get pods -n prod", Some("kubectl get")),
        ("docker compose up", Some("docker compose")),
        ("'claude' 'resume'", Some("claude")),
        ("clau\u{7f}de --private", Some("claude")),
        ("s\u{7f}udo -u user codex", Some("codex")),
        ("targo te\u{7f}st private", Some("targo test")),
        ("npm run build:prod", Some("npm run")),
        ("pnpm dlx create-foo", Some("pnpm dlx")),
        ("yarn build --token abc", Some("yarn build")),
        ("make ./target/x", Some("make")),
        ("cd ~/ay && claude", Some("claude")),
        ("cd ~/ay && exec claude", Some("claude")),
        ("exec claude --resume", Some("claude")),
        ("cd /tmp; codex", Some("codex")),
        ("make || echo failed", Some("make")),
        ("FOO=1 cd x && targo test", Some("targo test")),
        (
            "git pull && targo --unverified test -p aterm-gui",
            Some("targo test"),
        ),
        ("cd x && claude || echo failed", Some("claude")),
        ("cd x; claude;", Some("claude")),
        ("cd x &&", Some("cd")),
        ("(cd ~/ay && claude)", Some("claude")),
        ("cd x\nclaude", Some("claude")),
        ("cd x && cat log | grep err", Some("cat")),
        ("claude -p \"fix a; then b\"", Some("claude")),
        ("echo 'x && y' && codex", Some("codex")),
        ("echo a \\; codex", Some("echo")),
        ("for i in 1 2 3; do sleep 1; done", Some("for")),
        ("$ for i in 1 2 3; do sleep 1; done", Some("for")),
        ("while true; do claude; done", Some("while")),
        ("if [ -f x ]; then claude; fi", Some("if")),
        ("{ cd x; claude; }", Some("cd")),
        ("cd x && for i in 1 2 3; do sleep 1; done", Some("for")),
        ("cd x && if true; then claude; fi", Some("if")),
        ("time for i in 1 2 3; do sleep 1; done", Some("for")),
        ("", None),
        ("   \t ", None),
        (";", None),
    ];
    const KEYWORD_CLOSERS: [&str; 8] = ["done", "fi", "esac", "}", "then", "do", "else", "elif"];
    for (cmd, want) in cases {
        let got = command_detail(cmd);
        assert_eq!(got.as_deref(), *want, "{cmd:?}");
        assert!(
            !got.as_deref().is_some_and(|d| KEYWORD_CLOSERS.contains(&d)),
            "{cmd:?} reads a closer, which is never a program: {got:?}"
        );
    }
    let long = format!("{} arg", "p".repeat(200));
    let detail = command_detail(&long).unwrap();
    assert_eq!(detail.chars().count(), 48);
    assert!(
        detail.capacity() <= 48 * 4,
        "long input capacity must not be retained"
    );
    let wide = format!("{} private", "終".repeat(80));
    assert_eq!(command_detail(&wide), Some("終".repeat(48)));
    assert_eq!(command_detail("claude commit").as_deref(), Some("claude"));
}

#[test]
fn a_quoted_program_path_reads_its_name_and_never_an_argument() {
    let store = "Library/Application Support/aterm/pkg/agents/claude";
    let abs = format!("/Users//me/{store}");
    let cases: &[(String, &str)] = &[
        (
            format!(
                ". '/Users//me/.aterm/shell.d/00-atpkg.zsh'; rehash; '{abs}' \
                 '--dangerously-skip-permissions' '--resume' 'b87ea568-277e-4ba7-bbd2-f0ab8bd14d1c'"
            ),
            "claude",
        ),
        (format!("\"{abs}\" --resume SECRET"), "claude"),
        (
            format!("{} --resume SECRET", abs.replace(' ', r"\ ")),
            "claude",
        ),
        (format!("exec '{abs}'"), "claude"),
        (format!("'{abs}' '--resume' 'SECRET'"), "claude"),
        (
            format!(r"'/Users//o'\''neil/{store}' '--resume' 'SECRET'"),
            "claude",
        ),
        ("env 'FOO=a b' '/opt/my tools/codex' resume".into(), "codex"),
        (
            "/Users//me/.local/share/claude/versions/2.1.281 --resume x".into(),
            "2.1.281",
        ),
        (
            "claude -p 'fix the bug in Application Support'".into(),
            "claude",
        ),
        (
            r"& C:\Program` Files\Git\usr\bin\less.exe C:\Windows\win.ini".into(),
            "less",
        ),
        (
            r"C:\Program` Files\Git\usr\bin\less C:\Windows\win.ini".into(),
            "less",
        ),
        (r"& .\my` tools\build.cmd --target x".into(), "build"),
        (
            r"& C:/Program` Files/Git/usr/bin/vim.exe notes.txt".into(),
            "vim",
        ),
        (r"& C:\tools\my` app.exe --token SECRET".into(), "my"),
        ("claude` /secret/tok --resume x".into(), "claude"),
        ("claude `echo /secret/tok`".into(), "claude"),
        ("`which less` README.md".into(), "which"),
        (
            r"& 'C:\Program Files\Git\usr\bin\less.exe' C:\Windows\win.ini".into(),
            "less",
        ),
        (r"$'/usr/local/bin/don\'t' --token SECRET".into(), "t"),
        (
            r"'/Users//o\'neil/bin/claude' '--resume' 'b87ea568-277e-4ba7-bbd2-f0ab8bd14d1c'".into(),
            "claude",
        ),
        (
            r"'/Users//o\'neil/bin/claude' '--resume' '/secret/tok en'".into(),
            "claude",
        ),
        (
            format!(r"'/Users//o\'neil/{store}' '--resume' 'SECRET'"),
            "Application",
        ),
        ("don't --token SECRET".into(), "don't"),
        ("don't /secret/tok".into(), "don't"),
        ("don't '/secret/tok'".into(), "don't"),
        ("don't --token '/secret/tok'".into(), "don't"),
        ("don't \"/secret/tok\"".into(), "don't"),
        (
            "don't '--resume' '/Users//me/secret-session'".into(),
            "don't",
        ),
        (
            "it's-a-script.sh --password hunter2".into(),
            "it's-a-script.sh",
        ),
        (
            "it's-a-script.sh --password 'x/hunter2'".into(),
            "it's-a-script.sh",
        ),
        ("sudo don't '/secret/tok'".into(), "don't"),
        ("o'neil-tool --token '/secret/tok'".into(), "o'neil-tool"),
        (
            "/Users//o'neil/bin/claude --resume '/secret/tok'".into(),
            "claude",
        ),
        ("can't 'x' '/secret/tok'".into(), "can't"),
        ("it's 'a' b '/secret/tok'".into(), "it's"),
        (
            "env -S \"node --inspect /Users//me/secret-project/server.js\"".into(),
            "node",
        ),
        ("env -S 'claude --resume /secret/tok'".into(), "claude"),
        (
            "/usr/bin/env -S \"python3 /Users//me/private/notes.py --token x\"".into(),
            "python3",
        ),
        (
            "env --split-string 'deno run --allow-read /Users//me/secret/app.ts'".into(),
            "deno",
        ),
        (
            "env --split-string='deno run --allow-read /Users//me/secret/app.ts'".into(),
            "deno",
        ),
        ("env -vS 'node /srv/secret/server.js'".into(), "node"),
        ("env -S'node /srv/secret/server.js'".into(), "node"),
        ("env -S '-i FOO=1 /usr/bin/node /srv/x.js'".into(), "node"),
        ("env -uS /usr/bin/node /srv/x.js".into(), "node"),
        (r"/Users//me/my\ tool --token SECRET".into(), "my"),
        ("\"/opt/my tools/run me\" --token SECRET".into(), "run"),
    ];
    const ARGUMENTS: [&str; 7] = [
        "SECRET", "token", "hunter2", "b87ea568", "resume", "secret", "tok",
    ];
    for (cmd, want) in cases {
        let got = command_detail(cmd);
        assert_eq!(got.as_deref(), Some(*want), "{cmd:?}");
        let got = got.unwrap_or_default();
        assert!(!got.contains(char::is_whitespace), "{cmd:?}: {got:?}");
        for argument in ARGUMENTS {
            assert!(
                !got.contains(argument),
                "{cmd:?}: {got:?} carries {argument:?}"
            );
        }
    }
}

#[test]
fn executing_detail_reads_only_an_executing_block() {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    assert_eq!(executing_detail(&term), None, "no shell integration");
    term.process(b"\x1b]133;A\x07$ ");
    assert_eq!(executing_detail(&term), None, "prompt only");
    term.process(b"\x1b]133;B\x07targo --unverified test -p aterm-gui");
    assert_eq!(executing_detail(&term), None, "entering, not executing");
    term.process(b"\n\x1b]133;C\x07");
    assert_eq!(
        executing_detail(&term).as_deref(),
        Some("targo test"),
        "executing: screen-scraped command, sanitized"
    );
    term.process(b"running\n\x1b]133;D;0\x07");
    assert_eq!(executing_detail(&term), None, "complete");
    term.process(b"\x1b]133;A\x07$ \x1b]633;E;claude --dangerously-skip-permissions\x07\x1b]133;B\x07claude --dangerously-skip-permissions\n\x1b]133;C\x07");
    assert_eq!(executing_detail(&term).as_deref(), Some("claude"));
    term.process(b"\x1b]133;D;0\x07");
    term.process(
        "]133;A🐱 $ ]133;Bcodex resume
]133;C"
            .as_bytes(),
    );
    assert_eq!(executing_detail(&term).as_deref(), Some("codex"));
}

#[test]
fn a_full_clear_while_the_command_runs_never_names_what_the_program_drew() {
    for named in [false, true] {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        let explicit = if named {
            "\x1b]633;E;vim notes\x07"
        } else {
            ""
        };
        term.process(
            format!("\x1b[10;1H\x1b]133;A\x07$ {explicit}\x1b]133;B\x07vim notes\n\x1b]133;C\x07")
                .as_bytes(),
        );
        assert_eq!(
            executing_detail(&term).as_deref(),
            Some("vim"),
            "named={named}: CONTROL"
        );
        let row = term
            .current_block()
            .and_then(|b| b.command_start_row)
            .expect("the command row");
        term.process(b"\x1b[H\x1b[2J\x1b[10;1H$ python3 server.py");
        assert_eq!(
            executing_detail(&term).as_deref(),
            named.then_some("vim"),
            "named={named}"
        );
        assert_eq!(
            term.abs_row_text(row).as_deref().map(str::trim_end),
            Some("$ python3 server.py"),
            "named={named}: NEGATIVE CONTROL: the row holds the program's text"
        );
    }
}

#[test]
fn the_status_record_details_the_executing_block_and_nothing_else() {
    let mut app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();

    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" detail=- "), "nothing running: {record}");

    term.lock()
        .unwrap()
        .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep xyzzy\n\x1b]133;C\x07");
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" observed=false ") && record.contains(" detail=sleep "),
        "the engine's fact precedes the classifier's first publish: {record}"
    );

    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.shell = Some(ShellEvidence::Executing);
    settle_observer(&mut app.session_status, 0, &ev, t0);
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" observed=true ") && record.contains(" detail=sleep "),
        "{record}"
    );
    assert!(
        !record.contains("xyzzy"),
        "arguments never reach the wire: {record}"
    );

    let held = term.lock().unwrap();
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" subject=- subject_source=unavailable ") && record.contains(" detail=- "),
        "a contended terminal is `-`, not a stall or a lower rung: {record}"
    );

    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set("title", Some("deploy".to_string()))
        .expect("a short title is within cap");
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" subject=deploy subject_source=pin ") && record.contains(" detail=- "),
        "the pin must not need the terminal lock: {record}"
    );
    drop(held);
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" subject=deploy subject_source=pin ") && record.contains(" detail=sleep "),
        "{record}"
    );

    term.lock().unwrap().process(b"\x1b]133;D;0\x07");
    let record = app.session_status_record(0).expect("live session");
    assert!(record.contains(" detail=- "), "complete: {record}");
}

#[test]
fn the_sweep_publishes_the_running_detail_onto_the_chrome_status() {
    let mut app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();
    let t0 = Instant::now();

    app.observe_session_statuses(t0);
    assert_eq!(
        app.session_status.status(0).and_then(|s| s.detail.clone()),
        None,
        "idle publishes no detail"
    );

    term.lock()
        .unwrap()
        .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep 30\n\x1b]133;C\x07");
    let t1 = t0 + Duration::from_secs(10);
    app.observe_session_statuses(t1);
    assert_eq!(
        app.session_status
            .status(0)
            .and_then(|s| s.detail.clone())
            .as_deref(),
        Some("sleep"),
        "the executing block's sanitized program reaches the chrome status"
    );

    term.lock().unwrap().process(b"\x1b]133;D;0\x07");
    let t2 = t1 + Duration::from_secs(10);
    app.observe_session_statuses(t2);
    assert_eq!(
        app.session_status.status(0).and_then(|s| s.detail.clone()),
        None,
        "a completed block clears the chrome detail"
    );
}

#[test]
fn the_status_record_carries_the_task_record() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let app = crate::App::headless_for_test();
    let none = app.session_status_record(0).expect("live session");
    assert!(
        none.contains(
            " pause=- land_held=- land_nudges=- land_since=- end=- end_seen=- end_cheered=- \
             task_state=- task_rev=0 log_seq="
        ),
        "{none}"
    );
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    mark_session_task(&ctx, 0, &TaskMark::new(TaskState::Working)).expect("unfenced");
    mark_session_task(&ctx, 0, &TaskMark::new(TaskState::Done)).expect("unfenced");
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(
            " task=- pause=- land_held=- land_nudges=- land_since=- end=- end_seen=- end_cheered=- \
             task_state=done task_rev=2 log_seq="
        ),
        "{record}"
    );
    assert_stamp_rides_last(&record);
}

#[test]
fn the_conpty_field_names_the_host_on_windows_and_is_absent_elsewhere() {
    use aterm_pty::ConsoleHostKind;
    assert_eq!(
        conpty_status_field(true, Some(ConsoleHostKind::OpenConsole)),
        " conpty=openconsole"
    );
    assert_eq!(
        conpty_status_field(true, Some(ConsoleHostKind::Inbox)),
        " conpty=inbox"
    );
    assert_eq!(conpty_status_field(true, None), " conpty=-");
    for host in [
        None,
        Some(ConsoleHostKind::Inbox),
        Some(ConsoleHostKind::OpenConsole),
    ] {
        assert_eq!(conpty_status_field(false, host), "");
    }
    let app = crate::App::headless_for_test();
    let record = app.session_status_record(0).expect("live session");
    let want = if cfg!(windows) {
        " conpty=- integration="
    } else {
        " integration="
    };
    assert!(record.contains(want), "{record}");
    assert_eq!(record.contains(" conpty="), cfg!(windows), "{record}");
}

#[test]
fn a_look_reads_the_foreground_group_once() {
    use std::cell::Cell;
    thread_local! {
        static READS: Cell<u64> = const { Cell::new(0) };
    }
    const SHELL: i32 = 2_000_000_201;
    const JOB: i32 = 2_000_000_202;
    fn counted(_master: i32) -> i32 {
        READS.with(|r| r.set(r.get() + 1));
        JOB
    }
    const STAGED: u64 = 8;
    let mut app = crate::App::headless_for_test();
    app.session_factory.fg_probe = counted;
    for i in 0..STAGED {
        let mut session = crate::stub_session(1000 + i);
        session.master = i32::MAX - i32::try_from(i).expect("small fixture");
        session.pid = SHELL;
        app.pool.insert(session);
    }
    let t0 = Instant::now();
    let sweeps = 4u32;
    for turn in 0..sweeps {
        let _ = app.observe_session_statuses(t0 + app.session_status.min_interval * turn);
    }
    assert_eq!(
        READS.with(Cell::get),
        STAGED * u64::from(sweeps),
        "one foreground read per looked-at session per sweep"
    );
    for i in 0..STAGED {
        assert_eq!(
            app.session_status
                .status(1000 + i)
                .map(|s| s.phase.as_str()),
            Some("quiet"),
            "the job verdict is the read's: a group in front that is not the \
             shell's, with no output, is a quiet job"
        );
    }
}

#[test]
fn status_counts_a_harness_needs() {
    use aterm_types::needs::AttentionKind;
    let asked = |owner: &str, text: &str| {
        crate::needs::of(&crate::needs::NeedFacts {
            attention: vec![crate::needs::AttentionFact {
                owner: owner.to_string(),
                text: text.to_string(),
                kind: Some(AttentionKind::Ask),
            }],
            ..crate::needs::NeedFacts::default()
        })
    };
    let needs = crate::harness_host::NEEDS_ATTENTION_OWNER;
    for (verdict, want) in [
        (
            asked(
                needs,
                "claude needs you: sign it (+1 more) \u{2014} carrying on",
            ),
            "ask needs_n=2",
        ),
        (
            asked(needs, "gemini needs you: sign it \u{2014} carrying on"),
            "ask needs_n=1",
        ),
        (
            asked(
                "lab",
                "claude needs you: sign it (+1 more) \u{2014} carrying on",
            ),
            "ask needs_n=-",
        ),
        (crate::needs::Verdict::default(), "- needs_n=-"),
    ] {
        assert_eq!(super::needs_fields(&verdict), want);
    }
}

/// A REPORT NEVER SHOWS UNDER THE PROGRAM THAT REPLACED ITS WRITER: the real sweep
/// (`App::observe_session_statuses`) judges the claim by the program in front once it has
/// noted the foreground move, not by the name it read before. A codex in front shows its
/// `working`; it reports `blocked:permission` and a claude takes the front before the next
/// sweep: that sweep's `status` and `ls` show `program_state=-`, as does the one after the
/// resolver names claude. A codex that takes the front the same way has that report, which
/// no sweep saw before the move (its writer is not attributable), shown once it is named.
#[test]
fn a_report_that_rode_a_foreground_move_never_shows_under_the_program_now_in_front() {
    use super::program::{LeaderFacts, ProgramResolver};
    const WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
    const BLOCKED_PERMISSION: &[u8] = b"\x1b]7501;state=blocked:app=codex:kind=permission\x1b\\";
    const CODEX: i32 = i32::MAX - 11;
    const CLAUDE: i32 = i32::MAX - 12;
    const NEXT_CODEX: i32 = i32::MAX - 13;
    let field = |row: &str, key: &str| {
        row.split(' ')
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .map(str::to_string)
    };
    for (successor, successor_name, want) in [
        (CLAUDE, "claude", "-"),
        (NEXT_CODEX, "codex", "blocked:permission"),
    ] {
        let mut app = crate::App::headless_for_test();
        app.session_factory.fg_probe = |master| master;
        app.session_status.programs = ProgramResolver::with_lookup(|pgid, _shell| LeaderFacts {
            program: match pgid {
                CODEX | NEXT_CODEX => Some("codex".into()),
                CLAUDE => Some("claude".into()),
                _ => None,
            },
            ..LeaderFacts::default()
        });
        let sid = app.next_session_id;
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
        let front = |app: &mut crate::App, group: i32| {
            app.pool
                .sessions
                .get_mut(&sid)
                .expect("pooled")
                .session
                .master = group;
        };
        let (term, timeline) = {
            let pooled = app.pool.get(sid).expect("pooled");
            (
                std::sync::Arc::clone(&pooled.term),
                std::sync::Arc::clone(&pooled.ctx.timeline),
            )
        };
        let named = |want: &str| {
            let program = || timeline.lock().unwrap().agent().program.clone();
            let named = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::from_millis(1),
                || (program().as_deref() == Some(want)).then_some(()),
            );
            assert!(named.is_some(), "never named {want}: {:?}", program());
        };
        let shown = |app: &crate::App| {
            let status = app.session_status_record(sid).expect("a record");
            let row = crate::control::cmd_sessions_store(&app.store, None)
                .lines()
                .find(|line| line.split(' ').next() == Some(sid.to_string().as_str()))
                .expect("the session's row")
                .to_string();
            (
                field(&status, "program_state").unwrap_or_default(),
                field(&row, "program_state").unwrap_or_default(),
            )
        };
        let step = std::time::Duration::from_secs(1);
        let mut t = std::time::Instant::now();
        front(&mut app, CODEX);
        let _ = app.observe_session_statuses(t);
        named("codex");
        crate::term_lock(&term).process(WORKING);
        t += step;
        let _ = app.observe_session_statuses(t);
        assert_eq!(
            shown(&app),
            ("working".to_string(), "working".to_string()),
            "PRECONDITION: the codex in front shows its report"
        );
        crate::term_lock(&term).process(BLOCKED_PERMISSION);
        front(&mut app, successor);
        t += step;
        let _ = app.observe_session_statuses(t);
        assert_eq!(
            shown(&app),
            ("-".to_string(), "-".to_string()),
            "the sweep that notes the move judges the report by the unnamed program in front"
        );
        named(successor_name);
        t += step;
        let _ = app.observe_session_statuses(t);
        assert_eq!(
            shown(&app),
            (want.to_string(), want.to_string()),
            "once {successor_name} is named in front"
        );
    }
}

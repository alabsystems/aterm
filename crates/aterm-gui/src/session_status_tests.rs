// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::{Arc, Mutex};

use crate::session_timeline::SessionTimeline;

/// The record ends `seq=<n|-> hash=<hex16|->`, in that order: the pair a stamped report opens
/// with and `history` can be held against.
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
    // The UI sampled Claude before the worker published zsh and sent its
    // stop. A later request from that stale sample would revive the watch.
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

/// THE RE-ASK AT THE DRAWER'S EXEC EDGE (card engine §3.10). atpkg's shim
/// is named `claude` and `exec`s the store build in place: the group and the
/// name stay, so the sweep would not ask again for `RECHECK`, and its first
/// ask may have fallen in the shim, whose environment macOS hides (no Claude
/// dir, no registry watch). An edged visit whose group's published image is
/// a store or Native Claude Code drawer the last ask was not made under asks
/// at once, once per image (an ALab build the store's record names, too).
/// CONTROLS: the shim's own image, an unclassified `claude` and a visit off
/// the edge ask nothing.
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

/// The block that completed with `exit_code`, as [`completed_block`] reads it.
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

/// Drive a candidate past the dwell gate: observe, wait, observe again.
fn settle(fsm: &mut StatusFsm, ev: &Evidence, at: Instant) -> Instant {
    fsm.observe(ev, at);
    let later = at + DWELL;
    fsm.observe(ev, later);
    later
}

/// cmd.exe's `%PROMPT%` integration marks prompts and never a command, so `ping
/// -n 30 127.0.0.1` runs under the `Entering` block of the prompt it was typed
/// at. The snapshot of 2026-09-29 records the defect on 0.95.0 (2026-09-27):
/// `status` read `phase=idle` while the close guard refused the close as a
/// running job. With no command ever reported, the live job outranks that
/// prompt. CONTROL: the same evidence without the yield reads idle.
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

/// The other side of the line: a shell whose marks HAVE reported a command keeps
/// its prompt's authority over a child it started in the background
/// (`Start-Process notepad`), and a prompt-only shell with no live child stays
/// idle on its prompt block. Only the prompt arms yield: an executing or
/// completed block already says what it is.
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

    // Through the FSM: the reporting shell's prompt reads Idle on the shell
    // block, never the foreground job.
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

/// What "reported a command" is read from: cmd.exe's prompt marks (A, B, then
/// the typed line and its output, then the next prompt) never move the
/// completed-command count; pwsh's C…D does.
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

/// Every phase has a wire spelling and the classifier produces each one: the `status` verb's
/// vocabulary is exhaustive.
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

    // No evidence reports unknown rather than guessing.
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

    // Silence is Quiet, and only heuristically: a pager, a REPL, a password prompt and a
    // `sleep` look alike from here.
    ev.activity.last_output = None;
    settle(&mut fsm, &ev, t2 + QUIET * 2);
    assert_eq!(fsm.status().phase, Phase::Quiet);
    assert_eq!(fsm.status().confidence, Confidence::Heuristic);
    // `exited`: `a_collected_exit_code_becomes_the_outcome`.
}

#[test]
fn alt_screen_transition_is_a_resync_not_activity() {
    // Entering the alternate screen restarts content_gen. A naive
    // comparison would read that jump as a burst of work.
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

/// On a shell-integrated session, output that moves nothing reads `output_activity` (the
/// `movement_reason` the foreground-job arm uses), so `ctl status` shows whether the movement
/// clock is stale (`docs/EFFECTS-AND-WAKE-FOLLOWUPS-2026-08-24.md` item 21). The work starts with
/// the grid moving (output alone starts no `Running`:
/// [`output_that_moves_no_cell_sustains_running_but_never_starts_it`]), and bytes that mark no
/// cell keep it alive once the movement clock is stale.
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

    // The grid stands still past the quiet window while bytes keep coming.
    let t2 = t1 + QUIET;
    ev.activity.last_output = Some(t2);
    fsm.observe(&ev, t2);
    assert_eq!(fsm.status().phase, Phase::Running);
    assert_eq!(
        fsm.status().reasons,
        vec![Reason::ShellBlock, Reason::OutputActivity],
        "bytes arrived and the grid never moved"
    );

    // Control: the grid moves, and the record says so.
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

/// OUTPUT THAT MOVES NO CELL NEVER STARTS `Running`
/// (docs/measured/vitals-multi-tab-2026-10-22.md): a program at rest whose
/// writes mark no cell (Muse Code's SGR reset every ~9 s, Gemini CLI's OSC 11
/// query once a minute) stays where it rests (`Quiet` with a foreground job,
/// `Unknown` with no foreground evidence at all) and owes the sweep no wake.
/// Before, each such write read `quiet -> running -> quiet` and cost three
/// timed sweeps: the dwell's publish, the retirement, and its dwell.
/// NEGATIVE CONTROLS: a write that moves the grid still starts `Running`
/// after the dwell and owes its wakes, and a published `Running` is still
/// sustained by output alone
/// ([`executing_names_output_only_movement_as_output_activity`]).
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

        // Writes that mark no cell, one a second for a minute.
        for n in 1..=60 {
            let at = t1 + Duration::from_secs(n);
            ev.activity.last_output = Some(at);
            assert!(!fsm.observe(&ev, at), "{case}: nothing to publish");
            assert_eq!(fsm.status().phase, rest, "{case}");
            assert_eq!(fsm.owed_wake(), None, "{case}: no sweep is owed");
        }

        // Control: a write that moves the grid starts the work.
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

/// A command Ctrl+C stopped reads as interrupted, not as a failure code:
/// STATUS_CONTROL_C_EXIT, which a Windows console program (`ping -t` in pwsh,
/// measured 2026-10-17 as `failed (exit -1073741510)`) and a pwsh line Ctrl+C
/// stopped report, everywhere; Git Bash's 130 on Windows, so its tabs read as
/// pwsh's. The outcome stays a failure with its code (the wire and the tab's
/// failure mark are unchanged). NEGATIVE CONTROL: any other code is its code.
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
        // An exited session with no collectable status (an unwitnessed adopted shell, an
        // unreadable master) is unknown, not a failure worth marking.
        (Phase::Exited, Outcome::None, (false, false)),
        // A finished failure keeps the tab marked while the shell honestly
        // sits back at a prompt.
        (
            Phase::Idle,
            Outcome::Failure { exit_code: 1 },
            (false, true),
        ),
        (Phase::Exited, Outcome::Signal { signal: 9 }, (false, true)),
        // Running and failed: the previous unit's result is still worth showing,
        // and neither bit hides the other.
        (
            Phase::Running,
            Outcome::Failure { exit_code: 2 },
            (true, true),
        ),
        (Phase::Idle, Outcome::Success, (false, false)),
        // Quiet is work in flight that has printed nothing recently (reachable only with a
        // known foreground job), so the dot stays lit.
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
                // A terminal never writes the out-of-band bit, so the classifier's own
                // bit can be recomputed rather than latched.
                attention: false,
                status_attention: attention,
            },
            "{phase:?} with {outcome:?}"
        );
    }
}

/// End to end on the real fan-out: a background pane's transition reaches the tab its window
/// renders, which a focus-addressed status path would miss.
#[test]
fn a_background_pane_going_busy_marks_its_tab() {
    use crate::tab_model::TabIndicators;

    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    // The new pane takes focus, so session 0 becomes the background leaf.
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

/// The classifier's bit (`status_attention`) is refolded from the leaves, never ORed back from
/// the stored one: a cleared failure releases the tab while a native sibling's out-of-band
/// `attention` survives.
#[test]
fn a_cleared_failure_releases_the_tab_while_a_native_sibling_keeps_its_own_mark() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    // A mixed tab: the terminal leaf (session 0) plus a native sibling.
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

    // The native sibling raises its own attention out of band (a failed document shutdown, an
    // update announcement), which the status path must never erase. It is set while the
    // classifier's bit is also set, which one shared bool could not tell apart.
    app.windows
        .get_mut(&wid)
        .and_then(|window| window.tab_set.active_mut())
        .expect("active tab")
        .presentation
        .indicators
        .attention = true;

    // A new unit of work clears the terminal's outcome.
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

    // The native owner clears its own bit: nothing is left claiming attention.
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

/// What the chrome shows: the fold both renderers and the introspection serializer read, not
/// either owner's field alone.
fn attention_of(app: &crate::App, wid: crate::WindowId, tab_id: crate::tab_model::TabId) -> bool {
    app.windows[&wid]
        .tab_set
        .get(tab_id)
        .expect("tab")
        .presentation
        .indicators
        .wants_attention()
}

/// [`settle`] for the observer, which owns the dwell clock through its slots.
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

/// The sweep records each published transition as one `status-change` event on the session's
/// timeline (RFC §7), which the `events` digest pushes as `EVENT <local> status …`.
///
/// The bytes are a real integration's: the precmd's `133;D` is followed at once by the next
/// prompt's `133;A` and `133;B`, which archive the finished block before the sweep reads it, so
/// the outcome must come from the block the prompt closed.
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
    // A command ran and failed: OSC 133 A, B, C, then the precmd's D with
    // exit 7 and the next prompt's A and B, in one batch.
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
    // Nothing moved: another sweep records nothing.
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

    // The failure marks the tab until it is seen. A second failing command inside one sweep
    // interval is no status change; only its block, closed by the trailing prompt, re-arms it.
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

    // A command that succeeds reports that, through the same prompt.
    run("0");
    let _ = app.observe_session_statuses(t0 + Duration::from_secs(5));
    assert_eq!(
        status_events(&timeline).last().map(String::as_str),
        Some("phase=idle outcome=success exit_code=- signal=- confidence=strong"),
    );
    assert!(!attention(&app), "a success carries no mark");
}

/// A prompt reports only the block it closed ([`completed_block`]): the finished command behind
/// a prompt, the current block while it is `Complete`, nothing while a command runs, and nothing
/// when the prompt closed a block that never completed (an empty Enter, a replaced shell), so an
/// older command's result never comes back.
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

    // An empty Enter: A with no C or D closes a block that ran nothing.
    term.process(prompt);
    assert_eq!(
        completed_block(&term),
        None,
        "a prompt that closed an empty block reports no older result"
    );

    // A command whose shell never reported its end (`exec zsh`): the next
    // prompt closes an Executing block.
    term.process(b"\x1b]133;C\x1b\\");
    term.process(prompt);
    assert_eq!(completed_block(&term), None);
}

/// A SCROLLBACK ERASE NEVER LEAVES THE STATUS ON AN EARLIER EXIT: the
/// reviewer's probes through the real sweep, typed into a zsh-style prompt:
/// `sh -c 'exit 6'`, a command that writes ED 3 and exits 5, and `clear; sh
/// -c 'exit 4'` (macOS `clear` writes ED 3). Each publishes its own exit: the
/// erase keeps the running block, so its D lands there. Then a command whose
/// rows all went into the history before its prompt (it printed past the
/// screen and homed with ED 2), and a Cmd-K at that prompt before the next
/// sweep: the erase takes its block, and its exit still publishes (the last
/// D's copy, [`completed_block`]). Before the fix the first erase took the
/// running block and `exit_code=6` stood over 5, 4 and 3; NEGATIVE CONTROL:
/// a RIS still takes the block and swallows its D (by design), and the
/// published outcome stands, so the reading tells a lost block apart; the
/// last D's copy answers only for the block the prompt closed, so a finished
/// command before the RIS that no sweep read is not published over it.
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

    // NEGATIVE CONTROL: a RIS takes the running block and the phase, so the
    // shell's D is refused and no exit is read: the outcome stands. A command
    // that exited 2 just before it, with no sweep between, is the last D's
    // copy, but the prompt after the RIS closed the `reset` block, not that
    // one, so the copy is not read either (the same prompt with no erase or
    // RIS reports nothing: a prompt reports only the block it closed).
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

/// A session without shell integration has one failure edge, its lifecycle exit, and no block
/// to tell failures apart: the mark arms once on the published edge and a look quiets it.
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

/// The flapping policy (RFC §15): a `status-change` event is taken only when the published phase
/// or outcome moved, and dwell bounds how often that happens, so output alternating with silence
/// faster than dwell yields at most one event per dwell interval.
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
    // Not vacuous: a transition that survives dwell is an event.
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

/// A PTY exit publishes `Exited` immediately and stays there. Under `--hold` the pane outlives
/// its shell and keeps being swept; `tcgetpgrp` then fails, which reads as "no foreground job"
/// and would publish `Idle` (chrome's "ready") for a dead pane.
#[test]
fn an_exited_pane_stays_exited_instead_of_reverting_to_ready() {
    let mut app = crate::App::headless_for_test();
    app.hold = true;
    let t0 = Instant::now();
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    app.session_status.observe(0, &ev, t0);
    // The job draws: a moved grid starts `Running` (output alone never does).
    ev.activity.content_seq += 1;
    settle_observer(&mut app.session_status, 0, &ev, t0);
    assert_eq!(
        app.session_status.status(0).map(|s| s.phase),
        Some(Phase::Running)
    );

    // The stub session's pid is not a real child, so the collector honestly
    // answers "unknown" rather than inventing a code.
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

    // The `--hold` sweep: the child is gone, so `tcgetpgrp` reports no foreground job.
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

/// A collected code becomes the session's outcome, and a failure marks the tab.
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

/// A live policy edit reaches sessions that already exist: each FSM holds its own copy of the
/// policy, so updating only the observer would leave open tabs on the old numbers.
#[test]
fn a_policy_edit_reaches_live_sessions_and_reopens_the_deadline() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(250));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    settle_observer(&mut observer, 1, &ev, t0);

    // A candidate serving its dwell owes a wake at first_seen + dwell.
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

/// `tab_status = false` is a real off: no classification at all, and the records already on
/// screen are retired rather than frozen.
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

/// `tab_status_badge = false` keeps the record and only quiets the chrome: the two switches are
/// independent.
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

/// `land_at=` AND `task=` (the Autopilot design, §9.3.5 and §9.4.1), on
/// `status`, the `sessions` roster and the bare `meta`: `-` with nothing
/// stored, just before the screen's stamp on `status` and last on the
/// roster's row. The task's face prints only for an agent session whose
/// live claim THIS window's host holds — told through the host's own wake
/// (`App::apply_harness_fact`) — or a proven self-claim holds (Autopilot Land
/// §10.4: its own `landed@v1` act, told as `HarnessFact::SelfTask`), and the
/// instant only for an agent session, where `meta` prints what is stored.
/// NEGATIVE CONTROLS: a face told with no claim, or under another holder's,
/// prints `-`, and so does the self-claim's face once its claim is gone; a
/// stored instant on a session that runs no agent prints `-` on `status` and
/// the roster.
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

    // A face told with no claim of this host's: kept, never printed.
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
    // Another holder's claim: this host hears nothing from its loop.
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
    // A proven self-claim: its own landed@v1 act's face prints, on both
    // rows, and the hosted loop's facts beside it are no face of its.
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
    // NEGATIVE CONTROL: its claim gone, its face prints no more.
    assert!(crate::session_timeline::release_supervisor(
        &ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(field(&status(&app), "task="), "-", "no live claim");
    assert_eq!(field(&roster(&app), "task="), "-", "no live claim");
    // A fact for a sid this window does not hold is dropped.
    app.apply_harness_fact(
        "no-such-sid",
        crate::HarnessFact::Task(Some(TaskFace::Open)),
    );
    let record = status(&app);
    assert_stamp_rides_last(&record);
}

/// `pause=` AFTER `task=` (the Autopilot design, §10.5.1), on `status` and
/// the `sessions` roster: the unix second the loop's pause after short
/// turns ends, told through the host's own wake (`App::apply_harness_fact`,
/// `HarnessFact::Pause`), printed under `task=`'s rule — an agent session
/// whose live claim THIS window's host holds — and `-` once the pause is
/// over. NEGATIVE CONTROLS: a pause told with no claim, or under another
/// holder's, prints `-`; so does one on a session that runs no agent.
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
    // Another holder's claim: this host hears nothing from its loop.
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

/// `autopilot=` beside `supervisor=` (the Autopilot design, §5): `-` for a session that runs no
/// agent the harness supervises; once one does, its effective Autopilot (`off` with no host to
/// supervise it, then the table's default under one, then the session's own word) on both arms
/// of the record. Negative control: the master switch off reads `off` whatever the word.
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

/// `launch=` and `launch_seq=` (the launch witness, `crate::launch_witness`)
/// ride after `human_seq=` and are judged against THAT count: `person` while
/// it is the mark's, `since` once a person's gesture moved it; the screen's
/// stamp stays last.
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

/// `editing=<n>` (amacs §6.7) counts the open `edit` tickets holding the
/// session, just before the stamp: each ticket's hold adds one, and its
/// resolve (or an orphan's lapse) takes it away again. A driver polling
/// `status` learns why its `send` answers `ERR busy editing` without trying.
#[test]
fn status_reports_editing_count() {
    use crate::control_edit::hold::{self, Hold};
    use amacs::protocol::edit::Ticket;

    let mut app = crate::App::headless_for_test();
    // A session id no other test's App mints: the hold map is process-wide.
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

    // Both sessions start unclassified. An unavailable terminal must not
    // invent an observed detail or revision for either one.
    let locked_term = app.pool.get(0).unwrap().term.clone();
    let guard = locked_term.lock().unwrap();
    assert_same(&app);
    drop(guard);
    // No hold, so no origin: the bridge's row says whose hold stands.
    assert!(
        app.fabric_status_fields(0)
            .unwrap()
            .contains(" hold=0 hold_origin=- ")
    );

    // One session becomes classified and held; its cached detail supplies
    // the same percent-encoded fallback when its terminal is contended.
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

    // The owner's local hold is named as one: the bridge never lifts it.
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
    // The writer keeps the lock until the batch is back, so `true` from the join means the batch
    // returned while the writer still held the lock. The minute is a hang detector only.
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

/// The `status` verb's reply, the machine contract a poller parses: ONE
/// line, `schema=1` first so a consumer can reject an unknown major, the
/// `seq= hash=` stamp last, and the SAME keys in the same order on both
/// arms — never classified and classified — so no field appears from
/// nowhere and an additive field cannot land on one arm only. Never
/// classified is `observed=false`, distinct from `phase=unknown` ("looked
/// and found no evidence"). The values a driver acts on come from their
/// sources: the hold, an adopted session's attribution
/// and the `tab_status` switch.
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
    // The field set a poller parses (`help status`): an additive field
    // lands before the `seq= hash=` stamp, and none is dropped or renamed.
    // `conpty=` rides before `integration=` on Windows only; `needs_told=`
    // rides `needs_n=` in a development build only, as every test builds.
    let conpty = if cfg!(windows) { " conpty" } else { "" };
    assert_eq!(
        keys(&observed).join(" "),
        format!(
            "schema sid subject subject_source observed phase since_ms outcome exit_code signal \
             detail confidence reasons attribution fs_consent conflict revision enabled hold \
             hold_advisory fabric fabric_rtt_ms fabric_link_age_ms fleet hand hand_pid level story why \
             needs needs_n needs_told hold_by typed_by typed_ms path program agent agent_detail agent_rev \
             agent_since_ms agent_gen agent_fp \
             program_state last_act os_appearance theme_side reported_scheme scheme_from bg_dark \
             session_theme theme_origin{conpty} integration input input_bytes input_wait_ms fg_rss_mb \
             disk disk_free_mb disk_room_mb disk_age_ms job job_quiet_ms syspolicyd \
             syspolicyd_workers syspolicyd_full_ms supervisor supervisor_proven autopilot \
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

/// `consent_at_risk` is emitted on the conjunction and nothing else: silent while the cwd is
/// unknown (every session without shell integration), present once a reported cwd lands under
/// a protected root. It is not a claim that a dialog is showing.
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

    // The conjunction half is Unix-only: the OSC 7 below is `file://localhost<cwd>` with a
    // `/`-rooted cwd, and a Windows protected root is `C:\Users\…`.
    #[cfg(unix)]
    {
        // The protected roots are resolved data from the consent module: this file writes no
        // protected-folder literal (the `protected-folder` guard).
        // They are the App's published mirror, the same value the record's
        // consent check reads, so a seamless test pointing the process-global
        // HOME at a scratch dir cannot make the two disagree.
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

/// A prompt is `Idle` however its grid moves. `EnteringCommand` is the steady
/// state whenever a prompt is DISPLAYED, so the keystroke echo of a command
/// line being typed and a background job printing at an idle prompt
/// (`tail -f &`, a sibling build) look alike here, and neither is work: the
/// record publishes nothing new and owes the event loop no wake, so typing
/// costs no chrome refresh. A bare prompt (`133;A` alone) is the same
/// `tail -f &` case, and movement never promotes either to `Running`.
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

        // The grid moves at the prompt: a keystroke echo or ambient output.
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

/// A TUI that outputs without moving the grid (identical sync-bracketed repaints, heartbeats)
/// keeps a `Running` its grid movement started alive through `output_recently` while
/// `last_movement` goes stale, so the retire wake rides the fresher clock (`owed_wake` says why).
#[test]
fn output_without_movement_arms_the_retire_wake_off_the_freshest_clock() {
    let t0 = Instant::now();
    let mut observer = StatusObserver::new(policy(), Duration::from_millis(0));
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    ev.activity.last_output = Some(t0);
    observer.observe(1, &ev, t0);
    // One real grid movement starts `Running` (output alone never does), then the grid goes
    // still while output flows.
    ev.activity.content_seq = 2;
    ev.activity.last_output = Some(t0 + DWELL);
    let t1 = settle_observer(&mut observer, 1, &ev, t0 + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

    // Far past `quiet_after` on the movement clock, raw output still sustains Running, so the
    // owed wake rides the output clock.
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

    // At that wake the phase retires with no new bytes: output sustains the busy state, true
    // silence ages it out.
    observer.observe(1, &ev, owed);
    observer.observe(1, &ev, owed + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Quiet));
}

/// `next_wake` clamps every slot's owed instant to its own observation gate: `due()` refuses the
/// slot until `next_due`, so an earlier deadline could only fire, be refused and re-arm. The
/// clamp bounds any such bug to the observation rate.
#[test]
fn an_armed_wake_never_precedes_its_own_observation_gate() {
    let t0 = Instant::now();
    let interval = Duration::from_millis(250);
    let mut observer = StatusObserver::new(policy(), interval);
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(false);
    observer.observe(1, &ev, t0);

    // Re-observe near the dwell edge: the pending candidate's raw owed
    // instant (t0 + dwell) now precedes the refreshed observation gate.
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

    // The clamped wake still publishes: dwell was served well before it.
    assert!(observer.observe(1, &ev, owed), "the transition lands");
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Idle));
}

/// The sweep skips a session whose terminal `try_lock` the PTY reader holds, a path that never
/// reaches `observe`; `note_skipped` still advances the slot's `next_due`, or `next_wake` would
/// re-arm the same past instant every turn under the sustained output that contends the lock.
#[test]
fn a_refused_sample_still_advances_the_observation_gate() {
    let t0 = Instant::now();
    let interval = Duration::from_millis(250);
    let mut observer = StatusObserver::new(policy(), interval);

    // A Running session whose clocks then go stale: `owed_wake` returns
    // `last_activity + quiet_after`, which recedes into the past.
    let mut ev = evidence(blank(1));
    ev.foreground_job = Some(true);
    observer.observe(1, &ev, t0);
    // Movement needs two samples and the candidate then serves its dwell, so the third
    // observation publishes `Running` (`owed_wake` reads the published record).
    let moved_at = t0 + Duration::from_millis(10);
    ev.activity.content_seq = 2;
    observer.observe(1, &ev, moved_at);
    observer.observe(1, &ev, moved_at + DWELL);
    assert_eq!(observer.status(1).map(|s| s.phase), Some(Phase::Running));

    // Long past the retirement instant, with the lock contended ever since.
    let now = t0 + QUIET + Duration::from_secs(30);
    assert!(
        observer.next_wake().is_some_and(|wake| wake < now),
        "the shape under test: an owed wake already deep in the past"
    );

    observer.note_skipped(1, now);
    // Every sweep ends in `note_swept`, which restores `next_due_any` to the exact minimum
    // (`note_skipped`, like `observe`, only folds a lower bound); mirrored here.
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

    // An unknown session is not charged: it holds no slot, so it owes no deadline, and
    // inserting one would bank the pool epoch over a session that was never classified.
    observer.note_skipped(404, now);
    assert!(
        !observer.knows(404),
        "a refusal never invents a classification"
    );
}

/// The RFC §4 privacy rule as a table: the program's basename, an
/// allow-listed subcommand at most, never an argument — and the wrappers,
/// assignments and paths an agent's real command lines carry.
#[test]
fn command_detail_keeps_the_program_and_an_allowlisted_subcommand_only() {
    let cases: &[(&str, Option<&str>)] = &[
        ("claude --dangerously-skip-permissions", Some("claude")),
        // A prompt glyph a scrape left in front is not the program.
        ("$ sleep 30", Some("sleep")),
        ("❯ codex", Some("codex")),
        ("% ", None),
        ("codex", Some("codex")),
        ("targo --unverified test -p x", Some("targo test")),
        ("cargo build --release", Some("cargo build")),
        ("git status", Some("git status")),
        // `env -S`'s one string: the subcommand is read from its words,
        // from the closed list, and never an argument of it.
        (
            "env -S 'git status --porcelain /secret/repo'",
            Some("git status"),
        ),
        ("env -S 'git /secret/tok'", Some("git")),
        // A flag's value is never a subcommand, even one that looks like a bare identifier:
        // only vocabulary membership tells them apart, and a directory, namespace, context or
        // prefix is what the rule keeps off the wire.
        ("git -C /secret/repo status", Some("git")),
        ("git -C aterm status", Some("git")),
        ("kubectl -n prod get pods", Some("kubectl")),
        ("docker --context customer-acme ps", Some("docker")),
        ("npm --prefix web run build", Some("npm")),
        // A word outside the vocabulary is an argument even in the subcommand slot.
        ("git customer-acme", Some("git")),
        ("cargo xtask release", Some("cargo")),
        ("ssh me@host", Some("ssh")),
        ("/usr/local/bin/python3 secret.py", Some("python3")),
        ("./run.sh --token abc", Some("run.sh")),
        // Quoting is honoured: a quoted path with a space is one word, and a Windows launcher's
        // `.exe` names the same program as the bare name.
        (
            "\"C:\\Program Files\\Git\\usr\\bin\\less.exe\" README.md",
            Some("less"),
        ),
        // pwsh's call operator (a glyph, skipped as a prompt's is), then a single-quoted path
        // with a space, and the 8.3 short spelling of the same path.
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
        // An escaped space is part of the word, but a program's name is never published with
        // whitespace: it ends at the first one.
        ("my\\ tool --flag", Some("my")),
        ("\"git\" status", Some("git status")),
        // Only the executable extensions Windows resolves are dropped: a dotted program name is
        // not a path with an extension.
        ("python3.11 -m http.server", Some("python3.11")),
        ("/usr/bin/a.out", Some("a.out")),
        ("FOO=1 cmd --secret", Some("cmd")),
        ("FOO=1 BAR=two targo test", Some("targo test")),
        ("sudo -u root targo --unverified test", Some("targo test")),
        ("sudo apt install x", Some("apt")),
        ("env FOO=1 codex", Some("codex")),
        // A wrapper by full path is the same wrapper: basename first.
        ("/usr/bin/env FOO=1 codex", Some("codex")),
        (
            "/usr/bin/sudo -u root targo --unverified test",
            Some("targo test"),
        ),
        // A make/just target is user-named: only the conventional ones
        // are vocabulary.
        ("time make deploy-prod", Some("make")),
        ("make test", Some("make test")),
        ("just fmt", Some("just fmt")),
        ("aterm ctl ls", Some("aterm ctl")),
        ("atpkg install foo", Some("atpkg install")),
        ("nice -n 5 kubectl get pods -n prod", Some("kubectl get")),
        ("docker compose up", Some("docker compose")),
        ("'claude' 'resume'", Some("claude")),
        // Control removal applies before wrapper/vocabulary matching.
        ("clau\u{7f}de --private", Some("claude")),
        ("s\u{7f}udo -u user codex", Some("codex")),
        ("targo te\u{7f}st private", Some("targo test")),
        ("npm run build:prod", Some("npm run")),
        ("pnpm dlx create-foo", Some("pnpm dlx")),
        ("yarn build --token abc", Some("yarn build")),
        // A subcommand slot holding a path or an assignment is dropped.
        ("make ./target/x", Some("make")),
        // A compound line names the program of the segment that is running, not its first word
        // (`cd x && claude` is the supervise-agent skill's launch shape): the last of an `&&`/`;`
        // chain; before a trailing `|| …`, which runs only on failure, its operand; `exec` is a
        // wrapper.
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
        // A pipeline is still named by its first command.
        ("cd x && cat log | grep err", Some("cat")),
        // An operator inside quotes or behind a backslash is an argument,
        // not a cut.
        ("claude -p \"fix a; then b\"", Some("claude")),
        ("echo 'x && y' && codex", Some("codex")),
        ("echo a \\; codex", Some("echo")),
        // A keyword opener stays the keyword: the body is not parsed, and the `;` in `for …; do`
        // is the keyword's grammar, not a list operator. `{` opens a group the same way.
        ("for i in 1 2 3; do sleep 1; done", Some("for")),
        ("$ for i in 1 2 3; do sleep 1; done", Some("for")),
        ("while true; do claude; done", Some("while")),
        ("if [ -f x ]; then claude; fi", Some("if")),
        ("{ cd x; claude; }", Some("cd")),
        // The keyword need not open the line: behind `cd x &&` or a wrapper the running
        // segment's `;`-cut would be a closer (`done`, `fi`), so the first segment whose program
        // slot holds an opener is read instead.
        ("cd x && for i in 1 2 3; do sleep 1; done", Some("for")),
        ("cd x && if true; then claude; fi", Some("if")),
        ("time for i in 1 2 3; do sleep 1; done", Some("for")),
        ("", None),
        ("   \t ", None),
        (";", None),
    ];
    // A keyword that closes a compound command is never a program, so no row may read one.
    const KEYWORD_CLOSERS: [&str; 8] = ["done", "fi", "esac", "}", "then", "do", "else", "elif"];
    for (cmd, want) in cases {
        let got = command_detail(cmd);
        assert_eq!(got.as_deref(), *want, "{cmd:?}");
        assert!(
            !got.as_deref().is_some_and(|d| KEYWORD_CLOSERS.contains(&d)),
            "{cmd:?} reads a closer, which is never a program: {got:?}"
        );
    }
    // Bounded: a screen-scraped line can be long; the reply cannot.
    let long = format!("{} arg", "p".repeat(200));
    let detail = command_detail(&long).unwrap();
    assert_eq!(detail.chars().count(), 48);
    assert!(
        detail.capacity() <= 48 * 4,
        "long input capacity must not be retained"
    );
    let wide = format!("{} private", "終".repeat(80));
    assert_eq!(command_detail(&wide), Some("終".repeat(48)));
    // A program name is never reported with a subcommand it does not own.
    assert_eq!(command_detail("claude commit").as_deref(), Some("claude"));
}

/// A quoted or escaped space is part of its word, so a program at a spaced
/// path reads its name — the live upgrade's relaunch from `Application
/// Support` read `detail=Application` (D3, 2026-09-26), and pwsh's backtick
/// escape read `detail=Program` (0.95.0) — and that quote must never carry
/// an ARGUMENT into `detail=`: the first D3 fix published `'t' --token
/// SECRET`, a session id, and `tok`/`hunter2` from lines whose quoting POSIX
/// and fish read differently. A word ends where EITHER reading ends it, a
/// quote left open ends it at its first whitespace, `env -S`'s string is
/// read for its first word, and the published name never holds whitespace.
#[test]
fn a_quoted_program_path_reads_its_name_and_never_an_argument() {
    let store = "Library/Application Support/aterm/pkg/agents/claude";
    let abs = format!("/Users//me/{store}");
    let cases: &[(String, &str)] = &[
        // The upgrade's relaunch line, verbatim in shape, and its spellings.
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
        // A quoted assignment with a space is one assignment, skipped.
        ("env 'FOO=a b' '/opt/my tools/codex' resume".into(), "codex"),
        // CONTROL: no space in the path; an argument with spaces.
        (
            "/Users//me/.local/share/claude/versions/2.1.281 --resume x".into(),
            "2.1.281",
        ),
        (
            "claude -p 'fix the bug in Application Support'".into(),
            "claude",
        ),
        // pwsh's backtick escape inside a Windows path; a backtick after a
        // bare name ends the word as a POSIX shell would.
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
        // bash/zsh `$'…'`: a piece of the program's name, never the argument.
        (r"$'/usr/local/bin/don\'t' --token SECRET".into(), "t"),
        // fish quoting of an apostrophe home: POSIX runs on into the id.
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
        // An apostrophe in a bare word opens a quote that never closes, and
        // an argument's opening quote may close it.
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
        // `env -S`'s ONE string: its first word, never a basename in it.
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
        // Control: `-u` takes a value, so `-uS` unsets `S` and is no split.
        ("env -uS /usr/bin/node /srv/x.js".into(), "node"),
        // A space in the program's own name is never published.
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

/// `executing_detail` is the one producer: an executing block names its (sanitized)
/// command; a prompt, an entering block and a completed block name nothing.
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
    // OSC 633;E's explicit command line wins over the screen scrape.
    term.process(b"\x1b]133;A\x07$ \x1b]633;E;claude --dangerously-skip-permissions\x07\x1b]133;B\x07claude --dangerously-skip-permissions\n\x1b]133;C\x07");
    assert_eq!(executing_detail(&term).as_deref(), Some("claude"));
    term.process(b"\x1b]133;D;0\x07");
    // A wide prompt glyph before the 133;B mark: the cut is by display
    // column, so the program keeps its first letter.
    term.process(
        "]133;A🐱 $ ]133;Bcodex resume
]133;C"
            .as_bytes(),
    );
    assert_eq!(executing_detail(&term).as_deref(), Some("codex"));
}

/// A FULL CLEAR WHILE THE COMMAND RUNS (a program that clears and draws on
/// the main screen): the engine moves the block's rows the clear blanked to
/// the cleared screen's top row and its command rows are gone, so the scrape
/// names nothing rather than what the program drew on the row the command
/// was typed on; OSC 633;E's line still names it. CONTROL: before the clear
/// both roads name `vim`. NEGATIVE CONTROL (the pre-fix reading): that row
/// now reads the program's text, which the scrape would have named
/// `python3`.
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

/// The `status` record's `detail=`: the sanitized running command while the block executes, `-`
/// otherwise; answered before the classifier's first publish and never by waiting on a contended
/// terminal. Under contention the subject reads `unavailable` rather than a lower rung (which a
/// poller would read as a title change), and a pin answers from the metadata leaf lock.
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
    // The argument is a word no other field can spell: a number could match one of the
    // record's wall-clock ages (`since_ms=`, `agent_since_ms=`).
    assert!(
        !record.contains("xyzzy"),
        "arguments never reach the wire: {record}"
    );

    // Contended: `-`, never a wait, and the subject is `unavailable`, not
    // a lower rung.
    let held = term.lock().unwrap();
    let record = app.session_status_record(0).expect("live session");
    assert!(
        record.contains(" subject=- subject_source=unavailable ") && record.contains(" detail=- "),
        "a contended terminal is `-`, not a stall or a lower rung: {record}"
    );

    // A pin answers the subject from the leaf lock with the terminal still
    // held, and keeps the detail once the terminal is free.
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

/// The sweep folds `executing_detail` onto the published status ([`StatusObserver::set_detail`]):
/// an Executing block publishes the sanitized program, and idle clears it.
#[test]
fn the_sweep_publishes_the_running_detail_onto_the_chrome_status() {
    let mut app = crate::App::headless_for_test();
    let term = app.pool.get(0).expect("session 0").term.clone();
    let t0 = Instant::now();

    // Idle: no executing block, no detail.
    app.observe_session_statuses(t0);
    assert_eq!(
        app.session_status.status(0).and_then(|s| s.detail.clone()),
        None,
        "idle publishes no detail"
    );

    // An executing block: the sweep folds its sanitized program onto the status.
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

    // The block completes: the detail clears.
    term.lock().unwrap().process(b"\x1b]133;D;0\x07");
    let t2 = t1 + Duration::from_secs(10);
    app.observe_session_statuses(t2);
    assert_eq!(
        app.session_status.status(0).and_then(|s| s.detail.clone()),
        None,
        "a completed block clears the chrome detail"
    );
}

/// `task_state=<state|-> task_rev=<n>` (2026-10-15), after `pause=`: the
/// session's task record, read from the timeline leaf the agent columns come
/// from — `-`/`0` for a session with no task, the state and rev once one is
/// marked — and named apart from `task=`, the task's face under Land, which
/// a session with no live claim here prints `-` whatever its record says.
/// Additive: the stamp still rides last.
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

/// `conpty=` names the console host a Windows session runs in, `-` for a key
/// no pseudoconsole is behind, and is absent off Windows, where a live
/// record keeps the bytes every earlier build wrote.
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
    // The headless App's session is a stub key: on Windows it reads `-`,
    // elsewhere the field is not there at all.
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

/// ONE FOREGROUND READ PER LOOK. The group in front (`tcgetpgrp` on the PTY
/// master, ~51k instructions per call on macOS) is read once, under the
/// terminal hold, and the cwd witness, the job evidence and the program
/// identity all take that answer: a sweep over N looked-at sessions asks the
/// probe N times, however many readers each answer has, and the job verdict
/// is the injected probe's. NEGATIVE CONTROL: the sweep as it was (a read
/// under the hold, the job probe's own `tcgetpgrp`, and a third read for the
/// program identity) asks the injected probe twice per look and fails the
/// count, and its job verdict ignores the scripted group.
#[test]
fn a_look_reads_the_foreground_group_once() {
    use std::cell::Cell;
    thread_local! {
        static READS: Cell<u64> = const { Cell::new(0) };
    }
    // Groups no process holds: past every pid the kernel issues.
    const SHELL: i32 = 2_000_000_201;
    const JOB: i32 = 2_000_000_202;
    /// The injected probe: a job in front of every session, counted.
    fn counted(_master: i32) -> i32 {
        READS.with(|r| r.set(r.get() + 1));
        JOB
    }
    const STAGED: u64 = 8;
    let mut app = crate::App::headless_for_test();
    app.session_factory.fg_probe = counted;
    for i in 0..STAGED {
        let mut session = crate::stub_session(1000 + i);
        // A descriptor number no file holds: the probe is scripted, and
        // nothing below it touches the fd.
        session.master = i32::MAX - i32::try_from(i).expect("small fixture");
        session.pid = SHELL;
        app.pool.insert(session);
    }
    let t0 = Instant::now();
    let sweeps = 4u32;
    for turn in 0..sweeps {
        // One observation interval apart: every staged session is due.
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

/// `status needs_n=` (Autopilot v5 §12.2.1): a harness's ask naming one more
/// need reads `needs=ask needs_n=2`; a lone need `needs_n=1`; an approval,
/// another owner's ask and no need `needs_n=-`. NEGATIVE CONTROL (run for the
/// landing): counting only the first need (`needs_n=1` for the counted
/// entry) fails the first row.
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

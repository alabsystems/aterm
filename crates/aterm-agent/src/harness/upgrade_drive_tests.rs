// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::relaunch::{
    AtPrompt, CAUSE_EXIT, CAUSE_HOST, CONTINUE_FENCE_TRIES, EXIT_LOOK, EXIT_SETTLE, ExitRecord,
    HELD_WAIT_IN_TEST, MODEL_WAIT, RELAUNCH_FENCE_TRIES, RelaunchLineError, Restart, STALE_S,
    Snapshot, WAITS_IN_TEST, WaitsInTest, after_exit, after_host_ended, await_new,
    carry_on_with_tab_probe, continuation_session, exit_fallback, exit_record, fallback_origin,
    follow, line_for, look_at_exit, model_restart, owed, relaunch_by_parent, restart_from, resume,
    resumed_prompt, shell_dialect, type_relaunch_line, with_model,
};
use super::super::upgrade::Dialect;
use super::*;
use crate::supervise::ScratchDir;
// Every child these tests start holds its stdio and nothing else of this process: on Darwin a
// child that inherits another test thread's pipe (open between its `pipe()` and `F_SETFD`)
// keeps that pipe's reader waiting as long as the child lives.
use aterm_uds::spawnfd::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Where a test plants `session`'s record by hand: the record module keeps its
/// own path private, so the one writer is the only shipped way to it.
fn state_path(opts: &Opts, session: &str) -> std::path::PathBuf {
    state_dir(opts).join(format!("{session}.json"))
}

/// A `text --json trim tail=3` reply from a busy Claude Code tab: the cursor
/// row is absolute (57) and `first` is where the reply's rows start, so the
/// caret sits on reply row 0, column 2.
const SCREEN: &str = r#"{"rows":["❯","────────","  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← for agents"],"cursor":{"row":57,"col":2,"visible":true,"style":"blinking_block"},"dims":{"rows":60,"cols":144},"seq":232803,"trimmed":0,"first":57}"#;

/// The same tab's `status`, read the same minute.
const STATUS: &str = "OK schema=1 sid=0 subject=%E2%97%90%20Git%20commit%20and%20push \
    subject_source=osc observed=true phase=running since_ms=4431314 outcome=none exit_code=- \
    signal=- detail=- confidence=strong reasons=fg_job,content_activity attribution=adopted \
    fs_consent=unknown conflict=false revision=171 enabled=true hold=0 fabric=absent \
    fabric_rtt_ms=- fabric_link_age_ms=- identity=- hand=- level=quiet story=0 seq=232803 \
    hash=7adef2d5e8547a9e";

#[test]
fn a_screen_reply_is_read_with_the_cursor_made_relative_to_its_rows() {
    let s = parse_screen(SCREEN).expect("the measured reply parses");
    assert_eq!(s.rows.len(), 3);
    assert_eq!(s.cursor, Some((0, 2)));
    assert_eq!(s.seq, 232_803);
    assert_eq!(s.first, 57, "the screen row `cell` addresses rows[0] by");
    assert!(upgrade::composer_is_empty(&s.rows, s.cursor, false));
    // A cursor above the reply's first row is no cursor, not row 0.
    let above = SCREEN.replace(r#""row":57"#, r#""row":12"#);
    assert_eq!(parse_screen(&above).expect("parses").cursor, None);
    assert!(parse_screen("not json").is_none());
    // The person stamp (`"human_ms"`), read as the supervisor reads it: a
    // host that sends none is unknown, `null` is never, a number its age.
    assert_eq!(s.human, HumanInput::Unknown, "an older host sends none");
    let with = |h: &str| {
        parse_screen(&SCREEN.replace(
            r#""seq":232803"#,
            &format!(r#""seq":232803,"human_ms":{h}"#),
        ))
        .expect("parses")
        .human
    };
    assert_eq!(with("null"), HumanInput::Never);
    assert_eq!(with("4200"), HumanInput::Ago(4200));
    assert_eq!(with("\"12\""), HumanInput::Unknown);
}

/// The upgrade's gate reads the live zone's busy footer (`aterm_phase::busy_zone_start`): a dead
/// process's `esc to interrupt` above a new launch card does not hold it at busy, the new
/// session's own footer does, and a read too short for the frame is read whole.
#[test]
fn the_gate_reads_the_live_footer_not_a_dead_processs() {
    use aterm_phase::prompt::fixtures as f;
    assert!(!busy_footer(&f::screen(f::EXIT_RELAUNCH_STALE_FOOTER)));
    assert!(!busy_footer(&f::screen(
        f::EXIT_RELAUNCH_STALE_FOOTER_QUESTION2
    )));
    assert!(busy_footer(&f::screen(f::EXIT_RELAUNCH_STALE_FOOTER_BUSY)));
    let tail = parse_screen(SCREEN).expect("the measured reply parses");
    assert!(busy_footer(&tail.rows));
}

/// A proven self-claim (`supervisor_proven=1`) is its agent's session: no
/// upgrade decision of aterm's ([`due`], [`step`]). NEGATIVE CONTROLS: an
/// unproven claim of the same spelling, an older server with no field, and
/// an `ERR` are none.
#[test]
fn a_proven_self_claim_is_read_off_status_and_nothing_else_is() {
    let proven =
        "OK schema=1 sid=0 supervisor=codex-alab%40500 supervisor_proven=1 autopilot=cruise";
    assert!(self_claimed_by_status(proven));
    assert!(!self_claimed_by_status(
        &proven.replace("supervisor_proven=1", "supervisor_proven=0")
    ));
    assert!(!self_claimed_by_status(
        "OK schema=1 sid=0 supervisor=codex-alab%40500 autopilot=cruise"
    ));
    assert!(!self_claimed_by_status("ERR supervisor_proven=1"));
    assert!(!self_claimed_by_status("OKAY supervisor_proven=1"));
    assert!(!self_claimed_by_status("OKsuffix supervisor_proven=1"));
    assert!(!self_claimed_by_status("OK detail=supervisor_proven%3D1"));
    assert_eq!(SKIP_SELF_CLAIMED, "skip:self-claimed");
    assert!(
        !Report {
            pid: 0,
            tab: "s-x".to_string(),
            session: "-".to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: SKIP_SELF_CLAIMED.to_string(),
        }
        .is_act()
    );
}

/// A person at the keyboard wins: a tab a person typed into within
/// `[harness] human_grace_s` is held like a hand on it. Negative controls: a
/// keystroke older than the grace, `human_ms=-` and no field at all (an older
/// server) are nobody.
#[test]
fn a_tab_is_held_by_a_halt_a_hand_a_person_or_an_unreadable_status() {
    assert!(!held_by_status(STATUS, 120));
    assert!(held_by_status(&STATUS.replace("hold=0", "hold=1"), 120));
    assert!(held_by_status(
        &STATUS.replace("hand=-", "hand=turn:7"),
        120
    ));
    assert!(held_by_status("ERR no such session", 120));
    assert!(held_by_status("", 120));
    let typed = |ms: &str| STATUS.replace("hand=-", &format!("hand=- human_ms={ms}"));
    assert!(held_by_status(&typed("4000"), 120));
    assert!(!held_by_status(&typed("121000"), 120));
    assert!(!held_by_status(&typed("-"), 120));
    assert!(held_by_status(&typed("119999"), 120));
    assert!(
        !held_by_status(&typed("4000"), 0),
        "a zero grace holds nobody"
    );
}

/// The harness's own hand (`hand=lease:aterm-harness@<this pid>`) holds nothing against
/// itself; another holder's lease, a driver's turn, and input its program has not read yet (a
/// signal would drop it) hold the tab. Negative controls: another process's harness hand is
/// someone else's, and `input=clear`/`-` holds nothing.
#[test]
fn the_harness_own_hand_holds_nothing_against_it_and_unread_input_holds_the_tab() {
    let ours = STATUS.replace("hand=-", &format!("hand=lease:{}", hand_name()));
    assert!(our_hand(&format!("lease:{}", hand_name())));
    assert!(!held_by_status(&ours, 120), "{ours}");
    assert!(!crate::harness::relaunch::driver_hand(&ours));
    assert!(!crate::harness::relaunch::held_by_someone(&ours));
    for theirs in [
        "hand=lease:orchestrator".to_string(),
        "hand=lease:aterm-harness@1".to_string(),
        "hand=turn:7:manager".to_string(),
    ] {
        let line = STATUS.replace("hand=-", &theirs);
        assert!(held_by_status(&line, 120), "{line}");
        assert!(crate::harness::relaunch::driver_hand(&line), "{line}");
    }
    for input in ["pending", "typeahead", "stalled", "stopped"] {
        let line = STATUS.replace("hand=-", &format!("hand=- input={input}"));
        assert!(held_by_status(&line, 0), "{line}");
    }
    for input in ["clear", "-"] {
        let line = STATUS.replace("hand=-", &format!("hand=- input={input}"));
        assert!(!held_by_status(&line, 0), "{line}");
    }
}

#[test]
fn who_roster_exposes_a_unique_foreground_group_without_a_window_hop() {
    let body = "0 s-a driving=- watchers=0 turns=0 alive nonce=0123456789abcdef0123456789abcdef fgpgid=101\n\
                1 s-b driving=- watchers=1 turns=3 alive nonce=abcdef0123456789abcdef0123456789 fgpgid=-\n";
    let tabs = parse_host_roster("OK 2", body).expect("who roster");
    assert_eq!(tabs[0].fgpgid, Some(101));
    assert_eq!(tabs[1].fgpgid, None);
    assert_eq!(unique_tab_for_group(&tabs, 101), Some("s-a"));
    assert_eq!(parse_host_roster("OK 0", ""), Some(Vec::new()));
    assert_eq!(parse_host_roster("ERR busy", body), None);
    assert_eq!(parse_host_roster("OKAY", body), None);
    assert_eq!(
        parse_host_roster("OK 2", &body.replace("s-b", "s-a")),
        None,
        "duplicate tab ids are not an ownership roster"
    );
    let ambiguous = [
        tabs[0].clone(),
        LiveTab {
            sid: "s-c".to_string(),
            fgpgid: Some(101),
        },
    ];
    assert_eq!(unique_tab_for_group(&ambiguous, 101), None);
}

/// A conclusive Codex answer scans no Claude session file. Negative control: no answer (an
/// unknown or Claude foreground) still reads Claude's roster.
#[test]
fn codex_due_answers_do_not_scan_claude_sessions() {
    let dir = scratch("codex-due-skips-claude-roster");
    let opts = Opts {
        only_sid: Some("s-codex".to_string()),
        ..drive(&dir)
    };
    let sessions = opts.home.join(".claude/sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    std::fs::write(
        sessions.join("4000001.json"),
        format!(
            r#"{{"pid":4000001,"sessionId":"{SESSION}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("Claude session file");
    let tabs = [LiveTab {
        sid: "s-codex".to_string(),
        fgpgid: Some(4_000_002),
    }];
    let check = |answer: Option<Due>| {
        SESSION_FILE_SCANS.with(|count| count.set(0));
        let due = due_after_codex(&opts, &tabs, answer);
        (due, SESSION_FILE_SCANS.with(std::cell::Cell::get))
    };
    assert_eq!(
        check(Some(Due::No)),
        (Due::No, 0),
        "current Codex is conclusive"
    );
    assert_eq!(
        check(Some(Due::Yes)),
        (Due::Yes, 0),
        "old Codex is conclusive"
    );
    assert_eq!(
        check(None),
        (Due::No, 1),
        "unknown or Claude foreground still reads Claude's roster"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tab whose agent's own switch is off is never due, and its restart in flight is no pass's
/// ([`Opts::agents_off`], [`lane_on`]): the host is never told a step is due that [`lanes`]
/// then leaves alone, and its carry after an update never steps an off agent's restart. A Codex
/// answer is dropped while Codex is off; the Claude half reads no session record while Claude
/// Code is off. Negative controls: the other agent's switch off, or none, answers as usual.
#[test]
fn a_tab_whose_agents_switch_is_off_is_never_due() {
    use aterm_phase::Program;
    use upgrade::Agent::{Claude, Codex};
    let dir = scratch("due-agent-off");
    let base = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    let off = |program: Program| Opts {
        agents_off: vec![program],
        ..base.clone()
    };
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(4_000_002),
    }];
    let check = |opts: &Opts, codex: Option<Due>| {
        SESSION_FILE_SCANS.with(|count| count.set(0));
        let due = due_after_codex(opts, &tabs, codex);
        (due, SESSION_FILE_SCANS.with(std::cell::Cell::get))
    };
    // A Codex behind leads the tab.
    assert_eq!(check(&off(Program::Codex), Some(Due::Yes)), (Due::No, 0));
    assert_eq!(
        check(&off(Program::Claude), Some(Due::Yes)),
        (Due::Yes, 0),
        "control: Claude Code's switch is no Codex's"
    );
    // No Codex leads it: the tab is Claude Code's half to read.
    assert_eq!(check(&off(Program::Claude), None), (Due::No, 0));
    assert_eq!(
        check(&off(Program::Codex), None),
        (Due::No, 1),
        "control: Claude Code's roster is read"
    );

    // This tab's restart in flight, Claude Code's and then Codex's.
    std::fs::create_dir_all(state_dir(&base)).expect("state");
    for (agent, name, own, other) in [
        (Claude, SESSION.to_string(), Program::Claude, Program::Codex),
        (
            Codex,
            format!("codex-{TAB}"),
            Program::Codex,
            Program::Claude,
        ),
    ] {
        let st = St {
            agent,
            phase: Phase::Exiting { at_s: now_s() },
            pid: dead_pid(),
            shell: std::process::id(),
            tab: TAB.to_string(),
            ..St::default()
        };
        std::fs::write(state_path(&base, &name), st.to_json()).expect("write");
        assert!(in_flight(&off(own)).is_empty(), "{agent:?}");
        assert_eq!(
            due(&off(own)),
            Due::Unread(UNREAD_SOCKET),
            "{agent:?}: the restart is no answer; the roster decides"
        );
        assert_eq!(in_flight(&off(other)), [name.as_str()], "{agent:?}");
        assert_eq!(due(&off(other)), Due::Yes, "control: {agent:?}");
        assert_eq!(due(&base), Due::Yes, "control: {agent:?}");
        std::fs::remove_file(state_path(&base, &name)).expect("rm");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A build whose `--version` timed out decides nothing: the version cache
/// ([`cached_version_with`]) holds it unanswered until its back-off, probing nothing before
/// then, and the look is [`UNREAD_VERSION`], never "not due". Negative controls: once answered
/// it decides (`No` on it, `Yes` behind it); another tab's agent's record is [`UNREAD_RECORD`].
#[test]
fn a_build_whose_version_timed_out_is_unread_never_not_due() {
    let dir = scratch("due-unanswered");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    let twin = dir.join("agents/claude");
    std::fs::create_dir_all(dir.join("agents")).expect("agents");
    std::fs::write(&twin, "#!/bin/sh\necho 1.0.0\n").expect("the twin");
    let probes = std::cell::Cell::new(0);
    let look = |files: &[SessionFile], group: i64, now: Instant, answer: Probed| {
        due_among(
            &opts,
            files,
            &the_tab(),
            || {
                Targets::read_with(Some(twin.clone()), &opts.home, |exe| {
                    cached_version_with(exe, now, |_| {
                        probes.set(probes.get() + 1);
                        answer.clone()
                    })
                })
            },
            |_| Some(atpkg::caller_shell::ProcArgs::default()),
            |_| Some(group),
            |_| Some((10, group, group)),
        )
    };
    let on_it = [record("1.0.0")];
    let t0 = Instant::now();
    assert_eq!(
        look(&on_it, 101, t0, Probed::TimedOut),
        Due::Unread(UNREAD_VERSION),
        "a build that did not answer decides nothing"
    );
    assert_eq!(probes.get(), 1);
    let before = t0 + VERSION_RETRY_FIRST - Duration::from_secs(1);
    assert_eq!(
        look(&on_it, 101, before, Probed::TimedOut),
        Due::Unread(UNREAD_VERSION),
        "before the back-off: the same"
    );
    assert_eq!(probes.get(), 1, "and no probe before it");
    assert_eq!(
        look(&on_it, 202, before, Probed::TimedOut),
        Due::Unread(UNREAD_RECORD),
        "another tab's agent's record is none of this tab's"
    );
    let answers = Probed::Version(Version::parse("1.0.0").expect("v"));
    assert_eq!(
        look(&on_it, 101, t0 + VERSION_RETRY_FIRST, answers.clone()),
        Due::No,
        "answered at the back-off, and the session is on it"
    );
    assert_eq!(probes.get(), 2, "asked again at the back-off");
    assert_eq!(
        look(&[record("0.9.0")], 101, t0 + VERSION_RETRY_FIRST, answers),
        Due::Yes,
        "answered, and the session is behind it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_phase_survives_the_state_file() {
    let to = Candidate {
        exe: PathBuf::from("/x/claude"),
        version: Version::parse("2.1.281").expect("version"),
        source: Source::Native,
    };
    let from = Version::parse("2.1.280").expect("version");
    for phase in [
        Phase::Pending,
        Phase::Announced { at_s: 7, asks: 2 },
        Phase::Exiting { at_s: 8 },
        Phase::Relaunched { at_s: 9 },
        Phase::Done,
        Phase::Failed("argv:--print".to_string()),
    ] {
        let st = St {
            phase,
            marker: "ATERM-UPGRADE-READY-0badf00d".to_string(),
            pid: 9162,
            notice_pid: 9162,
            notice_start: "Mon Sep 21 12:00:00 2026".to_string(),
            shell: 9001,
            tab: "s-b5cf2faabac5ce5127bd".to_string(),
            line: " . '/h/.aterm/shell.d/00-atpkg.zsh'; rehash; '/x/claude' '--resume' 'id'"
                .to_string(),
            last_seq: 232_803,
            noted: "terminal:tmux".to_string(),
            model_before: "claude-opus-5-5".to_string(),
            mark: 45_092_357,
            launch_model: "claude-sonnet-5".to_string(),
            confirm_by: 1_790_000_120,
            resumed_on: "2.1.282".to_string(),
            resumed_pid: 9163,
            // The move's instant, kept through Done for the finished record.
            relaunched_at: 1_790_000_090,
            // The command block its line was typed into, by which its end is read.
            line_block: Some(12),
            hold_since_s: 1_790_000_060,
            hold_seen_s: 1_790_000_120,
            // What the record keeps of the process it is about and of the
            // work it waited on (an older build's file has none of them).
            proc_start: "Mon Sep 21 11:59:00 2026".to_string(),
            own_work_at: 1_790_000_030,
            waits: vec![WaitNote {
                at: 1_790_000_030,
                word: "background".to_string(),
                note: "status=busy holders=3".to_string(),
            }],
            // A relaunch's cause, so whichever caller carries it on types
            // the relaunch's words, not the upgrade's.
            cause: CAUSE_EXIT.to_string(),
            ..St::fresh(&from, &to, Proc::default(), 1_790_000_000)
        };
        assert_eq!(St::from_json(&st.to_json()), Some(st));
    }
    assert_eq!(St::from_json(r#"{"phase":"sideways"}"#), None);
    // A record with no cause (an older build's) reads as the upgrade's.
    let old = r#"{"phase":"relaunched","at":7}"#;
    assert_eq!(St::from_json(old).map(|s| s.cause), Some(String::new()));
    // With no `relaunched_at` either, a move in flight takes its phase's stamp
    // as the move's instant; a Done record keeps none, and a written `0` reads
    // as written.
    assert_eq!(St::from_json(old).map(|s| s.relaunched_at), Some(7));
    let done = r#"{"phase":"done","at":0}"#;
    assert_eq!(St::from_json(done).map(|s| s.relaunched_at), Some(0));
    let written = r#"{"phase":"relaunched","at":7,"relaunched_at":0}"#;
    assert_eq!(St::from_json(written).map(|s| s.relaunched_at), Some(0));
}

/// The record's agent word: none (an older record) or Claude's own name is Claude Code's; a
/// word this build has no lane for, or a value that is no word, is a newer build's record and
/// no record here. Negative control: Codex's word reads as Codex's.
#[test]
fn a_record_names_its_agent_or_is_no_record() {
    let agent =
        |word: &str| St::from_json(&format!(r#"{{"phase":"pending"{word}}}"#)).map(|st| st.agent);
    assert_eq!(agent(""), Some(upgrade::Agent::Claude));
    assert_eq!(agent(r#","agent":"""#), Some(upgrade::Agent::Claude));
    assert_eq!(agent(r#","agent":"claude""#), Some(upgrade::Agent::Claude));
    assert_eq!(agent(r#","agent":"codex""#), Some(upgrade::Agent::Codex));
    assert_eq!(agent(r#","agent":"gemini""#), None);
    assert_eq!(agent(r#","agent":"Codex""#), None);
    assert_eq!(agent(r#","agent":7"#), None);
}

/// One reader for both writers: a state written without the view fields (no
/// `prompt`, `pending_since`, `wait`, `request`, `request_tab`, `outcome`,
/// `done_at`) reads with those at their defaults (the age falls back to
/// `salt`, no request word) and keeps every field it did write. Negative
/// control: the same text re-written by this build round-trips.
#[test]
fn a_state_main_wrote_reads_with_the_owners_fields_at_their_defaults() {
    let main_wrote = r#"{"phase":"announced","why":"","from":"2.1.281","to":"2.1.282","source":"managed","marker":"ATERM-UPGRADE-READY-0badf00d","notice_start":"Mon Sep 21 12:00:00 2026","tab":"s-b5cf2faabac5ce5127bd","line":"","noted":"","model_before":"claude-opus-5","launch_model":"","model_list":"claude-opus-5-5","resumed_on":"","at":1790000060,"asks":1,"salt":1790000000,"last_seq":12,"seq_since":1790000000,"pid":9162,"notice_pid":9162,"shell":9001,"mark":0,"confirm_by":0,"resumed_pid":0}"#;
    let st = St::from_json(main_wrote).expect("main's state reads");
    assert_eq!(
        st.phase,
        Phase::Announced {
            at_s: 1_790_000_060,
            asks: 1,
        }
    );
    assert_eq!(st.model_list, "claude-opus-5-5");
    assert_eq!(st.model_before, "claude-opus-5");
    assert_eq!((st.from.as_str(), st.to.as_str()), ("2.1.281", "2.1.282"));
    assert_eq!(st.notice_pid, 9162);
    assert_eq!(st.prompt, None);
    assert_eq!(st.line_block, None);
    assert_eq!(st.request, Request::None);
    assert!(st.request_tab.is_empty() && st.wait.is_empty());
    assert_eq!(
        st.behind_since(),
        1_790_000_000,
        "the age falls back to salt"
    );
    assert_eq!(St::from_json(&st.to_json()), Some(st));
}

#[test]
fn background_work_is_the_shells_under_the_agent_at_any_depth() {
    let t: Vec<(u32, u32, String)> = [
        (100, 1, "claude"),
        (200, 100, "zsh"),        // a Bash tool
        (201, 200, "targo"),      // what it runs: its shell already counts
        (300, 100, "node"),       // an MCP server: a resume restarts it
        (301, 300, "bash"),       // ...but a shell under it is still work
        (400, 1, "zsh"),          // another tab
        (500, 200, "caffeinate"), // one a Bash tool started: work
    ]
    .iter()
    .map(|(p, pp, n)| (*p, *pp, (*n).to_string()))
    .collect();
    let mut found = background(100, &t);
    found.sort();
    assert_eq!(found, vec!["bash", "caffeinate", "zsh"]);
    assert!(background(400, &t).is_empty());
}

/// `(pid, ppid, comm)` rows as [`table`] reads them, basenames already taken.
fn ps_rows(rows: &[(u32, u32, &str)]) -> Vec<(u32, u32, String)> {
    rows.iter()
        .map(|(p, pp, n)| (*p, *pp, (*n).to_string()))
        .collect()
}

/// Claude Code's own keep-awake is not work: it starts `caffeinate -i -t 300` as its own child
/// every turn and stops it some 30 s after, so counting it would hold every restart past the
/// READY answer. Negative controls, each holding the restart: a `caffeinate` under a Bash
/// tool's shell; a Bash tool's python beside the keep-awake (only the keep-awake is let go); a
/// shell the keep-awake runs; a caffeinate an agent's helper started.
#[test]
fn the_agents_own_keep_awake_does_not_hold_the_restart() {
    let idle = Facts {
        status: "idle".to_string(),
        status_age_s: 60,
        composer_empty: true,
        quiet_s: 60,
        ..Facts::default()
    };
    let asked = Phase::Announced { at_s: 100, asks: 1 };
    let restart = |t: &[(u32, u32, String)]| {
        let mut found = background(4163, t);
        found.sort();
        let f = Facts {
            background: found.clone(),
            ..idle.clone()
        };
        (
            found,
            upgrade::gate_restart(&f, true),
            upgrade::next_step(&asked, &f, true, 200),
        )
    };
    // The agent under the tab's shell, as every row below has it.
    let agent = [(4100, 1, "zsh"), (4163, 4100, "2.1.281")];
    let with = |rows: &[(u32, u32, &str)]| ps_rows(&[&agent[..], rows].concat());

    // At READY: Claude's own keep-awake alone.
    let (found, gate, step) = restart(&with(&[(8931, 4163, "caffeinate")]));
    assert_eq!(
        (gate, step),
        (upgrade::Gate::Go, Step::Terminate),
        "the agent's own keep-awake held the restart as work: {found:?}"
    );
    assert!(found.is_empty(), "{found:?}");

    // A caffeinate a Bash tool started is the tool's work.
    let (found, gate, step) = restart(&with(&[(4492, 4163, "zsh"), (4494, 4492, "caffeinate")]));
    assert_eq!(found, vec!["caffeinate", "zsh"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));
    assert_eq!(step, Step::Wait("background"));

    // The python a Bash tool left running, beside Claude's own keep-awake:
    // the tool's shell still holds the restart.
    let (found, gate, step) = restart(&with(&[
        (4444, 4163, "caffeinate"),
        (4492, 4163, "zsh"),
        (4494, 4492, "Python"),
    ]));
    assert_eq!(found, vec!["zsh"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));
    assert_eq!(step, Step::Wait("background"));

    // A shell the keep-awake itself runs is walked to and counted.
    let (found, gate, _) = restart(&with(&[(4444, 4163, "caffeinate"), (4445, 4444, "sh")]));
    assert_eq!(found, vec!["sh"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));

    // A caffeinate one of the agent's helpers started is not the agent's own.
    let (found, gate, _) = restart(&with(&[(4300, 4163, "node"), (4301, 4300, "caffeinate")]));
    assert_eq!(found, vec!["caffeinate"]);
    assert_eq!(gate, upgrade::Gate::Wait("background"));
}

/// The notice names what runs under the agent by its `eval` payload, without the Bash tool's
/// boilerplate; its pids come from the gate's walk.
#[test]
fn the_notice_names_what_runs_under_the_agent_by_its_command() {
    let measured = "/bin/zsh -c source /Users//u/.claude/shell-snapshots/snapshot-zsh-1790024225604-061jsy.sh \
        2>/dev/null || true && setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL 2>/dev/null || true && { \
        \\builtin unalias -- 'unsetenv'; \\builtin unset -f -- 'unsetenv'; } >/dev/null 2>&1 || true \
        && eval 'D=/tmp/wf; until [ \"$(python3 -c \"\\012import json\\012print(sum(1 for l in \
        open('\"'\"'$D/journal.jsonl'\"'\"')))\")\" -ge 6 ]; do sleep 15; done; echo \"CLIPPY_PHASE_DONE\"' \
        < /dev/null && pwd -P >| /tmp/claude-f730-cwd";
    let head = command_head(measured);
    assert!(head.starts_with("D=/tmp/wf; until ["), "{head}");
    assert!(head.contains("-ge 6 ]; do sleep 15; done"), "{head}");
    assert!(
        head.contains("open('$D/journal.jsonl')"),
        "the quoting undone: {head}"
    );
    assert!(
        !head.contains("shell-snapshots") && !head.contains("pwd -P"),
        "{head}"
    );
    assert!(!head.contains("\\012"), "{head}");
    // A command line that is not the Bash tool's is its own head.
    assert_eq!(
        command_head("/opt/bin/watcher --poll 5"),
        "/opt/bin/watcher --poll 5"
    );

    for (etime, secs) in [
        ("00:07", 7),
        ("12:34", 754),
        ("04:10:51", 15_051),
        ("05-04:10:51", 5 * 86_400 + 15_051),
    ] {
        assert_eq!(parse_etime(etime), Some(secs), "{etime}");
    }
    assert_eq!(parse_etime("soon"), None);
    assert_eq!(parse_etime("1:2:3:4"), None);

    // The give-up's ledger row quotes the same heads, keeping the `grep -m1`
    // (what makes the `tail -f` endless) past the long path ahead of it.
    let closer = "/bin/zsh -c source /Users//u/.claude/shell-snapshots/snapshot-zsh-1790024225604-061jsy.sh \
        2>/dev/null || true && eval 'D=/Users//person/.claude/projects/-Users-person/\
        00000000-0000-4000-8000-000000000001/subagents/workflows/wf_examplerun-1; tail -f -n +1 \
        \"$D/journal.jsonl\" | /usr/bin/grep -m1 -F '\"'\"'\"agentId\":\"a0000000000000001\",\
        \"result\"'\"'\"' > /dev/null; echo \"closer finished\"' < /dev/null && pwd -P >| \
        /tmp/claude-f730-cwd";
    let row = gave_up_words(
        upgrade::Agent::Claude,
        &[upgrade::Held {
            pid: 41234,
            name: "zsh".to_string(),
            age_s: 475_200,
            command: command_head(closer),
        }],
        false,
    );
    assert!(
        row.contains("pid 41234 (zsh, 5d12h): D=/Users/…/wf_examplerun-1;")
            && row
                .contains("| /usr/bin/grep -m1 -F '\"agentId\":\"a0000000000000001\",\"result\"'")
            && row.ends_with("echo \"closer finished\""),
        "{row}"
    );
    // The row says the release is owed, not typed (the `released:` row says
    // that); the Codex lane types none and promises none.
    let claude = gave_up_words(upgrade::Agent::Claude, &[], false);
    assert!(
        claude.contains("owes the agent the line that releases it")
            && !claude.contains("releases the agent,"),
        "{claude}"
    );
    let codex = gave_up_words(upgrade::Agent::Codex, &[], false);
    assert!(!codex.contains("release"), "{codex}");
    // Both lanes' rounds rest, and say when the next one asks (no stop is
    // for good).
    let next = format!(
        "a new round asks again in {}",
        upgrade::span(upgrade::RETRY_S)
    );
    assert!(
        claude.contains(&next) && codex.contains(&next),
        "{claude} / {codex}"
    );

    // The walk the gate reads, with pids: the agent's own keep-awake is not
    // named, the loops and whatever they run under them are.
    let t = vec![
        (74315, 74252, "claude".to_string()),
        (82096, 74315, "caffeinate".to_string()),
        (63492, 74315, "zsh".to_string()),
        (68290, 63492, "sleep".to_string()),
        (64036, 74315, "zsh".to_string()),
    ];
    let mut procs = background_procs(74315, &t);
    procs.sort_unstable();
    assert_eq!(
        procs,
        vec![(63492, "zsh".to_string()), (64036, "zsh".to_string())]
    );
    assert_eq!(background(74315, &t).len(), 2);
}

/// The restart's last look fails closed: an empty process table (`ps` gave none) reads as work,
/// never as "nothing runs".
#[test]
fn an_unreadable_process_table_is_never_read_as_nothing_running() {
    assert_eq!(
        background_or_unreadable(4163, &[]),
        vec![UNREADABLE_TABLE.to_string()]
    );
    assert!(background_or_unreadable(4163, &[(1, 0, "launchd".to_string())]).is_empty());
    // The live table is never empty: it holds this very process.
    assert!(!table().is_empty());
    assert!(!live_background(std::process::id()).contains(&UNREADABLE_TABLE.to_string()));
}

#[test]
fn a_sweep_never_signals_its_own_ancestry() {
    let t = table();
    assert!(is_our_ancestor(std::process::id(), &t));
    if let Some((parent, _, _)) = ids(std::process::id()) {
        assert!(is_our_ancestor(parent, &t));
    }
    let synthetic = vec![(10, 1, "a".to_string()), (11, 10, "b".to_string())];
    assert!(!is_our_ancestor(11, &synthetic));
}

#[test]
fn host_filters_foreign_groups_before_expensive_process_reads() {
    let file = |pid: u32, kind: &str| SessionFile {
        pid,
        session_id: SESSION.to_string(),
        cwd: "/tmp".to_string(),
        version: "1.0.0".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 0,
        proc_start: "start".to_string(),
        kind: kind.to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    };
    let files = [
        file(1, "interactive"),
        file(2, "interactive"),
        file(3, "print"),
    ];
    let ours = [LiveTab {
        sid: "s-mine".to_string(),
        fgpgid: Some(101),
    }];
    let mut args_read = Vec::new();
    let mut groups_read = Vec::new();
    let found = host_candidates(
        &files,
        &ours,
        |_, _| true,
        |pid| {
            args_read.push(pid);
            Some(atpkg::caller_shell::ProcArgs::default()) // macOS omits env.
        },
        |pid| {
            groups_read.push(pid);
            Some(if pid == 1 { 101 } else { 202 })
        },
        |_| Some((10, 101, 101)),
    );
    assert_eq!(found.len(), 1);
    let (sf, _, claim) = found[0].as_ref().expect("claimed");
    assert_eq!(sf.pid, 1);
    assert_eq!(claim.tab, "s-mine");
    assert_eq!(args_read, [1]);
    assert_eq!(groups_read, [1, 2]);

    // The tab's foreground moved between the roster and the ids: nothing
    // owns the tab, and nothing was read to decide on — the step's wait.
    let found = host_candidates(
        &files[..1],
        &ours,
        |_, _| true,
        |_| Some(atpkg::caller_shell::ProcArgs::default()),
        |_| Some(101),
        |_| Some((10, 101, 202)),
    );
    let [Err(wait)] = found.as_slice() else {
        panic!("a background job does not own the tab: {found:?}");
    };
    assert_eq!(
        (wait.pid, wait.tab.as_str(), wait.step.as_str()),
        (1, "s-mine", "wait:no-process")
    );
    let found = host_candidates(
        &files[..1],
        &ours,
        |_, _| true,
        |_| {
            Some(atpkg::caller_shell::ProcArgs {
                env: vec!["ATERM_PARENT_SESSION_ID=s-other".to_string()],
                ..atpkg::caller_shell::ProcArgs::default()
            })
        },
        |_| Some(101),
        |_| Some((10, 101, 101)),
    );
    assert!(
        found.is_empty(),
        "a readable conflicting env vetoes the claim"
    );
}

/// A current Claude costs the host tick no `ps` or argv read; a newer build or a restart in
/// flight still reaches the ownership proofs.
#[test]
fn current_claude_skips_process_inspection_but_new_and_in_flight_do_not() {
    let mut file = SessionFile {
        pid: 101,
        session_id: SESSION.to_string(),
        cwd: "/tmp".to_string(),
        version: "2.1.281".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 0,
        proc_start: "start".to_string(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    };
    let target = |version: &str, source| Candidate {
        exe: PathBuf::from("/x/claude"),
        version: Version::parse(version).expect("version"),
        source,
    };
    let mut targets = Targets {
        managed: Some(target("2.1.281", Source::Managed)),
        native: Some(target("2.1.280", Source::Native)),
        unanswered: false,
    };
    let ours = [LiveTab {
        sid: "s-mine".to_string(),
        fgpgid: Some(101),
    }];
    assert!(!needs_upgrade_inspection(&file, &targets, None));
    let current = host_candidates(
        std::slice::from_ref(&file),
        &ours,
        |sf, _| needs_upgrade_inspection(sf, &targets, None),
        |_| panic!("current build must not read argv"),
        |_| Some(101), // one cheap group lookup identifies this tab
        |_| panic!("current build must not run ps for process ids"),
    );
    assert!(current.is_empty());

    targets.native = Some(target("2.1.282", Source::Native));
    assert!(
        needs_upgrade_inspection(&file, &targets, None),
        "native may be eligible until the executable is checked"
    );
    let newer = host_candidates(
        std::slice::from_ref(&file),
        &ours,
        |sf, _| needs_upgrade_inspection(sf, &targets, None),
        |_| Some(atpkg::caller_shell::ProcArgs::default()),
        |_| Some(101),
        |_| Some((10, 101, 101)),
    );
    assert!(
        matches!(newer.as_slice(), [Ok(_)]),
        "new build keeps the ownership path"
    );

    targets.native = None;
    let restarting = St {
        phase: Phase::Exiting { at_s: 1 },
        ..St::default()
    };
    assert!(needs_upgrade_inspection(&file, &targets, Some(&restarting)));
    file.version = "unknown".to_string();
    assert!(needs_upgrade_inspection(&file, &targets, None));
}

/// A session file its dead owner left behind holds nothing: the restart it
/// names is still the orphan pass's to carry on (`visit` acts on live processes
/// only, `wait:stale-file`). Control: the same file with a live owner is
/// `visit`'s, and skipped here.
#[test]
fn a_dead_owners_leftover_session_file_does_not_strand_the_restart() {
    let dir = scratch("leftover");
    let opts = Opts {
        sock: None,
        dry_run: true,
        ..drive(&dir)
    };
    let dead = dead_pid();
    let session = "03396a15-856e-4f1b-8174-ae9a3e4b36a0";
    let st = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: dead,
        shell: std::process::id(),
        tab: "s-b5cf2faabac5ce5127bd".to_string(),
        to: "2.1.281".to_string(),
        source: "native".to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, session), st.to_json()).expect("write");
    let file = |pid: u32, start: &str| {
        upgrade::parse_session_file(&format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"2.1.280","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ))
        .expect("a session file")
    };
    let steps = |files: &[SessionFile]| {
        orphans(&opts, files, None)
            .into_iter()
            .map(|r| r.step)
            .collect::<Vec<_>>()
    };
    assert_eq!(steps(&[file(dead, "gone")]), ["would-resume:exiting"]);
    // Control: this test's own live process holding the conversation.
    let me = std::process::id();
    let start = kernel_start(me).expect("our own start");
    assert!(steps(&[file(me, &start)]).is_empty(), "`visit` has it");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_restart_left_between_exit_and_relaunch_is_resumed_by_the_next_sweep() {
    let dir = scratch("orphan");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    // A pid that was a process and is not one now.
    let dead = dead_pid();
    let opts = Opts {
        sock: None,
        dry_run: true,
        ..drive(&dir)
    };
    let session = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
    // Signalled a moment ago, and the shell it ran under still there.
    let at_s = now_s();
    let st = St {
        phase: Phase::Exiting { at_s },
        pid: dead,
        shell: std::process::id(),
        tab: "s-b5cf2faabac5ce5127bd".to_string(),
        to: "2.1.281".to_string(),
        source: "native".to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, session), st.to_json()).expect("write");
    let got = sweep(&opts);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].session, session);
    assert_eq!(got[0].step, "would-resume:exiting");
    // A dry run wrote nothing: the phase is where it was.
    assert_eq!(
        load(&opts, session).map(|s| s.phase),
        Some(Phase::Exiting { at_s })
    );
    // Another tab's restart is not this sweep's when it is told one tab.
    let only = Opts {
        only_sid: Some("s-0".to_string()),
        ..opts.clone()
    };
    assert!(sweep(&only).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sweep leaves out the lane of an agent whose own switch is off ([`Opts::agents_off`],
/// [`lanes`]): that lane's pass never runs, and the other agent's runs as usual. Negative
/// control: with nothing off both lanes run, the Codex lane first and the Claude lane's
/// reports before its.
#[test]
fn a_sweep_leaves_out_the_lane_of_an_agent_whose_switch_is_off() {
    use aterm_phase::Program;
    use upgrade::Agent::{Claude, Codex};
    let dir = scratch("lanes-off");
    let base = drive(&dir);
    let ran = std::cell::RefCell::new(Vec::new());
    let pass = |agent: upgrade::Agent| {
        ran.borrow_mut().push(agent);
        vec![Report {
            pid: 0,
            tab: "-".to_string(),
            session: agent.product().to_string(),
            from: "-".to_string(),
            to: "-".to_string(),
            step: "current".to_string(),
        }]
    };
    let swept = |agents_off: Vec<Program>| {
        ran.borrow_mut().clear();
        let opts = Opts {
            agents_off,
            ..base.clone()
        };
        let reports = lanes(&opts, None, &pass);
        let said: Vec<String> = reports.into_iter().map(|r| r.session).collect();
        (ran.borrow().clone(), said)
    };
    assert_eq!(
        swept(Vec::new()),
        (
            vec![Codex, Claude],
            vec!["Claude Code".into(), "Codex".into()]
        )
    );
    assert_eq!(
        swept(vec![Program::Claude]),
        (vec![Codex], vec!["Codex".into()])
    );
    assert_eq!(
        swept(vec![Program::Codex]),
        (vec![Claude], vec!["Claude Code".into()])
    );
    assert_eq!(
        swept(vec![Program::Claude, Program::Codex]),
        (Vec::new(), Vec::new())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("upgrade", name)
}

/// The home's model caches are not read while two or more accounts are listed
/// ([`Opts::several_logins`]): the home's one profile, catalog, access cache and announcements
/// are whichever login started last, so the evidence is unknown. Negative control: with one
/// login the same files are read.
#[test]
fn the_homes_model_caches_are_not_read_under_several_logins() {
    let dir = scratch("model-evidence-logins");
    let home = dir.join("home");
    let catalog = home.join(".claude/cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(
        catalog.join("0000-cc.json"),
        r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"}]}}}"#,
    )
    .expect("served");
    std::fs::write(
        home.join(".claude.json"),
        r#"{"modelAccessCache":[{"apiName":"claude-opus-5","entitled":false}],
            "additionalModelOptionsCache":[{"value":"claude-fable-5[1m]","disabled":true}],
            "cachedGrowthBookFeatures":{"tengu_startup_announcements":[{"id":"opus-5-5-launch","text":"t","requiresModel":"claude-opus-5-5"}]}}"#,
    )
    .expect("the profile");
    // Reads only: no list is written.
    let one = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    seed_builds(&one);
    assert!(
        !one.several_logins,
        "control: the packaged state is one login"
    );
    let read = model_evidence(&one, &Records::of(&one.home, None));
    assert_eq!(read.denied, ["claude-opus-5"]);
    assert_eq!(read.disabled_options, ["claude-fable-5"]);
    assert_eq!(read.served.as_ref().map(Vec::len), Some(1));
    assert_eq!(read.announcements.len(), 1);
    assert!(
        !models::availability("claude-opus-5", "9.9.9", &read, None).ok(),
        "control: the cache's denial is read"
    );

    let several = Opts {
        several_logins: true,
        instances: Instances::Listed(Vec::new()),
        ..one.clone()
    };
    let unknown = model_evidence(&several, &Records::of(&several.home, None));
    assert_eq!(
        unknown,
        models::Evidence::default(),
        "nothing of the home's"
    );
    // [`models_report`] judges by the same evidence (what it says of the
    // installed build's catalog is not asserted): one login cites the denial and
    // the served catalog, several cite neither.
    let served = models_report(&one);
    assert!(
        served.contains("served catalog") && served.contains("modelAccessCache entitled:false"),
        "{served}"
    );
    let unread = models_report(&several);
    assert!(
        !unread.contains("served catalog") && !unread.contains("modelAccessCache"),
        "{unread}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_second_sweeper_does_nothing_while_the_first_holds_the_lock() {
    let dir = scratch("lock");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    let opts = Opts {
        sock: None,
        ..drive(&dir)
    };
    let first = sweep_lock(&opts).expect("free").expect("a real run locks");
    let got = sweep(&opts);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].step, "busy:another-sweep");
    assert!(!got[0].is_act());
    // Released by `LOCK_UN` (the guard's drop), so the very next sweep runs
    // (see `a_dropped_sweep_lock_is_free_while_a_copy_of_its_descriptor_lives`).
    drop(first);
    let after = sweep(&opts);
    assert!(
        after.is_empty(),
        "no sessions, nothing to report: {after:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_that_cannot_be_carried_is_refused_before_the_agent_is_asked_to_wind_down() {
    let dir = scratch("preflight");
    let opts = Opts {
        sock: None,
        ..drive(&dir)
    };
    let session = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
    let exe = Path::new("/Users/_me/Library/Application Support/aterm/pkg/agents/claude");
    let hook = Path::new("/Users/_me/.aterm/shell.d/00-atpkg.zsh");
    let launch = |extra: &[&str]| -> Vec<String> {
        ["claude", "--dangerously-skip-permissions"]
            .iter()
            .chain(extra)
            .map(|w| (*w).to_string())
            .collect()
    };
    // The plan `visit` hands the announcement, from the same builder `restart`
    // uses (`plan` reads the shell, the heal and the directory; this is the
    // rest of it).
    let planned = |argv: &[String]| {
        line_for(
            Dialect::Zsh,
            Some(hook),
            None,
            exe,
            argv,
            Some(session),
            None,
            None,
            &[],
        )
        .map(|line| Plan {
            shell: 1,
            dialect: Dialect::Zsh,
            line,
        })
    };
    let report = || Report {
        pid: 9162,
        tab: "s-b5cf2faabac5ce5127bd".to_string(),
        session: session.to_string(),
        from: "2.1.280".to_string(),
        to: "2.1.281(native)".to_string(),
        step: String::new(),
    };
    let mut typed: Vec<u32> = Vec::new();
    let mut send = |asks: u32| -> Result<String, String> {
        typed.push(asks);
        Ok("ATERM-UPGRADE-READY-0badf00d".to_string())
    };

    // A launch whose relaunch line the tty could cut (a long inline system
    // prompt) is refused at the announcement, the notice never typed, not
    // after the agent has wound down and answered READY.
    let prompt = "p".repeat(upgrade::MAX_LINE);
    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--append-system-prompt", &prompt])),
        100,
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "refused:line");
    assert!(got.is_act(), "a refusal is reported, not a wait");
    assert!(
        matches!(&st.phase, Phase::Failed(w) if w.starts_with("line:")),
        "{:?}",
        st.phase
    );
    // ...and for good: a failed upgrade is never announced to this target.
    assert_eq!(
        upgrade::next_step(&st.phase, &Facts::default(), true, 200),
        Step::Wait("failed")
    );
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert!(
        ledger.contains(r#""step":"refused:line""#) && ledger.contains("the line would be"),
        "{ledger}"
    );

    // A flag the rewrite cannot carry: the same, before the notice.
    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--brand-new-flag"])),
        100,
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "refused:unknown-flag --brand-new-flag");
    assert_eq!(
        st.phase,
        Phase::Failed("argv:unknown-flag --brand-new-flag".to_string())
    );

    // A fact this sweep could not read: a wait, nothing typed, nothing failed.
    let mut st = St::default();
    let got = announce(
        &opts,
        report(),
        &mut st,
        Err(NoPlan::Wait("shell-dialect")),
        100,
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "wait:shell-dialect");
    assert_eq!(st.phase, Phase::Pending);

    // A dry run says what it would refuse, and records nothing.
    let dry = Opts {
        dry_run: true,
        state: dir.join("dry-state"),
        ..opts.clone()
    };
    let got = announce(
        &dry,
        report(),
        &mut st,
        planned(&launch(&["--append-system-prompt", &prompt])),
        100,
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "would-refuse:line");
    assert!(!got.is_act());
    assert_eq!(st.phase, Phase::Pending);
    assert!(!state_dir(&dry).exists());

    // Negative control: an everyday launch is announced, once, through the same
    // path — so the refusals above are the plan's, not a notice that never types.
    let got = announce(
        &opts,
        report(),
        &mut st,
        planned(&launch(&["--model", "opus"])),
        100,
        1,
        None,
        &mut send,
    );
    assert_eq!(got.step, "announced:1");
    assert_eq!(st.phase, Phase::Announced { at_s: 100, asks: 1 });
    assert_eq!(st.marker, "ATERM-UPGRADE-READY-0badf00d");
    assert_eq!(
        typed,
        vec![1],
        "the notice was typed only for the everyday launch"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The drain's bound over the real state: an answered announcement a box nobody answers has
/// held past [`upgrade::DRAIN_S`] is voided. The READY answer no longer counts, the ledger says
/// why, the phase keeps its asks, and the next idle point asks again. A dry run changes nothing.
#[test]
fn a_drain_a_person_holds_past_its_bound_voids_the_answer_and_asks_again() {
    let dir = scratch("drain-bound");
    let opts = drive(&dir);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let tail = format!(
        r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
    );
    let mut st = St {
        phase: Phase::Announced { at_s: 100, asks: 1 },
        marker: marker.to_string(),
        ..St::default()
    };
    let report = || Report {
        pid: 9162,
        tab: "s-b5cf2faabac5ce5127bd".to_string(),
        session: "03396a15-856e-4f1b-8174-ae9a3e4b369f".to_string(),
        from: "2.1.280".to_string(),
        to: "2.1.281(native)".to_string(),
        step: String::new(),
    };
    let idle = Facts {
        status: "idle".to_string(),
        status_age_s: 60,
        composer_empty: true,
        quiet_s: 60,
        ..Facts::default()
    };
    let boxed = Facts {
        approval_box: true,
        hold_s: upgrade::HOLD_S,
        ..idle.clone()
    };
    let expiry = 100 + upgrade::DRAIN_S;

    assert!(
        answered(&st, Some(&tail)),
        "the answer counts before the bound"
    );
    let step = upgrade::next_step(&st.phase, &boxed, answered(&st, Some(&tail)), expiry);
    assert_eq!(step, Step::Void("box"));

    let dry = Opts {
        dry_run: true,
        state: dir.join("dry-state"),
        ..opts.clone()
    };
    let got = drain_expired(&dry, report(), &mut st, "box", &boxed, expiry);
    assert_eq!(got.step, "would-void:box");
    assert!(!got.is_act());
    assert!(answered(&st, Some(&tail)), "a dry run forgets nothing");
    assert!(st.release.is_empty(), "and owes nothing");
    assert!(!state_dir(&dry).exists());

    let got = drain_expired(&opts, report(), &mut st, "box", &boxed, expiry);
    assert_eq!(got.step, "drain-expired:box");
    assert!(got.is_act(), "the host logs it");
    assert_eq!(after(&got.step, 0), After::NextIdle, "the release is owed");
    assert!(
        !answered(&st, Some(&tail)),
        "the voided answer can never end the agent later"
    );
    // The agent stopped for a restart that is not coming: it is owed its
    // release, and the re-ask clock starts over at the void.
    assert_eq!(st.release, "void");
    assert_eq!(
        st.phase,
        Phase::Announced {
            at_s: expiry,
            asks: 1
        }
    );
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert!(
        ledger.contains(r#""step":"drain-expired:box""#)
            && ledger.contains("box nobody answered had held the restart for 5 min")
            && ledger.contains("the agent is owed its release, and it is asked again later"),
        "{ledger}"
    );
    // A lane that types no release (Codex) says nothing of one.
    let codex_dir = scratch("drain-bound-codex");
    let codex_opts = drive(&codex_dir);
    let mut codex = St {
        phase: Phase::Announced { at_s: 100, asks: 1 },
        marker: marker.to_string(),
        agent: upgrade::Agent::Codex,
        ..St::default()
    };
    drain_expired(&codex_opts, report(), &mut codex, "box", &boxed, expiry);
    assert!(codex.release.is_empty());
    let said =
        std::fs::read_to_string(state_dir(&codex_opts).join("ledger.jsonl")).expect("ledger");
    assert!(
        said.contains(r#""step":"drain-expired:box""#) && !said.contains("release"),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(&codex_dir);
    // Once the box clears, the stale answer does not end the agent: the release
    // goes first ([`upgrade::gate_release`]) and the next notice waits `REASK_S`
    // after the void; while the box stands, both wait.
    let ready = answered(&st, Some(&tail));
    assert_eq!(
        upgrade::next_step(&st.phase, &idle, ready, expiry + 60),
        Step::Wait("awaiting-ready")
    );
    assert_eq!(upgrade::gate_release(&idle, ready), upgrade::Gate::Go);
    assert_eq!(
        upgrade::gate_release(&boxed, ready),
        upgrade::Gate::Wait("box")
    );
    assert_eq!(
        upgrade::next_step(&st.phase, &idle, ready, expiry + upgrade::REASK_S),
        Step::Announce
    );
    assert_eq!(
        upgrade::next_step(&st.phase, &boxed, ready, expiry + upgrade::REASK_S),
        Step::Wait("box")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sweep lock is free the moment its guard drops, whatever copies of its descriptor live on:
/// `copy` stands in for a child mid-spawn on another thread, which holds every descriptor until
/// its exec, so a lock released by close alone would read `busy:another-sweep`. Negative control
/// first: a held lock refuses.
#[test]
fn a_dropped_sweep_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = scratch("lock-window");
    let opts = Opts {
        sock: None,
        ..drive(&dir)
    };
    let holder = sweep_lock(&opts).expect("free").expect("a real run locks");
    assert_eq!(
        sweep_lock(&opts).err(),
        Some("another-sweep"),
        "a lock a sweep holds refuses the next"
    );
    let copy = holder.0.try_clone().expect("a copy of the descriptor");
    // Released by `LOCK_UN` (the guard's drop) while the copy is still open.
    drop(holder);
    assert!(
        matches!(sweep_lock(&opts), Ok(Some(_))),
        "a released sweep lock must be free while a copy of its descriptor lives"
    );
    drop(copy);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_what_a_sweep_did_is_an_act() {
    let r = |step: &str| Report {
        pid: 1,
        tab: "s-1".to_string(),
        session: "s".to_string(),
        from: "2.1.280".to_string(),
        to: "2.1.281(native)".to_string(),
        step: step.to_string(),
    };
    for quiet in [
        "current",
        "wait:not-idle",
        "skip:not-selected",
        "busy:another-sweep",
        "would-announce",
    ] {
        assert!(!r(quiet).is_act(), "{quiet}");
    }
    for act in [
        "announced:1",
        "done",
        "done:no-continue",
        "gave-up",
        "failed:no-resume",
        "refused:--print",
        "held-back:terminal:tmux",
    ] {
        assert!(r(act).is_act(), "{act}");
    }
    assert_eq!(
        r("done").line(),
        "upgrade pid=1 tab=s-1 session=s from=2.1.280 to=2.1.281(native) step=done"
    );
}

// ---------------------------------------------------------------- stand-ins

/// The tab every stand-in agent names: no instance has it, and every test here
/// points its sweep at a socket that does not exist, so nothing can be typed.
const TAB: &str = "s-feedfacefeedfacefeed";

/// A conversation id the stand-in agents hold.
const SESSION: &str = "0badf00d-1111-2222-3333-444455556666";

/// A person's prompt: what makes the conversation a transcript below stands
/// for one with a task (a record of the harness's own turns alone is
/// restarted afresh, never announced to, and is carried on with nothing).
const ASKED: &str = r#"{"type":"user","message":{"role":"user","content":"fix the bug"}}"#;

/// The stand-in agent's argv: this test binary, running only [`park_when_asked`].
const PARK: [&str; 2] = ["harness::upgrade_drive::tests::park_when_asked", "--exact"];

/// What tells [`park_when_asked`] to park.
const PARK_ENV: &str = "UPGRADE_DRIVE_TEST_PARK";

/// The stand-in agents' body: idle unless asked to park, then parked while the test process
/// that started it lives (its parent changes when that process ends), bounded only as a hang
/// detector.
#[test]
fn park_when_asked() {
    if std::env::var_os(PARK_ENV).is_none() {
        return;
    }
    let until = std::time::Instant::now() + Duration::from_secs(600);
    #[cfg(unix)]
    {
        // SAFETY: getppid takes nothing and cannot fail.
        let parent = unsafe { libc::getppid() };
        // SAFETY: as above.
        while unsafe { libc::getppid() } == parent && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    #[cfg(not(unix))]
    std::thread::sleep(until.saturating_duration_since(std::time::Instant::now()));
}

/// A stand-in agent in [`TAB`]: this test binary, parked. Not `/bin/sleep`: a
/// platform binary's environment is hidden from `KERN_PROCARGS2`, a real
/// agent's is not.
fn parked() -> Command {
    let mut c = parked_bare();
    c.arg(PARK[1]);
    c
}

/// [`parked`] without `--exact`, which no Claude Code launch carries: an argv
/// the relaunch plan can carry into a resume.
fn parked_bare() -> Command {
    let mut c = Command::new(std::env::current_exe().expect("exe"));
    c.arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        // A stand-in names no login of its own, whatever shell runs the
        // tests: a test that wants one adds it to what the visit reads.
        .env_remove(crate::harness::accounts::LOGIN_ENV)
        // Nor a HOME: an unset Claude home is its process's own `HOME`'s
        // (`Records::at`), and a stand-in's is the sweep's user home.
        .env_remove("HOME")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    c
}

/// Wait until `pid` runs this test binary (a fork not yet exec'd still reads
/// as its parent).
fn wait_exec(pid: u32) {
    let me = std::env::current_exe().expect("exe");
    for _ in 0..3_000 {
        if atpkg::caller_shell::process_args(pid)
            .is_some_and(|a| Path::new(&a.exec_path).file_name() == me.file_name())
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the stand-in agent {pid} never started");
}

/// Claude's session file for `pid`, with the kernel's start time.
fn register(home: &Path, pid: u32, session: &str) -> SessionFile {
    register_in(&home.join(".claude"), pid, session)
}

/// [`register`] in the Claude Code directory `claude_dir` itself — where a
/// Claude Code started with `CLAUDE_CONFIG_DIR=<claude_dir>` files it.
fn register_in(claude_dir: &Path, pid: u32, session: &str) -> SessionFile {
    register_as(claude_dir, pid, session, "1.0.0")
}

/// [`register_in`], the process running build `version` (`unknown`: one
/// nobody can compare, which the host looks at as it does a behind one).
fn register_as(claude_dir: &Path, pid: u32, session: &str, version: &str) -> SessionFile {
    let start = kernel_start(pid).expect("lstart");
    std::fs::create_dir_all(claude_dir.join("sessions")).expect("sessions");
    std::fs::write(
        claude_dir.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"{version}","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("session file");
    session_file_in(claude_dir, pid).expect("the file parses")
}

/// A sweep over several live conversations reads the session roster once, not once per
/// candidate.
#[test]
fn multiple_candidates_share_one_preliminary_session_scan() {
    let dir = scratch("one-precheck-scan");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    seed_builds(&opts);
    let mut a = parked().spawn().expect("agent A");
    let mut b = parked().spawn().expect("agent B");
    wait_exec(a.id());
    wait_exec(b.id());
    register(&opts.home, a.id(), SESSION);
    register(&opts.home, b.id(), "0badf00d-1111-2222-3333-444455556667");
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let reports = sweep(&opts);
    let scans = SESSION_FILE_SCANS.with(std::cell::Cell::get);
    assert_eq!(
        reports.len(),
        2,
        "both conversations were visited: {reports:?}"
    );
    assert!(
        reports
            .iter()
            .all(|r| matches!(r.step.as_str(), "current" | "would-refuse:not-a-shell-job")),
        "both candidates passed the preliminary owner check: {reports:?}"
    );
    assert_eq!(scans, 1, "only the initial complete roster read is needed");
    for child in [&mut a, &mut b] {
        child.kill().expect("stop agent");
        child.wait().expect("reap agent");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ready_from_tab_a_never_announces_or_terminates_tab_b() {
    let dir = scratch("notice-owner");
    let opts = drive(&dir);
    let mut a = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-a")
        .spawn()
        .expect("tab A");
    let mut b = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-b")
        .spawn()
        .expect("tab B");
    wait_exec(a.id());
    wait_exec(b.id());
    let sf_a = register(&opts.home, a.id(), SESSION);
    let sf_b = register(&opts.home, b.id(), SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{marker}"}}]}}}}"#
        ),
    )
    .expect("READY transcript");
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(
            &transcript(&opts.home, SESSION).expect("transcript"),
            TAIL_BYTES
        )
        .0,
        marker
    ));
    let st = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: marker.to_string(),
        tab: "s-a".to_string(),
        notice_pid: sf_a.pid,
        notice_start: squash(&sf_a.proc_start),
        ..St::default()
    };
    assert!(st.notice_belongs_to(&sf_a, "s-a"));
    let mut recycled = sf_a.clone();
    recycled.proc_start = "Thu Sep 24 00:00:00 2026".to_string();
    assert!(!st.notice_belongs_to(&recycled, "s-a"));
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
    let args_b = atpkg::caller_shell::ProcArgs {
        env: vec!["ATERM_PARENT_SESSION_ID=s-b".to_string()],
        ..atpkg::caller_shell::ProcArgs::default()
    };
    let files = session_files(&opts.home).expect("complete session scan");
    let model = aterm_spec::derive::harness_upgrade_notice_owner_model();
    let mut duplicate = model.init_state();
    assert!(model.fire("Announce", &mut duplicate));
    assert!(model.fire("Ready", &mut duplicate));
    assert!(model.fire("DuplicateOwner", &mut duplicate));
    assert_eq!(
        model.action_enabled("Terminate", &duplicate),
        unique_live_owner(&files, &sf_a) && st.notice_belongs_to(&sf_a, "s-a")
    );
    let report = visit_with_claim(
        &opts,
        &sf_b,
        Some(&files),
        &[],
        &newer(),
        &Live,
        Some(&args_b),
        None,
    );
    assert_eq!(report.step, "wait:conversation-owner-ambiguous");
    assert_eq!(load(&opts, SESSION), Some(st.clone()));

    a.kill().expect("stop A");
    a.wait().expect("reap A");
    let files = session_files(&opts.home).expect("complete session scan");
    assert!(unique_live_owner(&files, &sf_b));
    let mut foreign = model.init_state();
    assert!(model.fire("Announce", &mut foreign));
    assert!(model.fire("Ready", &mut foreign));
    assert!(model.fire("OtherTab", &mut foreign));
    assert_eq!(
        model.action_enabled("Terminate", &foreign),
        unique_live_owner(&files, &sf_b) && st.notice_belongs_to(&sf_b, "s-b"),
        "Tier-1: B cannot consume A's READY after A exits"
    );
    // A is gone: the upgrade reopens, and B, never asked and still behind,
    // is asked afresh (the whole re-ask is
    // `a_notice_whose_process_is_gone_is_asked_afresh_of_the_live_holder`).
    // A dry run says it would reopen, and changes nothing.
    assert!(model.fire("OwnerExits", &mut foreign));
    let report = visit_with_claim(
        &Opts {
            dry_run: true,
            ..opts.clone()
        },
        &sf_b,
        Some(&files),
        &[],
        &newer(),
        &Live,
        Some(&args_b),
        None,
    );
    assert_eq!(report.step, "would-reopen:notice-process-gone");
    assert_eq!(
        model.action_enabled("Reask", &foreign),
        report.step.ends_with(":notice-process-gone"),
        "Tier-1: the notice's process gone, the live holder is asked afresh"
    );
    assert!(!model.action_enabled("Terminate", &foreign));
    assert_eq!(load(&opts, SESSION), Some(st.clone()));
    assert!(alive(sf_b.pid), "B was not signalled");

    let no_env = atpkg::caller_shell::ProcArgs::default();
    let report = visit_with_claim(
        &opts,
        &sf_b,
        Some(&files),
        &[],
        &newer(),
        &Live,
        Some(&no_env),
        None,
    );
    assert_eq!(report.step, "wait:no-aterm-tab");
    assert_eq!(load(&opts, SESSION), Some(st));

    assert_eq!(
        continuation_session(&Records::of(&opts.home, None), sf_b.pid, SESSION),
        Some(sf_b.clone())
    );
    let other = "0badf00d-1111-2222-3333-444455556667";
    register(&opts.home, sf_b.pid, other);
    assert!(continuation_session(&Records::of(&opts.home, None), sf_b.pid, SESSION).is_none());
    b.kill().expect("stop B");
    b.wait().expect("reap B");
    let _ = std::fs::remove_dir_all(dir);
}

/// A notice whose process is gone is asked afresh of the live holder B (in A's tab, in another,
/// or under A's pid recycled). While A lives B waits; once A is gone a dry run changes nothing,
/// and one visit reopens the upgrade in a new round (fresh salt; A's markers, fence and owed
/// release dropped; said once) and asks B with a marker of its own. A's READY never restarts B.
/// Tier-1 of `harness_upgrade_notice_owner_model` (`Reask`, `Terminate`).
#[cfg(unix)]
#[test]
fn a_notice_whose_process_is_gone_is_asked_afresh_of_the_live_holder() {
    const OTHER: &str = "0badf00d-1111-2222-3333-444455556667";
    let model = aterm_spec::derive::harness_upgrade_notice_owner_model();
    let nine = Version::parse("9.9.9").expect("version");
    // (name, B's tab, the notice's pid recycled, the model's holder)
    for (name, b_tab, recycled, holder) in [
        ("gone-same-tab", "s-a", false, "Resume"),
        ("gone-other-tab", "s-b", false, "OtherTab"),
        ("gone-recycled", "s-a", true, "Recycled"),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance_of(&dir, b_tab);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let now = now_s();
        let salt = now - 7_200;
        let old = upgrade::ready_marker(SESSION, &nine, salt + 1);
        let project = opts.home.join(".claude/projects/p");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            format!(
                r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{old}"}}]}}}}"#
            ),
        )
        .expect("A's READY");
        // Without `--exact`, which no Claude Code launch carries: B is
        // announced to, never refused.
        let mut b = parked_bare()
            .env("ATERM_PARENT_SESSION_ID", b_tab)
            .spawn()
            .expect("B");
        wait_exec(b.id());
        let sf_b = register(&opts.home, b.id(), SESSION);
        // A, alive in `s-a` and holding another conversation now — or,
        // recycled, B's own pid under the start of the process it was.
        let mut a = (!recycled).then(|| {
            let a = parked()
                .env("ATERM_PARENT_SESSION_ID", "s-a")
                .spawn()
                .expect("A");
            wait_exec(a.id());
            a
        });
        let (notice_pid, notice_start) = match &a {
            Some(a) => (a.id(), register(&opts.home, a.id(), OTHER).proc_start),
            None => (b.id(), "Thu Sep 24 00:00:00 2026".to_string()),
        };
        let st = St {
            phase: Phase::Announced {
                at_s: now - 600,
                asks: 1,
            },
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            marker: old.clone(),
            markers: vec![old.clone()],
            asked: vec![old.clone()],
            release: "void".to_string(),
            salt,
            pending_since: salt,
            last_seq: 77,
            seq_since_s: now - 3_600,
            tab: "s-a".to_string(),
            notice_pid,
            notice_start: squash(&notice_start),
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
        let shell = dead_pid();
        let table = vec![(shell, 1, "zsh".to_string())];
        let args = atpkg::caller_shell::process_args(b.id()).expect("B's argv");
        let visit_as = |opts: &Opts| {
            let files = session_files(&opts.home).expect("complete session scan");
            visit_with_claim(
                opts,
                &sf_b,
                Some(&files),
                &table,
                &newer(),
                &Script::new(shell, usize::MAX, None),
                Some(&args),
                None,
            )
        };
        let ledger =
            || std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();

        // Negative control: the notified process lives, so B waits.
        if let Some(live) = a.as_mut() {
            let waits = visit_as(&opts);
            assert_eq!(waits.step, "wait:notice-owned-by-other-process", "{name}");
            assert_eq!(
                model.action_enabled("Reask", &run_model(&model, &["Announce", "Ready", holder])),
                waits.step.ends_with(":notice-process-gone"),
                "{name}: Tier-1, the notified process alive"
            );
            assert_eq!(load(&opts, SESSION), Some(st.clone()), "{name}");
            live.kill().expect("stop A");
            live.wait().expect("reap A");
        }
        let gone = run_model(&model, &["Announce", "Ready", "OwnerExits", holder]);

        // A dry run says it would reopen, and changes nothing.
        let dry = visit_as(&Opts {
            dry_run: true,
            ..opts.clone()
        });
        assert_eq!(dry.step, "would-reopen:notice-process-gone", "{name}");
        assert_eq!(load(&opts, SESSION), Some(st.clone()), "{name}");
        assert!(ledger().is_empty(), "{name}: a dry run ledgers nothing");

        // The visit: a new round, and B asked afresh in it.
        let report = visit_as(&opts);
        assert_eq!(report.step, "announced:1", "{name}");
        assert_eq!(
            model.action_enabled("Reask", &gone),
            ledger().contains(r#""step":"reopened:notice-process-gone""#),
            "{name}: Tier-1, the notified process gone"
        );
        let got = load(&opts, SESSION).expect("state");
        assert!(
            matches!(got.phase, Phase::Announced { asks: 1, .. }),
            "{name}: {:?}",
            got.phase
        );
        assert_ne!(got.marker, old, "{name}: a marker of B's own");
        assert_eq!(got.markers, vec![got.marker.clone()], "{name}");
        assert_eq!(got.asked, vec![got.marker.clone()], "{name}");
        assert!(
            got.salt > salt + u64::from(upgrade::MAX_ASKS),
            "{name}: a fresh salt, past every marker the old round could make"
        );
        // B's age is its own: behind since it began, not since A's round.
        assert!(
            got.pending_since > salt,
            "{name}: B's age, not A's round's: {} vs {salt}",
            got.pending_since
        );
        assert_eq!(
            (got.tab.as_str(), got.notice_pid, got.notice_start.as_str()),
            (b_tab, sf_b.pid, squash(&sf_b.proc_start).as_str()),
            "{name}: the notice is B's"
        );
        assert!(got.release.is_empty(), "{name}: nothing owed to A");
        let typed = asked.lock().expect("requests").clone();
        let notices: Vec<&String> = typed.iter().filter(|l| l.contains(" turn ")).collect();
        assert_eq!(notices.len(), 1, "{name}: {typed:?}");
        assert!(
            notices[0].contains(&got.marker) && !notices[0].contains(&old),
            "{name}: {}",
            notices[0]
        );
        let rows = ledger();
        let reopened: Vec<&str> = rows
            .lines()
            .filter(|l| l.contains(r#""step":"reopened:notice-process-gone""#))
            .collect();
        assert_eq!(reopened.len(), 1, "{name}: said once: {rows}");
        assert!(
            reopened[0].contains("(`void`)"),
            "{name}: the release dropped is said: {}",
            reopened[0]
        );

        // A's READY is no answer to B's round: B is never restarted on it.
        let tail = tail_to_end(
            &transcript(&opts.home, SESSION).expect("transcript"),
            TAIL_BYTES,
        )
        .0;
        assert!(
            upgrade::transcript_has_ready(&tail, &old),
            "{name}: still A's last word"
        );
        assert!(!heard(&got, &sf_b, b_tab, Some(&tail)), "{name}");
        let mut asked_afresh = gone.clone();
        assert!(model.fire("Reask", &mut asked_afresh));
        assert!(model.fire("Announce", &mut asked_afresh));
        let again = visit_as(&opts);
        assert_eq!(
            model.action_enabled("Terminate", &asked_afresh),
            again.step == "terminated",
            "{name}: Tier-1, A's READY signals nothing"
        );
        assert_eq!(again.step, "wait:awaiting-ready", "{name}");
        let typed = asked.lock().expect("requests").clone();
        // The notice typed for B was typed under the harness's own hand
        // (`type_fenced`): taken and given back, once — and nothing signalled.
        let leases = |verb: &str| {
            typed
                .iter()
                .filter(|l| l.contains(&format!(" lease {verb} ")))
                .count()
        };
        assert!(
            !typed.iter().any(|l| l.contains(" signal ")),
            "{name}: {typed:?}"
        );
        assert_eq!(
            (leases("acquire"), leases("release")),
            (1, 0),
            "{name}: {typed:?}"
        );
        assert_eq!(turns(&asked), 1, "{name}: nothing typed again");
        assert!(alive(b.id()), "{name}: B was not signalled");
        b.kill().expect("stop B");
        b.wait().expect("reap B");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A gone notice reopens the upgrade before the visit knows whether a move is still owed: here
/// A exited and the person resumed its conversation by hand on the current build, so B is
/// current and nothing is asked. The ledger says `reopened:` once and promises an ask only
/// while the conversation is still behind.
#[cfg(unix)]
#[test]
fn a_gone_notices_row_promises_no_ask_to_a_holder_already_current() {
    let dir = scratch("gone-notice-current");
    let (sock, asked) = instance_of(&dir, "s-a");
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let now = now_s();
    let nine = Version::parse("9.9.9").expect("version");
    let old = upgrade::ready_marker(SESSION, &nine, now - 7_199);
    let mut a = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-a")
        .spawn()
        .expect("A");
    wait_exec(a.id());
    let notice_start = kernel_start(a.id()).expect("A's start");
    a.kill().expect("stop A");
    a.wait().expect("reap A");
    let mut b = parked()
        .env("ATERM_PARENT_SESSION_ID", "s-a")
        .spawn()
        .expect("B");
    wait_exec(b.id());
    // B runs the build the upgrade was moving the conversation to.
    register(&opts.home, b.id(), SESSION);
    let file = opts.home.join(format!(".claude/sessions/{}.json", b.id()));
    let text = std::fs::read_to_string(&file).expect("session file");
    std::fs::write(
        &file,
        text.replace(r#""version":"1.0.0""#, r#""version":"9.9.9""#),
    )
    .expect("rewrite");
    let sf_b = session_file_of(&opts.home, b.id()).expect("the file parses");
    assert_eq!(sf_b.version, "9.9.9");
    let st = St {
        phase: Phase::Announced {
            at_s: now - 600,
            asks: 1,
        },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: old.clone(),
        markers: vec![old.clone()],
        asked: vec![old],
        release: "void".to_string(),
        salt: now - 7_200,
        tab: "s-a".to_string(),
        notice_pid: a.id(),
        notice_start: squash(&notice_start),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".to_string())];
    let args = atpkg::caller_shell::process_args(b.id()).expect("B's argv");
    let visit_b = || {
        let files = session_files(&opts.home).expect("complete session scan");
        visit_with_claim(
            &opts,
            &sf_b,
            Some(&files),
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        )
    };
    let ledger =
        || std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).unwrap_or_default();

    let report = visit_b();
    assert_eq!(report.step, "current", "B runs the build: nothing owed");
    assert_eq!(turns(&asked), 0, "nothing typed");
    // Reopened for the live holder, and — B runs the build — closed in the
    // same visit: nothing owed to A, and nothing left to age toward the next
    // build (`reached:by-hand`).
    assert!(load(&opts, SESSION).is_none(), "closed: B is on the build");
    let rows = ledger();
    assert_eq!(
        rows.lines()
            .filter(|l| l.contains(r#""step":"reached:by-hand""#))
            .count(),
        1,
        "said once: {rows}"
    );
    let reopened: Vec<&str> = rows
        .lines()
        .filter(|l| l.contains(r#""step":"reopened:notice-process-gone""#))
        .collect();
    assert_eq!(reopened.len(), 1, "said once: {rows}");
    assert!(
        reopened[0].contains("pending again in a new round")
            && reopened[0].contains("still behind")
            && !reopened[0].contains("it is asked afresh in a new round"),
        "no ask promised that the visit never makes: {}",
        reopened[0]
    );
    assert!(
        !rows.contains("reasked"),
        "no row says the holder was asked: {rows}"
    );
    // The next visit says nothing more, and asks nothing.
    assert_eq!(visit_b().step, "current");
    assert_eq!(ledger(), rows);
    assert_eq!(turns(&asked), 0);
    assert!(alive(b.id()), "B was not signalled");
    b.kill().expect("stop B");
    b.wait().expect("reap B");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The record a gone notice leaves ([`St::notice_gone`]): pending in a new round in the
/// holder's tab (salt past every old marker; markers, `asked` and fence forgotten; the behind
/// age kept), the gone process's owed release dropped, and the person-hold clocks restarted. A
/// `--now` is spent with the old round whatever tab it named; a `--skip` or `--defer` keeps its
/// fence.
#[test]
fn a_gone_notice_leaves_a_new_round_pending_in_the_holders_tab() {
    let mut announced = St {
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        tab: "s-a".to_string(),
        notice_pid: 4242,
        notice_start: "Fri Sep 25 16:00:00 2026".to_string(),
        salt: 1_000,
        pending_since: 900,
        noted: "attended".to_string(),
        hold_since_s: 700,
        hold_seen_s: 1_050,
        idle_looks: 1,
        held_by: vec![HeldBy {
            pid: 63_492,
            name: "zsh".to_string(),
            since: 900,
        }],
        ..St::default()
    };
    announced.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), 1_100, 1);
    announced.owe_release("void");
    assert_eq!(announced.release, "void", "owed to the notified agent");
    assert!(!announced.markers.is_empty() && !announced.asked.is_empty());
    for (request, request_tab, holder, kept) in [
        (Request::Now, "s-a", "s-a", false),
        (Request::Now, "s-a", "s-b", false),
        (Request::Now, "", "s-a", false),
        (Request::Skip("9.9.9".to_string()), "s-a", "s-b", true),
        (Request::DeferUntil(9_999), "s-a", "s-b", true),
    ] {
        let mut st = St {
            request: request.clone(),
            request_tab: request_tab.to_string(),
            request_at: 1_150,
            ..announced.clone()
        };
        let dropped = st.notice_gone(holder, 5_000);
        assert_eq!(dropped, "void", "{request:?}");
        assert_eq!(st.phase, Phase::Pending);
        assert!(st.release.is_empty(), "nothing owed to a gone process");
        assert!(st.marker.is_empty() && st.markers.is_empty() && st.asked.is_empty());
        assert!(st.salt > 1_000 + u64::from(upgrade::MAX_ASKS));
        assert_eq!(st.pending_since, 900, "behind since the first round");
        assert_eq!(
            (st.tab.as_str(), st.notice_pid, st.notice_start.as_str()),
            (holder, 0, ""),
            "{request:?}"
        );
        assert!(st.noted.is_empty(), "a hold is said again in the new round");
        assert_eq!(
            (st.hold_since_s, st.hold_seen_s),
            (0, 0),
            "{request:?}: the hold clocks start again"
        );
        // What the gone process's looks read is not the holder's: its idle
        // screen counts toward no lag, and what ran under it held nothing of
        // the new round.
        assert_eq!(st.idle_looks, 0, "{request:?}");
        assert!(st.held_by.is_empty(), "{request:?}");
        let (word, named, at) = (st.request.clone(), st.request_tab.as_str(), st.request_at);
        if kept {
            assert_eq!(
                (word, named, at),
                (request.clone(), request_tab, 1_150),
                "{request:?} named {request_tab:?}, holder in {holder}"
            );
        } else {
            assert_eq!(
                (word, named, at),
                (Request::None, "", 0),
                "{request:?} named {request_tab:?}, holder in {holder}"
            );
        }
    }
    // Nothing was owed: nothing is dropped.
    let mut asked = St {
        tab: "s-a".to_string(),
        ..St::default()
    };
    asked.announced("ATERM-UPGRADE-READY-0badf00d".to_string(), 10, 1);
    assert_eq!(asked.notice_gone("s-a", 20), "");
}

/// `actions` fired in turn from `m`'s initial state.
fn run_model(
    m: &aterm_spec::derive::Model,
    actions: &[&str],
) -> std::collections::BTreeMap<&'static str, i64> {
    let mut s = m.init_state();
    for action in actions {
        assert!(m.fire(action, &mut s), "{action} at {s:?}");
    }
    s
}

/// A sibling session file half-written or unreadable as the sweep reads it leaves the roster
/// partial, so even the valid owner's READY answer is not acted on.
#[test]
fn malformed_or_unreadable_sibling_vetoes_a_valid_owner() {
    let dir = scratch("incomplete-roster");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{marker}"}}]}}}}"#
        ),
    )
    .expect("READY transcript");
    let st = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: marker.to_string(),
        tab: TAB.to_string(),
        notice_pid: sf.pid,
        notice_start: squash(&sf.proc_start),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("state file");
    // A live sibling: only a running process can hold a conversation, so only
    // its unreadable record makes the roster partial.
    let mut sibling_agent = parked().spawn().expect("sibling");
    wait_exec(sibling_agent.id());
    let sibling_pid = sibling_agent.id();
    assert_ne!(sibling_pid, sf.pid);
    let sibling = opts
        .home
        .join(format!(".claude/sessions/{sibling_pid}.json"));
    let args = atpkg::caller_shell::ProcArgs {
        env: vec![format!("ATERM_PARENT_SESSION_ID={TAB}")],
        ..atpkg::caller_shell::ProcArgs::default()
    };
    for unreadable in [false, true] {
        if unreadable {
            std::fs::create_dir(&sibling).expect("unreadable JSON path");
        } else {
            std::fs::write(&sibling, "{").expect("half-written JSON");
        }
        assert!(session_files(&opts.home).is_none());
        assert_eq!(
            require_unique_owner(&opts.home, &sf),
            Err("session-files-unreadable")
        );
        let model = aterm_spec::derive::harness_upgrade_notice_owner_model();
        let mut partial = model.init_state();
        assert!(model.fire("Announce", &mut partial));
        assert!(model.fire("Ready", &mut partial));
        assert!(model.fire("IncompleteScan", &mut partial));
        assert_eq!(
            model.action_enabled("Terminate", &partial),
            require_unique_owner(&opts.home, &sf).is_ok() && st.notice_belongs_to(&sf, TAB),
            "Tier-1: incomplete scan cannot use a valid owner's READY"
        );
        let sweep = sweep(&opts);
        assert_eq!(sweep.len(), 1);
        assert_eq!(sweep[0].step, "wait:session-files-unreadable");
        let report = visit_with_claim(&opts, &sf, None, &[], &newer(), &Live, Some(&args), None);
        assert_eq!(report.step, "wait:session-files-unreadable");
        assert_eq!(load(&opts, SESSION), Some(st.clone()));
        assert!(alive(sf.pid), "the valid owner was never signalled");
        if unreadable {
            std::fs::remove_dir(&sibling).expect("remove directory");
        } else {
            std::fs::remove_file(&sibling).expect("remove partial JSON");
        }
    }
    std::fs::copy(
        opts.home.join(format!(".claude/sessions/{}.json", sf.pid)),
        &sibling,
    )
    .expect("valid JSON under the wrong PID filename");
    assert!(
        session_files(&opts.home).is_none(),
        "a numeric filename cannot attest another process's record"
    );
    std::fs::remove_file(&sibling).expect("remove mismatched record");
    assert!(session_files(&opts.home).is_some());
    sibling_agent.kill().expect("stop sibling");
    sibling_agent.wait().expect("reap sibling");
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// One stale sibling does not stop every Claude upgrade: a dead agent's
/// truncated record, an unreadable one, one in a shape this build does not
/// read, and a `*.json` whose name is no pid leave the roster whole, with the
/// live owner's record in it. A stale record whose `pid` names a live process
/// under another file name still vetoes (a numeric name cannot attest another
/// process's record).
#[test]
fn a_dead_or_foreign_sibling_leaves_the_roster_whole() {
    let dir = scratch("stale-sibling");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let sessions = opts.home.join(".claude/sessions");
    let gone = dead_pid();
    assert!(!alive(gone));
    let stale = sessions.join(format!("{gone}.json"));
    for shape in ["half", "dir", "schema", "foreign"] {
        match shape {
            "half" => std::fs::write(&stale, "{").expect("truncated"),
            "dir" => std::fs::create_dir(&stale).expect("unreadable"),
            "schema" => std::fs::write(
                &stale,
                format!(r#"{{"pid":{gone},"sessionId":"{SESSION}","cwd":"/"}}"#),
            )
            .expect("an older shape"),
            _ => std::fs::write(sessions.join("notes.json"), "{").expect("not a pid record"),
        }
        let roster = session_files(&opts.home)
            .unwrap_or_else(|| panic!("{shape}: a stale sibling vetoed the roster"));
        assert_eq!(roster, vec![sf.clone()], "{shape}");
        assert_eq!(require_unique_owner(&opts.home, &sf), Ok(()), "{shape}");
        match shape {
            "dir" => std::fs::remove_dir(&stale).expect("remove"),
            "foreign" => std::fs::remove_file(sessions.join("notes.json")).expect("remove"),
            _ => std::fs::remove_file(&stale).expect("remove"),
        }
    }
    // Negative control: a dead pid's file name over a live process's record.
    std::fs::copy(sessions.join(format!("{}.json", sf.pid)), &stale).expect("copy");
    assert!(
        session_files(&opts.home).is_none(),
        "a live pid still vetoes"
    );
    std::fs::remove_file(&stale).expect("remove");
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// A newer build to move to, so a sweep has something to do.
fn newer() -> Targets {
    Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("9.9.9").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    }
}

/// A real sweep's options over `dir`, dialling a socket that does not exist:
/// a sweep that got past every proof reports `wait:no-socket`.
fn drive(dir: &Path) -> Opts {
    Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: Some(dir.join("no.sock").to_string_lossy().into_owned()),
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
        // The ask-and-drain flow these tests pin (`upgrade_ask = true`) —
        // unless the test runs [`quietly`], under the default.
        ask: !QUIET.with(std::cell::Cell::get),
        several_logins: false,
        usage_tap: None,
        instances: Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
    }
}

/// Seeds `opts`'s state with the baked-catalog cache ([`catalog::baked_cached`], keyed by the
/// binary's path, size and mtime) for the machine's managed Claude Code build, scanned once per
/// process: a scan reads the whole build (230 MB, 0.7 s of CPU in a debug build), and every
/// test's fresh state would scan it anew. The test's read is then the product's second read of
/// the build, asserted equal to the scan. The state's listings read only `*.json`, never
/// `builds`. No managed build: nothing is seeded.
fn seed_builds(opts: &Opts) {
    type Key = (PathBuf, u64, Option<SystemTime>);
    type Files = Vec<(std::ffi::OsString, Vec<u8>)>;
    static SCANNED: std::sync::Mutex<Option<(Key, Files)>> = std::sync::Mutex::new(None);
    let Some(bin) = atpkg::store::resolve_configured()
        .and_then(|layout| std::fs::read_to_string(layout.agents_dir().join("claude")).ok())
        .and_then(|twin| catalog::twin_binary(&twin))
    else {
        return;
    };
    let Ok(meta) = std::fs::metadata(&bin) else {
        return;
    };
    let key = (bin.clone(), meta.len(), meta.modified().ok());
    let mut scanned = SCANNED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if scanned.as_ref().is_none_or(|(k, _)| *k != key) {
        let once = ScratchDir::new("upgrade", "builds");
        let first = catalog::baked_cached(&bin, &once);
        assert_eq!(
            catalog::baked_cached(&bin, &once),
            first,
            "the cache file reads back as the scan that wrote it"
        );
        let files = std::fs::read_dir(&*once)
            .expect("the builds cache")
            .flatten()
            .map(|e| {
                (
                    e.file_name(),
                    std::fs::read(e.path()).expect("a cache file"),
                )
            })
            .collect();
        *scanned = Some((key, files));
    }
    let builds = state_dir(opts).join("builds");
    std::fs::create_dir_all(&builds).expect("the builds cache");
    for (name, bytes) in scanned.iter().flat_map(|(_, files)| files) {
        std::fs::write(builds.join(name), bytes).expect("a cache file");
    }
}

thread_local! {
    /// This test thread's visits run under the default `upgrade_ask = false` ([`quietly`]).
    static QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// `f` with [`drive`]'s options under the default `[harness] upgrade_ask =
/// false`: the quiet upgrade ([`upgrade::Facts::quiet`]).
fn quietly<T>(f: impl FnOnce() -> T) -> T {
    QUIET.with(|q| q.set(true));
    let out = f();
    QUIET.with(|q| q.set(false));
    out
}

/// A `/bin/sh` script at `exe` running `body`, `exe` a hard link of one stand-in per process:
/// macOS assesses every new executable at its first exec, one at a time machine-wide (a fresh
/// script's first run took 0.46-2.3 s under load, a link of one already run 9 ms). The stand-in
/// is `#!/bin/sh` + `. "$0.sh"` and `body` goes beside it as `<exe>.sh`, so the same shell runs
/// the same commands with the same `$0`, arguments, environment and descriptors. Where no link
/// can be made (another filesystem), `exe` is a fresh copy.
#[cfg(unix)]
pub(super) fn sh_script(exe: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    const STAND_IN: &str = "#!/bin/sh\n. \"$0.sh\"\n";
    static SHARED: std::sync::Mutex<Option<(ScratchDir, PathBuf)>> = std::sync::Mutex::new(None);
    extern "C" fn remove_stand_in() {
        if let Ok(mut shared) = SHARED.try_lock() {
            shared.take();
        }
    }
    unsafe extern "C" {
        /// `stdlib.h`, declared inline as `aterm-gui`'s `test_scratch` declares
        /// it (the workspace's `libc` is `aterm-libc`, which does not carry it).
        fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
    }
    let mut beside = exe.as_os_str().to_owned();
    beside.push(".sh");
    std::fs::write(&beside, format!("{body}\n")).expect("the script's body");
    let mut shared = SHARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if shared.is_none() {
        let dir = ScratchDir::new("upgrade", "stand-in");
        let file = dir.join("sh");
        if std::fs::write(&file, STAND_IN).is_ok()
            && std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o555)).is_ok()
        {
            *shared = Some((dir, file));
            // SAFETY: registers, once, a plain `extern "C"` function with no
            // captured state that only takes the stand-in out of its lock
            // (dropping it removes its directory) as the process exits; a
            // failed registration only leaves the directory behind.
            unsafe { atexit(remove_stand_in) };
        }
    }
    let linked = shared
        .as_ref()
        .is_some_and(|(_, file)| std::fs::hard_link(file, exe).is_ok());
    drop(shared);
    if !linked {
        std::fs::write(exe, STAND_IN).expect("the script");
        std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

/// Runs independent cases each on its own thread, joins every one, and fails with the first
/// failed case's panic in the order given: cases that each wait out the product's fixed waits
/// overlap rather than add. A case shares nothing with another and sets any thread-local wait
/// it needs on its own thread.
pub(super) fn side_by_side<'a>(cases: Vec<Box<dyn FnOnce() + Send + 'a>>) {
    std::thread::scope(|scope| {
        let running: Vec<_> = cases.into_iter().map(|case| scope.spawn(case)).collect();
        let mut failed = None;
        for case in running {
            if let Err(panic) = case.join() {
                failed.get_or_insert(panic);
            }
        }
        if let Some(panic) = failed {
            std::panic::resume_unwind(panic);
        }
    });
}

/// A pid that was a process and is not one now.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    dead
}

fn ledger_lines(opts: &Opts) -> usize {
    std::fs::read_to_string(state_dir(opts).join("ledger.jsonl")).map_or(0, |t| t.lines().count())
}

// ---------------------------------------------------------------- the transcript

/// The READY answer is read whatever byte the tail window starts on, also on
/// the second byte of `é` (C3 A9): a strict decode would refuse the window and
/// read the answer as silence.
#[test]
fn a_ready_answer_is_read_when_the_tail_window_starts_inside_a_character() {
    let dir = scratch("tail");
    let path = dir.join("t.jsonl");
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let ready = format!(
        r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
    ) + "\n";
    // An earlier turn whose `é` straddles the first byte of a 256-byte window.
    let mut body = String::from(r#"{"type":"user","message":"é"#);
    body.push_str(&"b".repeat(252 - ready.len()));
    body.push_str("\"}\n");
    body.push_str(&ready);
    std::fs::write(&path, &body).expect("write");
    let bytes = body.as_bytes();
    assert_eq!(
        bytes[bytes.len() - 256],
        0xa9,
        "the window starts mid-character"
    );
    assert!(std::str::from_utf8(&bytes[bytes.len() - 256..]).is_err());
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 256).0,
        marker
    ));
    // Controls: the window on the character's lead byte, on ASCII, and whole.
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 257).0,
        marker
    ));
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 255).0,
        marker
    ));
    assert!(upgrade::transcript_has_ready(
        &tail_to_end(&path, 1 << 20).0,
        marker
    ));
    // The mark is where the bytes read end, whatever the window.
    assert_eq!(tail_to_end(&path, 256).1, bytes.len() as u64);
    assert_eq!(tail_to_end(&path, 1 << 20).1, bytes.len() as u64);
    assert_eq!(
        tail_to_end(&dir.join("absent.jsonl"), 256),
        (String::new(), 0)
    );
    // What is read past a mark: the rest, and nothing from a file that shrank.
    let end = bytes.len() as u64;
    assert_eq!(since(&path, end - 4, 1 << 20), "]}}\n");
    assert_eq!(since(&path, end + 1, 1 << 20), "");
    assert_eq!(since(&path, 0, 3), r#"{"t"#);
    // Lossy never invents an answer: another marker is still absent.
    assert!(!upgrade::transcript_has_ready(
        &tail_to_end(&path, 256).0,
        "ATERM-UPGRADE-READY-00000000"
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the composer

#[test]
fn a_transcript_append_cannot_grow_a_tail_read_past_its_budget() {
    // Eight bytes when metadata was read, sixteen by the read.
    let body = b"0123456789abcdef";
    let mut file = std::io::Cursor::new(body);
    let (tail, mark) = tail_window(&mut file, 8, 4);
    assert_eq!(tail, "4567");
    assert_eq!(mark, 8);
    let mut unread = String::new();
    file.read_to_string(&mut unread).expect("remaining bytes");
    assert_eq!(unread, "89abcdef", "the returned mark skips no append");

    let mut file = std::io::Cursor::new(body);
    assert_eq!(tail_window(&mut file, 2, 4), ("0123".to_owned(), 4));
    let mut file = std::io::Cursor::new(body);
    assert_eq!(tail_window(&mut file, 8, 0), (String::new(), 8));
}

/// `cell` replies from a Claude Code tab: the placeholder's first letter at
/// the caret row's column 2, the caret glyph, the no-break space after it, a
/// blank past the text.
#[test]
fn a_cell_reply_is_dim_only_by_its_attrs() {
    assert!(cell_is_dim("OK p 999999 111318 dim"));
    assert!(!cell_is_dim("OK %E2%9D%AF d0d0d0 111318 none"));
    assert!(!cell_is_dim("OK %C2%A0 d0d0d0 111318 none"));
    assert!(!cell_is_dim("OK %20 d0d0d0 111318 none"));
    // The attrs are a comma list, a link trails them, and a never-written
    // cell's grapheme is an empty token that must not shift the fields.
    assert!(cell_is_dim(
        "OK x 999999 111318 bold,dim link=https%3A%2F%2Fa"
    ));
    assert!(cell_is_dim("OK  999999 111318 dim,wide_cont"));
    assert!(!cell_is_dim("OK  999999 111318 none"));
    assert!(!cell_is_dim("ERR out of range"));
    assert!(!cell_is_dim("OK x 999999 111318"), "no attrs, no proof");
    assert!(!cell_is_dim(""));
}

/// A control connection whose server answers each request with the next of
/// `replies`, and hands back every request it read once the client is gone.
#[cfg(unix)]
fn fake_tab(replies: Vec<&'static str>) -> (Client, std::thread::JoinHandle<Vec<String>>) {
    use std::io::BufRead as _;
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let server = std::thread::spawn(move || {
        let mut out = theirs.try_clone().expect("clone");
        let mut asked = Vec::new();
        let mut replies = replies.into_iter();
        for line in std::io::BufReader::new(theirs).lines() {
            let Ok(line) = line else { break };
            asked.push(line);
            let reply = replies.next().unwrap_or("ERR no reply scripted");
            if writeln!(out, "{reply}").is_err() {
                break;
            }
        }
        asked
    });
    (RelayClient::new(ours), server)
}

/// `--status` reads other supervisors' claims and the owner's Autopilot Off from one
/// `sessions` read: a tab whose `supervisor=` names another holder (verbatim or encoded) is
/// claimed, and one whose `autopilot=` is `off` shows as held by that switch, never as a record
/// aterm stopped checking. Negative controls: a window host's own claim (any pid, encoded or
/// not), no claim, and a title that reads like one are no other supervisor's; `cruise`,
/// `land`, a shell's `-` and a title that reads `autopilot=off` are no Off; a refused read is
/// no roster; an instance that never answers is given up on.
#[cfg(unix)]
#[test]
fn the_roster_names_another_supervisors_tabs_and_the_off_ones_never_the_windows_own() {
    const ROSTER: &str = concat!(
        "OK 8\n",
        "1 s-a - live claude meta=1 window=1 supervisor=aterm-supervise:42 autopilot=off \
         upgrade=-\n",
        "2 s-b - live claude meta=1 window=1 supervisor=aterm-harness@777 autopilot=cruise \
         upgrade=-\n",
        "3 s-c - live zsh meta=0 window=1 supervisor=- autopilot=- upgrade=-\n",
        "4 s-d s-a live supervisor=aterm-supervise:9 meta=0 window=1 supervisor=- autopilot=- \
         upgrade=-\n",
        "5 s-e - live claude meta=1 window=1 supervisor=aterm-harness%40778 autopilot=land \
         upgrade=-\n",
        "6 s-f - live claude meta=1 window=1 supervisor=a%20script upgrade=-\n",
        "7 s-g - live claude meta=1 window=1 supervisor=- autopilot=off upgrade=-\n",
        "8 s-h - live autopilot=off meta=1 window=1 supervisor=- autopilot=cruise upgrade=-",
    );
    let (mut c, server) = fake_tab(vec![ROSTER]);
    let aside = roster_aside(&mut c, &[]);
    drop(c);
    assert_eq!(
        aside,
        Some(Aside {
            claimed: ["s-a", "s-f"].map(str::to_string).into(),
            off: ["s-a", "s-g"].map(str::to_string).into(),
            ..Aside::default()
        }),
        "another's claim, never the window's own; the owner's Off"
    );
    assert_eq!(server.join().expect("server"), vec!["sessions"]);
    let (mut c, _) = fake_tab(vec!["ERR denied"]);
    assert_eq!(roster_aside(&mut c, &[]), None, "no roster");
    // An instance that takes the request and never answers.
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(roster_aside(&mut RelayClient::new(ours), &[]));
    });
    let given_up = rx.recv_timeout(Duration::from_secs(60));
    drop(theirs);
    assert_eq!(given_up, Ok(None), "a wedged instance is no roster");
}

/// `--status` unions every instance's claims and Off tabs; one that refuses its roster adds
/// nothing and holds no other back.
#[cfg(unix)]
#[test]
fn the_aside_of_every_instance_is_one() {
    let (a, _) = fake_tab(vec![
        "OK 1\n1 s-a - live claude meta=1 window=1 supervisor=aterm-supervise:42 upgrade=-",
    ]);
    let (b, _) = fake_tab(vec![concat!(
        "OK 2\n",
        "1 s-b - live claude meta=1 window=1 supervisor=a%20script upgrade=-\n",
        "2 s-o - live claude meta=1 window=1 supervisor=- autopilot=off upgrade=-",
    )]);
    let (refused, _) = fake_tab(vec!["ERR denied"]);
    assert_eq!(
        aside_of([a, refused, b], &[]),
        Aside {
            claimed: ["s-a", "s-b"].map(str::to_string).into(),
            off: ["s-o"].map(str::to_string).into(),
            ..Aside::default()
        }
    );
}

/// On `--status` too, a tab whose agent's own switch is off is set aside as that switch's, as
/// the window's view reads it: the roster prints such a session `autopilot=off`, which read as
/// the Autopilot's alone would promise a move once Autopilot is cruise or land again, and no
/// cruise word can move it. Each row whose `program=` is a switched-off agent goes in
/// `agent_off`, which ranks before `off`. Negative control: the other agent's row, Off by its
/// own word, is Off alone; with no agent switched off, `agent_off` is empty.
#[cfg(unix)]
#[test]
fn a_switched_off_agents_tab_is_set_aside_as_its_switchs_on_status() {
    let roster = || {
        fake_tab(vec![concat!(
            "OK 2\n",
            "1 s-x - live codex meta=1 window=1 program=codex supervisor=- autopilot=off upgrade=-\n",
            "2 s-c - live claude meta=1 window=1 program=claude supervisor=- autopilot=off upgrade=-",
        )])
        .0
    };
    let aside = aside_of([roster()], &[aterm_phase::Program::Codex]);
    assert_eq!(aside.agent_off, ["s-x"].map(str::to_string).into());
    assert_eq!(aside.off, ["s-c", "s-x"].map(str::to_string).into());
    assert!(aside_of([roster()], &[]).agent_off.is_empty());
}

#[cfg(unix)]
const TAIL_DRAFT: &str = r#"OK {"rows":["────────────","❯ draft","────────────"],"cursor":{"row":57,"col":7},"first":56}"#;
#[cfg(unix)]
const TAIL_EMPTY: &str =
    r#"OK {"rows":["────────────","❯ ","────────────"],"cursor":{"row":57,"col":2},"first":56}"#;
#[cfg(unix)]
const FULL_DRAFT: &str =
    r#"OK {"rows":["────────────","❯ draft","────────────"],"cursor":{"row":1,"col":7},"first":0}"#;
#[cfg(unix)]
const FULL_EMPTY: &str =
    r#"OK {"rows":["────────────","❯ ","────────────"],"cursor":{"row":1,"col":2},"first":0}"#;

#[cfg(unix)]
#[test]
fn continuation_poll_uses_only_the_tail_while_a_draft_remains() {
    let (mut c, server) = fake_tab(vec![TAIL_DRAFT, TAIL_DRAFT]);
    let mut tail_available = true;
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(tail_available);
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec!["@s-x text --json tail=20"; 2],
        "a draft never needs a full-grid read or a cell probe"
    );
}

#[cfg(unix)]
#[test]
fn continuation_poll_confirms_an_empty_tail_with_the_full_screen() {
    let (mut c, server) = fake_tab(vec![
        TAIL_EMPTY,
        "OK %20 d0d0d0 111318 none",
        FULL_DRAFT,
        TAIL_EMPTY,
        "OK %20 d0d0d0 111318 none",
        FULL_EMPTY,
        "OK %20 d0d0d0 111318 none",
    ]);
    let mut tail_available = true;
    assert!(
        !continuation_composer_empty(&mut c, "s-x", &mut tail_available),
        "a blank tail alone cannot authorize continuation"
    );
    assert!(continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec![
            "@s-x text --json tail=20",
            "@s-x cell 57 2",
            "@s-x text --json",
            "@s-x text --json tail=20",
            "@s-x cell 57 2",
            "@s-x text --json",
            "@s-x cell 1 2",
        ],
        "tail `first` and full-screen cell coordinates stay distinct"
    );
}

#[cfg(unix)]
#[test]
fn continuation_poll_falls_back_on_an_unreadable_or_inconclusive_tail() {
    const NO_CARET: &str =
        r#"OK {"rows":["continuation of a long draft"],"cursor":{"row":58,"col":2},"first":56}"#;
    let (mut c, server) = fake_tab(vec![
        NO_CARET,
        FULL_DRAFT,
        "ERR usage: text [--json] [tail=<n>]",
        FULL_DRAFT,
        FULL_DRAFT,
    ]);
    let mut tail_available = true;
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(tail_available, "a truncated slice may resolve next poll");
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    assert!(!tail_available, "an unsupported tail is probed only once");
    assert!(!continuation_composer_empty(
        &mut c,
        "s-x",
        &mut tail_available
    ));
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec![
            "@s-x text --json tail=20",
            "@s-x text --json",
            "@s-x text --json tail=20",
            "@s-x text --json",
            "@s-x text --json",
        ]
    );
}

/// The composer check trusts only an `OK … dim` at the caret row's column 2.
/// The placeholder is empty; the same rows and cursor with text typed and the
/// caret moved home (←, Home, ctrl-a: the cursor back at column 2, the cell
/// `none`) are a draft, which the cursor alone would read as empty; an
/// unreadable cell is a draft.
#[cfg(unix)]
#[test]
fn a_homed_draft_is_never_read_as_the_placeholder() {
    // The live composer's shape (`❯`, a no-break space, the text), on screen
    // rows 53-55 of the tab.
    let screen = |text: &str, col: usize| Screen {
        rows: vec!["─".repeat(20), format!("❯\u{a0}{text}"), "─".repeat(20)],
        cursor: Some((1, col)),
        seq: 1,
        first: 53,
        generation: None,
        human: HumanInput::Never,
    };
    let text = "push it as soon as the gates pass";
    let (mut c, server) = fake_tab(vec![
        "OK p 999999 111318 dim",
        "OK p d0d0d0 111318 none",
        "ERR out of range",
        "OK %20 d0d0d0 111318 none",
    ]);
    assert!(
        composer_empty(&mut c, "s-x", &screen(text, 2)),
        "the placeholder"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(text, 2)),
        "typed, the caret moved home"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(text, 2)),
        "unreadable"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(text, 35)),
        "typed, the caret at the end: nothing asked"
    );
    assert!(composer_empty(&mut c, "s-x", &screen("", 2)), "blank");
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec!["@s-x cell 54 2"; 4],
        "the caret row's column 2, in the screen rows `cell` takes; a caret inside the text \
         asks nothing"
    );
    // The rule itself: the rows and cursor of the two are identical, and only
    // the dim bit tells them apart.
    let homed = screen(text, 2);
    assert!(upgrade::composer_is_empty(&homed.rows, homed.cursor, true));
    assert!(!upgrade::composer_is_empty(
        &homed.rows,
        homed.cursor,
        false
    ));
}

/// A grid displaced by a net-zero re-grid never makes a placeholder a draft: Claude sees no
/// size change in a quick 61>60>61-row re-grid and does not repaint, so every row sits one
/// higher than it drew it and the cursor lands on the rule below the composer. The dim cell is
/// read at the caret row, never the cursor's. Negative controls: the displaced grid with typed
/// text (`none`) or an unreadable cell is a draft; a cursor inside the text asks nothing.
#[cfg(unix)]
#[test]
fn a_displaced_grid_never_makes_a_placeholder_a_draft() {
    let rule = "─".repeat(20);
    let text = "push it as soon as the gates pass";
    // Claude's composer at rows 55-57 of the tab; the engine shows it at 54-56
    // with the cursor where Claude left it, one row below the prompt row.
    let screen = |cursor: Option<(usize, usize)>| Screen {
        rows: vec![rule.clone(), format!("❯\u{a0}{text}"), rule.clone()],
        cursor,
        seq: 1,
        first: 54,
        generation: None,
        human: HumanInput::Never,
    };
    let (mut c, server) = fake_tab(vec![
        "OK p 999999 111318 dim",
        "OK p d0d0d0 111318 none",
        "ERR out of range",
        "OK p 999999 111318 dim",
        "OK p 999999 111318 dim",
    ]);
    assert!(
        composer_empty(&mut c, "s-x", &screen(Some((2, 2)))),
        "displaced: the cursor on the rule below, the suggestion dim"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(Some((2, 2)))),
        "displaced, typed: a draft"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(Some((2, 2)))),
        "displaced, unreadable: a draft"
    );
    assert!(
        composer_empty(&mut c, "s-x", &screen(None)),
        "no cursor at all: the dim cell decides"
    );
    assert!(
        composer_empty(&mut c, "s-x", &screen(Some((0, 40)))),
        "a cursor on another row says nothing of the composer"
    );
    assert!(
        !composer_empty(&mut c, "s-x", &screen(Some((1, 40)))),
        "the cursor inside the text is typed text: nothing is asked"
    );
    drop(c);
    assert_eq!(
        server.join().expect("server"),
        vec!["@s-x cell 55 2"; 5],
        "always the caret row, never the cursor's"
    );
}

// ---------------------------------------------------------------- the terminal

fn tty_rows(rows: &[(u32, u32, &str, &str)]) -> Vec<(u32, u32, String, String)> {
    rows.iter()
        .map(|(p, pp, tty, name)| (*p, *pp, (*tty).to_string(), (*name).to_string()))
        .collect()
}

/// The terminal proof over process tables: the program that opened the
/// agent's terminal is an aterm, or gone (the tab handed on), never a
/// multiplexer, `script`, or nothing.
#[test]
fn a_terminal_is_the_tabs_only_when_aterm_opened_it() {
    // A live agent, its `-zsh`, and launchd: the aterm that opened the tab
    // handed it on and exited.
    let live = tty_rows(&[
        (4205, 1916, "ttys000", "2.1.281"),
        (1916, 1, "ttys000", "zsh"),
        (1, 0, "??", "launchd"),
    ]);
    assert_eq!(terminal_owner(4205, &live), Some((1, "launchd")));
    assert!(terminal_owner(4205, &live).is_some_and(owned_by_aterm));
    // A tab of an instance still running: the owner is that aterm (its argv0
    // aliases too).
    for name in ["aterm", "aterm-gui"] {
        let fresh = tty_rows(&[
            (700, 600, "ttys004", "claude"),
            (600, 500, "ttys004", "zsh"),
            (500, 1, "??", name),
            (1, 0, "??", "launchd"),
        ]);
        assert!(
            terminal_owner(700, &fresh).is_some_and(owned_by_aterm),
            "{name}"
        );
    }
    // A program under `script`, on the pty it opened.
    let script = tty_rows(&[
        (99234, 99225, "ttys002", "sleep"),
        (99225, 1, "??", "script"),
        (1, 0, "??", "launchd"),
    ]);
    assert_eq!(terminal_owner(99234, &script), Some((99225, "script")));
    assert!(!terminal_owner(99234, &script).is_some_and(owned_by_aterm));
    // GNU screen started in a tab: the window's agent is on the pty screen's
    // server opened; the tab's own shell is on the tab's.
    let screen = tty_rows(&[
        (29687, 28893, "ttys005", "2.1.0"),
        (28893, 28890, "ttys005", "zsh"),
        (28890, 28889, "??", "screen"),
        (28889, 22415, "ttys006", "screen"),
        (22415, 1, "ttys006", "zsh"),
        (1, 0, "??", "launchd"),
    ]);
    assert!(!terminal_owner(29687, &screen).is_some_and(owned_by_aterm));
    assert!(terminal_owner(22415, &screen).is_some_and(owned_by_aterm));
    // A tmux pane (Linux spellings): the server, a daemon, opened it.
    let tmux = tty_rows(&[
        (800, 801, "pts/7", "claude"),
        (801, 802, "pts/7", "bash"),
        (802, 1, "?", "tmux"),
        (1, 0, "?", "systemd"),
    ]);
    assert!(!terminal_owner(800, &tmux).is_some_and(owned_by_aterm));
    assert_eq!(owner_word(terminal_owner(800, &tmux)), "tmux");
    // No terminal at all, or a walk that cannot finish: no proof.
    let bare = tty_rows(&[(900, 1, "??", "claude"), (1, 0, "??", "launchd")]);
    assert_eq!(terminal_owner(900, &bare), None);
    let cut = tty_rows(&[(901, 555, "ttys001", "claude")]);
    assert_eq!(terminal_owner(901, &cut), None);
    assert_eq!(owner_word(None), "none");
    assert_eq!(owner_word(Some((5, "a b/c"))), "abc");
}

/// End to end: an agent on a pty the tab does not own (`script`'s here, the
/// shape of every tmux or screen pane, whose environment still names the tab)
/// is never driven through that tab. The sweep stops before any socket is
/// dialled, says why once (in the ledger and as an act the window's host
/// logs), then waits in silence.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_on_a_pty_the_tab_does_not_own_is_never_driven_through_the_tab() {
    let dir = scratch("pty");
    let opts = drive(&dir);
    let exe = std::env::current_exe().expect("exe");
    let mut script = Command::new("/usr/bin/script")
        .args(["-q", "/dev/null"])
        .arg(&exe)
        .args(PARK)
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("script");
    let owner = script.id();
    let agent = (0..3_000)
        .find_map(|_| {
            let hit = table()
                .into_iter()
                .find(|(_, pp, _)| *pp == owner)
                .map(|(p, _, _)| p);
            if hit.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            hit
        })
        .expect("the agent under script");
    wait_exec(agent);
    let sf = register(&opts.home, agent, SESSION);
    let got = visit(&opts, &sf, &table(), &newer(), &Live);
    let again = visit(&opts, &sf, &table(), &newer(), &Live);
    let _ = Command::new("kill")
        .args(["-9", &agent.to_string()])
        .status();
    let _ = script.kill();
    let _ = script.wait();
    assert_eq!(got.step, "held-back:terminal:script", "{got:?}");
    assert_eq!(got.tab, TAB);
    assert!(got.is_act(), "said, where the window's host logs it");
    assert_eq!(again.step, "wait:terminal:script", "{again:?}");
    assert!(!again.is_act(), "said ONCE");
    assert_eq!(ledger_lines(&opts), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the job

/// [`Job`] from `ps`'s numbers, recorded shapes first, then real processes.
#[cfg(unix)]
#[test]
fn only_its_shells_foreground_job_is_ever_announced_to_or_ended() {
    // `ps -o pid,ppid,pgid,tpgid` of a live agent and its `-zsh`.
    assert_eq!(job(4205, 4205, 4205, 1916), Job::Foreground);
    // The same agent suspended or backgrounded: the shell holds the terminal.
    assert_eq!(job(4205, 4205, 1916, 1916), Job::Background);
    // A bash launcher (`cc.sh`, no `exec`) and its agent on a pty share one
    // group, and it is the terminal's.
    assert_eq!(job(30771, 30770, 30770, 30770), Job::NoJobControl);
    // zsh with `unsetopt monitor`: the agent in the shell's own group.
    assert_eq!(job(24965, 24932, 24932, 24932), Job::NoJobControl);
    // Real processes: a child left in its parent's group (what `sh -c`, `make`
    // or an IDE task does) is no job; one given a group of its own is a job,
    // and not the terminal's foreground.
    let me = std::process::id();
    let mut plain = parked().spawn().expect("spawn");
    let mut own = parked().process_group(0).spawn().expect("spawn");
    let (a, b) = (job_of(plain.id()), job_of(own.id()));
    for c in [&mut plain, &mut own] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(a, Some((Job::NoJobControl, me)));
    assert_eq!(b, Some((Job::Background, me)));
}

/// End to end: an agent no job-control shell started (a launcher script that
/// runs `claude` without `exec`, `sh -c`, `make`, an IDE task) is refused
/// before anything is typed, once, in the ledger: never announced to, never
/// ended with nothing to relaunch it.
#[cfg(unix)]
#[test]
fn an_agent_no_job_control_shell_started_is_refused_before_anything_is_typed() {
    let dir = scratch("job");
    let opts = drive(&dir);
    // In this process's group: what a launcher script gives its child.
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let first = visit(&opts, &sf, &table(), &newer(), &Live);
    let again = visit(&opts, &sf, &table(), &newer(), &Live);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(first.step, "refused:not-a-shell-job", "{first:?}");
    assert!(first.is_act(), "said, where the window's host notes it");
    assert_eq!(
        load(&opts, SESSION).map(|s| s.phase),
        Some(Phase::Failed("not-a-shell-job".to_string()))
    );
    // Said once: the next sweep finds the upgrade failed and asks nothing.
    assert_eq!(again.step, "wait:no-socket", "{again:?}");
    assert_eq!(ledger_lines(&opts), 1);
    // A dry run says what it would do and records nothing.
    let other = "0badf00d-1111-2222-3333-444455556667";
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), other);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let got = visit(&dry, &sf, &table(), &newer(), &Live);
    // Control: the same stand-in as a job of its own passes this proof (and,
    // on no terminal at all, is no foreground job: it waits).
    let mut job = parked().process_group(0).spawn().expect("spawn");
    wait_exec(job.id());
    let third = "0badf00d-1111-2222-3333-444455556668";
    let sf = register(&opts.home, job.id(), third);
    let control = visit(&opts, &sf, &table(), &newer(), &Live);
    for c in [&mut agent, &mut job] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(got.step, "would-refuse:not-a-shell-job");
    assert_eq!(load(&opts, other), None);
    assert_eq!(control.step, "wait:not-foreground", "{control:?}");
    assert!(!control.is_act(), "{control:?}");
    assert_eq!(ledger_lines(&opts), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A visit's own retarget is said once in the ledger (`retargeted:<from>-><to>`): the
/// stand-in, a job of its own, waits at the foreground proof after the retarget, so the
/// row is the retarget's alone, and the next visit writes none. Negative controls: a
/// record already on the target and a finished one write no row; a dry run writes nothing.
#[cfg(unix)]
#[test]
fn a_visits_own_retarget_is_said_once_in_the_ledger() {
    let dir = scratch("visit-retarget");
    let opts = drive(&dir);
    let waiting = |to: &str, phase: Phase| St {
        phase,
        from: "1.0.0".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        salt: 100,
        pending_since: 100,
        ..St::default()
    };
    let mut agents = Vec::new();
    let mut visit_as = |opts: &Opts, session: &str, prior: Option<St>| {
        let agent = parked().process_group(0).spawn().expect("spawn");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), session);
        if let Some(mut prior) = prior {
            // An announced round's notice reached this very process.
            if matches!(prior.phase, Phase::Announced { .. }) {
                prior.notice_pid = sf.pid;
                prior.notice_start = squash(&sf.proc_start);
            }
            save(opts, session, &prior);
        }
        let first = visit(opts, &sf, &table(), &newer(), &Live);
        let again = visit(opts, &sf, &table(), &newer(), &Live);
        agents.push(agent);
        (first, again)
    };
    let (first, again) = visit_as(&opts, SESSION, Some(waiting("9.9.8", Phase::Pending)));
    for r in [&first, &again] {
        assert_eq!(r.step, "wait:not-foreground", "{r:?}");
        assert!(!r.is_act(), "{r:?}");
    }
    assert_eq!(
        noted_for(&opts, "retargeted:9.9.8->9.9.9", SESSION),
        1,
        "said once, by the visit that made it"
    );
    assert_eq!(ledger_lines(&opts), 1, "and nothing else");
    let st = load(&opts, SESSION).expect("the record");
    assert_eq!((st.to.as_str(), &st.phase), ("9.9.9", &Phase::Pending));
    assert_eq!(st.behind_since(), 100, "the move clock kept");
    // Negative controls: no retarget, no row.
    let on_target = "0badf00d-1111-2222-3333-444455556670";
    let _ = visit_as(&opts, on_target, Some(waiting("9.9.9", Phase::Pending)));
    let finished = "0badf00d-1111-2222-3333-444455556671";
    let _ = visit_as(&opts, finished, Some(waiting("9.9.8", Phase::Done)));
    assert_eq!(ledger_lines(&opts), 1, "neither wrote a row");
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let unwritten = "0badf00d-1111-2222-3333-444455556672";
    save(&opts, unwritten, &waiting("9.9.8", Phase::Pending));
    let (got, _) = visit_as(&dry, unwritten, None);
    assert!(!got.step.is_empty(), "{got:?}");
    assert_eq!(ledger_lines(&opts), 1, "a dry run says nothing");
    assert_eq!(
        load(&opts, unwritten).map(|st| st.to),
        Some("9.9.8".to_string()),
        "and writes nothing"
    );
    // How it left the round, said truly: an announced round retargeted in place (its
    // notice this agent's, for no model, on the build the session runs) keeps its round,
    // marker and READY, and its row says so. Negative control: a stopped round's retarget
    // starts a new pending round and says "started over".
    let detail_of = |session: &str| {
        std::fs::read_to_string(ledger_path(&opts))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("session").and_then(Value::as_str) == Some(session))
            .filter(|v| {
                v.get("step")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.starts_with("retargeted:"))
            })
            .find_map(|v| v.get("detail").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    };
    assert!(
        detail_of(SESSION).ends_with("; still pending") && !detail_of(SESSION).contains("READY"),
        "{}",
        detail_of(SESSION)
    );
    for (session, phase, from) in [
        (
            "0badf00d-1111-2222-3333-444455556673",
            Phase::Announced { at_s: 100, asks: 1 },
            "1.0.0",
        ),
        (
            "0badf00d-1111-2222-3333-444455556674",
            Phase::Failed("unanswered".to_string()),
            "1.0.0",
        ),
        // A pending round the abandoning arm rebuilds (its session moved off the build
        // the round began on): pending before and after, no marker either side, yet a
        // new round.
        (
            "0badf00d-1111-2222-3333-444455556675",
            Phase::Pending,
            "0.9.0",
        ),
    ] {
        let agent = parked().process_group(0).spawn().expect("spawn");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), session);
        let prior = St {
            marker: if phase == Phase::Pending {
                String::new()
            } else {
                "ATERM-UPGRADE-READY-0badf00d".to_string()
            },
            from: from.to_string(),
            notice_pid: sf.pid,
            notice_start: squash(&sf.proc_start),
            wait: "not-idle:busy".to_string(),
            wait_since: 200,
            ..waiting("9.9.8", phase.clone())
        };
        save(&opts, session, &prior);
        let r = visit(&opts, &sf, &table(), &newer(), &Live);
        agents.push(agent);
        assert_eq!(r.step, "wait:not-foreground", "{r:?}");
        assert_eq!(noted_for(&opts, "retargeted:9.9.8->9.9.9", session), 1);
        let st = load(&opts, session).expect("the record");
        let said = detail_of(session);
        if matches!(phase, Phase::Announced { .. }) {
            assert_eq!(st.phase, phase, "kept in place");
            assert_eq!(st.marker, prior.marker);
            assert!(
                said.contains("its notice and any READY still count"),
                "{said}"
            );
            assert!(!said.contains("started over"), "{said}");
        } else {
            assert_eq!(st.phase, Phase::Pending, "a new round");
            assert!(said.contains("started over"), "{said}");
            assert!(!said.contains("still count"), "{said}");
        }
        if phase == Phase::Pending {
            // Phase and marker as they were, yet a new round.
            assert_eq!((&st.phase, st.marker.as_str()), (&prior.phase, ""));
            assert_ne!(st.salt, prior.salt, "rebuilt: a new round");
        }
    }
    for mut agent in agents {
        let _ = agent.kill();
        let _ = agent.wait();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`St::retarget`] names the arm that made a retarget: a pending record with a model move due
/// and a newer build is rebuilt ("started over"), an owed release and [`St::left_typed`] riding
/// it. Negative control: with no model move it moves in place ("still pending").
#[test]
fn a_retarget_is_told_by_the_arm_that_made_it() {
    let v = |s: &str| Version::parse(s).expect("version");
    let to = Candidate {
        exe: PathBuf::from("/nonexistent/claude"),
        version: v("2.1.291"),
        source: Source::Managed,
    };
    let prior = St {
        phase: Phase::Pending,
        from: "2.1.290".to_string(),
        to: "2.1.290".to_string(),
        source: "managed".to_string(),
        salt: 100,
        pending_since: 100,
        wait: "not-idle:busy".to_string(),
        wait_since: 200,
        last_stop: upgrade::GAVE_UP.to_string(),
        left_typed: "[aterm harness] Claude Code 2.1.290 (man".to_string(),
        ..St::default()
    };
    let (st, how) = St::retarget(
        Some(prior.clone()),
        &v("2.1.290"),
        &to,
        Some("claude-opus-5-5"),
        Proc::default(),
        900,
    );
    assert_eq!(how, Retarget::Anew);
    assert_eq!((&st.phase, st.marker.as_str()), (&prior.phase, ""));
    assert!(st.wait.is_empty() && st.last_stop.is_empty() && st.salt == 900);
    assert_eq!(st.left_typed, prior.left_typed, "its words ride it");
    assert_eq!(retarget_how(how, &st), "started over");
    // Negative control: no model move, in place.
    let (st, how) = St::retarget(
        Some(prior.clone()),
        &v("2.1.290"),
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(how, Retarget::InPlace);
    assert_eq!((st.wait.as_str(), st.salt), ("not-idle:busy", 100));
    assert_eq!(retarget_how(how, &st), "still pending");
}

/// One job read, as every look before an announce or a SIGTERM takes it: only
/// its shell's foreground job goes on, with that shell.
#[test]
fn a_job_read_lets_through_only_its_shells_foreground_job() {
    assert_eq!(foreground_shell(Some((Job::Foreground, 1916))), Ok(1916));
    assert_eq!(
        foreground_shell(Some((Job::Background, 1916))),
        Err("not-foreground")
    );
    assert_eq!(
        foreground_shell(Some((Job::NoJobControl, 1916))),
        Err("not-a-shell-job")
    );
    assert_eq!(foreground_shell(None), Err("ids"));
}

/// A [`Kernel`] that answers as scripted: the agent is `shell`'s foreground
/// job for its first `foreground` job reads and a background job after (a
/// ctrl-z landing between two looks, which no real process does on cue), its
/// terminal the tab's (the tab handed on), and its parent `parent`.
#[cfg(unix)]
struct Script {
    shell: u32,
    foreground: usize,
    reads: std::cell::Cell<usize>,
    parent: Option<u32>,
}

#[cfg(unix)]
impl Script {
    fn new(shell: u32, foreground: usize, parent: Option<u32>) -> Script {
        Script {
            shell,
            foreground,
            reads: std::cell::Cell::new(0),
            parent,
        }
    }
}

#[cfg(unix)]
impl Kernel for Script {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        let n = self.reads.get();
        self.reads.set(n + 1);
        let job = if n < self.foreground {
            Job::Foreground
        } else {
            Job::Background
        };
        Some((job, self.shell))
    }

    fn parent(&self, _: u32) -> Option<u32> {
        self.parent
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        Some((1, "launchd".to_string()))
    }
}

/// A visit's parked test process with its process facts injected (its start, its background
/// work), so its restart tests script the job and server races whether or not a concurrent
/// case starts a `ps` at that moment. Every other `Script` user keeps the live probes.
#[cfg(unix)]
struct ParkedProcess<'a> {
    script: &'a Script,
    pid: u32,
    start: Option<String>,
    background: Vec<String>,
}

#[cfg(unix)]
impl Kernel for ParkedProcess<'_> {
    fn job(&self, pid: u32) -> Option<(Job, u32)> {
        self.script.job(pid)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.script.parent(pid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        self.script.terminal(pid)
    }

    fn process_start(&self, pid: u32) -> Option<String> {
        (pid == self.pid).then(|| self.start.clone()).flatten()
    }

    fn restart_background(&self, pid: u32) -> Vec<String> {
        if pid == self.pid {
            self.background.clone()
        } else {
            vec![UNREADABLE_TABLE.to_string()]
        }
    }

    fn shell_holds_tab(&self, shell: u32) -> Option<bool> {
        self.script.shell_holds_tab(shell)
    }

    fn describe(&self, agent: u32, procs: &[(u32, String)]) -> Vec<upgrade::Held> {
        self.script.describe(agent, procs)
    }
}

/// The screen [`instance`] shows: an idle Claude's empty composer, the caret at
/// column 2 of its row, and no person's input to the session (`"human_ms":null`,
/// so the tab is not attended: `upgrade::attended_by`).
#[cfg(unix)]
fn idle_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// The token [`instance`] writes beside its socket.
#[cfg(unix)]
const TOKEN: &str = "0badf00d0badf00d";

/// What [`instance_with`]'s stand-in answers, beyond its fixed replies.
#[cfg(unix)]
#[derive(Clone)]
struct Answers {
    /// Columns after the tab's `sessions` row (`active=1 wfocus=1`).
    row: &'static str,
    /// The `text --json` body.
    screen: String,
    /// After this many `text` replies, this body instead.
    after: Option<(usize, String)>,
    /// The `help <verb>` reply.
    help: &'static str,
    /// Each `turn`'s reply, in order; the last one repeats.
    turn: Vec<&'static str>,
    /// The `blocks 1 --json` reply (the shell integration's command blocks).
    blocks: &'static str,
    /// The `line <n>` reply.
    line: &'static str,
    /// The `rekey shell=<pid>` reply (a `rekey withdraw` is answered
    /// `OK rekey withdrawn`).
    rekey: &'static str,
    /// How long the stand-in agent answers a submitted turn: a `turn` that
    /// settles on a quiet longer than a keystroke's (`idle=` above 1 ms)
    /// returns only once that answer is over, as the server's does.
    answer: Duration,
    /// The `lease acquire …` reply; a `lease release` is answered `OK lease
    /// released` after an acquire this granted, `OK lease none` otherwise.
    lease: &'static str,
    /// The `status` reply from the second read after a `lease acquire` on:
    /// what a person's keystroke, or a prompt someone sent, makes it between a
    /// restart's first look (under the hand) and its last.
    after_first_look: Option<&'static str>,
    /// The `signal` reply, sending nothing (empty: the stand-in agent named
    /// by `pid=` is sent SIGTERM, then `OK`).
    signal: &'static str,
    /// The `cell <r> <c>` reply (default: a plain blank, not dim).
    cell: &'static str,
    /// The `who` reply: the host's roster, whose `fgpgid=` proves the shell a
    /// relaunch line is typed at holds the tab (`None`: unscripted, so no
    /// relaunch line is ever typed).
    who: Option<String>,
    /// A Claude Code prompt the stand-in draws itself ([`Prompt`]) in place of `screen`:
    /// what a `turn` answered with a skip that typed part of its text (`typed=<k>`) or all
    /// of it (`reason=guard`) left there, and a person's key after it where `person_key`
    /// says; `key … ctrl+u` kills the line when its `if-gen=` names the screen drawn.
    prompt: Option<Prompt>,
}

/// What [`Answers::prompt`]'s stand-in prompt holds and how it is drawn.
#[cfg(unix)]
#[derive(Clone, Default)]
struct Prompt {
    /// The text in the prompt.
    draft: String,
    /// A person's key lands after a stopped turn's part (`x`).
    person_key: bool,
}

#[cfg(unix)]
impl Prompt {
    /// The prompt as Claude Code draws it, 40 columns wide: its rules, the
    /// caret row, the draft's continuation rows indented two, the cursor
    /// behind the text; screen generation `4.<generation>`.
    fn screen(&self, generation: u64) -> String {
        const WIDTH: usize = 40;
        let rule = "─".repeat(WIDTH);
        let chars: Vec<char> = self.draft.chars().collect();
        let mut rows = vec![rule.clone()];
        let mut chunks: Vec<String> = chars
            .chunks(WIDTH - 2)
            .map(|c| c.iter().collect())
            .collect();
        if chunks.is_empty() {
            chunks.push(String::new());
        }
        for (i, chunk) in chunks.iter().enumerate() {
            rows.push(format!("{}{chunk}", if i == 0 { "❯ " } else { "  " }));
        }
        let (row, col) = (rows.len() - 1, 2 + chunks[chunks.len() - 1].chars().count());
        rows.push(rule);
        let rows: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
        format!(
            r#"{{"rows":[{}],"cursor":{{"row":{row},"col":{col}}},"seq":{generation},"first":0,"gen":"4.{generation}","human_ms":null,"human_seq":0}}"#,
            rows.join(",")
        )
    }
}

#[cfg(unix)]
impl Default for Answers {
    fn default() -> Self {
        Self {
            row: "",
            screen: idle_screen(),
            after: None,
            help: "ERR unscripted",
            turn: vec!["OK status=done"],
            blocks: "ERR unscripted",
            line: "ERR unscripted",
            rekey: "ERR unscripted",
            answer: Duration::ZERO,
            lease: "ERR unscripted",
            after_first_look: None,
            signal: "",
            cell: "OK %20 d0d0d0 111318 none",
            who: None,
            prompt: None,
        }
    }
}

/// A control socket at `<dir>/t.sock` (its token beside it) standing in for [`TAB`]'s aterm with
/// an idle Claude: `text --json` is [`idle_screen`], `blocks` unscripted, `status` holding
/// nothing, each overridden by `<dir>/t.screen`, `t.blocks` or `t.status`; a `turn` is answered
/// OK and recorded. Returns the socket and every request it read.
#[cfg(unix)]
fn instance(dir: &Path) -> (String, crate::harness::FakeInstanceLog) {
    instance_of(dir, TAB)
}

/// [`instance`] for the tab `tab`.
#[cfg(unix)]
fn instance_of(dir: &Path, tab: &'static str) -> (String, crate::harness::FakeInstanceLog) {
    instance_for(dir, tab, Answers::default())
}

/// [`instance`], answering as `answers` says.
#[cfg(unix)]
fn instance_with(dir: &Path, answers: Answers) -> (String, crate::harness::FakeInstanceLog) {
    instance_for(dir, TAB, answers)
}

/// [`instance`] for the tab `tab`, answering as `answers` says.
#[cfg(unix)]
fn instance_for(
    dir: &Path,
    tab: &'static str,
    answers: Answers,
) -> (String, crate::harness::FakeInstanceLog) {
    let status_file = dir.join("t.status");
    let screen_file = dir.join("t.screen");
    let blocks_file = dir.join("t.blocks");
    let mut turn_replies = answers.turn.clone();
    let mut texts = 0;
    let mut prompt = answers.prompt.clone();
    let mut generation = 77;
    // Status reads since a `lease acquire` (`None`: none acquired yet).
    let mut looks: Option<usize> = None;
    crate::harness::fake_instance(&dir.join("t.sock"), TOKEN, move |line| {
        let verb = line
            .split_whitespace()
            .find(|w| !w.starts_with('@'))
            .unwrap_or("");
        match verb {
            "sessions" => format!("OK 1\nlocal {tab} 0 idle claude {}", answers.row),
            "text" if prompt.is_some() => {
                format!(
                    "OK {}",
                    prompt
                        .as_ref()
                        .map_or_else(String::new, |p| p.screen(generation))
                )
            }
            "text" => {
                texts += 1;
                match (&answers.after, std::fs::read_to_string(&screen_file)) {
                    (_, Ok(written)) => format!("OK {}", written.trim()),
                    (Some((n, later)), _) if texts > *n => format!("OK {later}"),
                    _ => format!("OK {}", answers.screen),
                }
            }
            "cell" => answers.cell.to_string(),
            "who" => answers
                .who
                .clone()
                .unwrap_or_else(|| "ERR unscripted".to_string()),
            "status" => {
                let look = looks.as_mut().map(|n| {
                    *n += 1;
                    *n
                });
                match answers.after_first_look {
                    Some(later) if look.is_some_and(|n| n >= 2) => later.to_string(),
                    _ => std::fs::read_to_string(&status_file).map_or_else(
                        |_| "OK schema=1 hold=0 hand=-".to_string(),
                        |s| s.trim().to_string(),
                    ),
                }
            }
            "help" => answers.help.to_string(),
            "blocks" => std::fs::read_to_string(&blocks_file)
                .map_or_else(|_| answers.blocks.to_string(), |b| b.trim().to_string()),
            "line" => answers.line.to_string(),
            "rekey" if line.ends_with(" rekey withdraw") => "OK rekey withdrawn".to_string(),
            "rekey" => answers.rekey.to_string(),
            "lease" if line.contains(" lease release ") => {
                if answers.lease.starts_with("OK lease acquired") {
                    "OK lease released".to_string()
                } else {
                    "OK lease none".to_string()
                }
            }
            "lease" => {
                looks.get_or_insert(0);
                answers.lease.to_string()
            }
            "turn" => {
                let settles_on_quiet = line
                    .split_whitespace()
                    .find_map(|w| w.strip_prefix("idle="))
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .is_none_or(|ms| ms > 1);
                if settles_on_quiet {
                    std::thread::sleep(answers.answer);
                }
                let next = turn_replies.first().copied().unwrap_or("OK status=done");
                if turn_replies.len() > 1 {
                    turn_replies.remove(0);
                }
                // What the stopped turn left in the prompt.
                if let Some(p) = prompt.as_mut() {
                    let text = line
                        .find(" [aterm harness]")
                        .map_or("", |at| &line[at + 1..]);
                    let typed = next
                        .split_whitespace()
                        .find_map(|w| w.strip_prefix("typed="))
                        .and_then(|k| k.parse::<usize>().ok());
                    let left = match typed {
                        Some(k) if k > 0 => Some(text.chars().take(k).collect()),
                        _ if next.contains("reason=guard") => Some(text.to_string()),
                        _ => None,
                    };
                    if let Some(left) = left {
                        p.draft = left;
                        if p.person_key {
                            p.draft.push('x');
                        }
                        generation += 1;
                    }
                }
                next.to_string()
            }
            // The prompt's ctrl+u: the line killed, while the screen is
            // the one the key's `if-gen=` names.
            "key" if prompt.is_some() && line.ends_with(" ctrl+u") => {
                if line.contains(&format!(" if-gen=4.{generation} ")) {
                    if let Some(p) = prompt.as_mut() {
                        p.draft.clear();
                    }
                    generation += 1;
                    format!("OK seq={generation}")
                } else {
                    format!("OK skipped reason=changed seq={generation}")
                }
            }
            "send" | "key" => "OK seq=1".to_string(),
            // `signal term pid=<n>` as the server answers it: that
            // process is sent SIGTERM (the stand-in has no foreground
            // group to check it against), then `OK`.
            "signal" if !answers.signal.is_empty() => answers.signal.to_string(),
            "signal" => match line
                .split_whitespace()
                .find_map(|w| w.strip_prefix("pid="))
                .and_then(|p| p.parse::<i32>().ok())
            {
                Some(pid) if line.contains(" signal term ") => {
                    // SAFETY: a plain signal to the stand-in agent the
                    // test spawned and names by pid.
                    if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
                        "OK signal term".to_string()
                    } else {
                        "ERR no such process".to_string()
                    }
                }
                _ => "ERR usage".to_string(),
            },
            _ => "ERR unscripted".to_string(),
        }
    })
}

/// The requests of `asked` that type into the tab.
#[cfg(unix)]
fn turns(asked: &std::sync::Mutex<Vec<String>>) -> usize {
    asked
        .lock()
        .map_or(0, |a| a.iter().filter(|l| l.contains(" turn ")).count())
}

/// The initial roster is only a preliminary filter. If another live process
/// claims the conversation before the announce, the action's fresh read must
/// refuse the turn even though the earlier snapshot still names one owner.
#[cfg(unix)]
#[test]
fn stale_preliminary_roster_cannot_authorize_an_announcement() {
    let dir = scratch("stale-precheck-roster");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut a = parked_bare().spawn().expect("agent A");
    wait_exec(a.id());
    let sf_a = register(&opts.home, a.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let initial_files = session_files(&opts.home).expect("initial complete roster");
    assert!(unique_live_owner(&initial_files, &sf_a));

    let mut b = parked().spawn().expect("agent B");
    wait_exec(b.id());
    register(&opts.home, b.id(), SESSION);
    let st = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 77,
        seq_since_s: now_s() - 3600,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".to_string())];
    let args = atpkg::caller_shell::process_args(a.id()).expect("agent argv");
    let run = || {
        visit_with_claim(
            &opts,
            &sf_a,
            Some(&initial_files),
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
            Some(&args),
            None,
        )
    };
    SESSION_FILE_SCANS.with(|count| count.set(0));
    let blocked = run();
    let scans = SESSION_FILE_SCANS.with(std::cell::Cell::get);
    assert_eq!(blocked.step, "wait:conversation-owner-ambiguous");
    assert_eq!(scans, 1, "the act re-read ownership after the snapshot");
    assert_eq!(turns(&asked), 0, "the stale roster typed nothing");
    assert_eq!(load(&opts, SESSION), Some(st.clone()));

    b.kill().expect("stop agent B");
    b.wait().expect("reap agent B");
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{}.json", b.id())))
        .expect("remove B's session file");
    let allowed = run();
    assert_eq!(allowed.step, "announced:1", "the action path is live");
    assert_eq!(turns(&asked), 1);
    a.kill().expect("stop agent A");
    a.wait().expect("reap agent A");
    let _ = std::fs::remove_dir_all(dir);
}

/// The first PTY claim can be true and the tab can switch foreground groups
/// while the continuation reads its latest session file. The final claim must
/// veto typing, leaving the restart in flight for another sweep.
#[cfg(unix)]
#[test]
fn carry_on_rechecks_tab_ownership_at_the_turn() {
    let dir = scratch("carry-on-tab-race");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let mut reads = 0;
    let report = carry_on_with_tab_probe(
        &opts,
        blank(&sf),
        &mut st,
        &mut c,
        SESSION,
        &sf,
        MODEL_WAIT,
        |_, pid, tab| {
            assert_eq!((pid, tab), (sf.pid, TAB));
            reads += 1;
            reads == 1
        },
        None,
    );
    assert_eq!(
        reads, 2,
        "the second ownership read is immediately before turn"
    );
    assert_eq!(report.step, "wait:tab-ownership-changed");
    assert_eq!(turns(&asked), 0, "nothing typed into the changed tab");
    assert!(matches!(st.phase, Phase::Relaunched { .. }));
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// The continuation a relaunched agent is owed waits on a person's keystroke within the grace
/// (`wait:held`, nothing typed, the restart still in flight). Negative control: with nobody at
/// the keys it is typed, with `yield=0.2`.
#[cfg(unix)]
#[test]
fn carry_on_is_held_for_a_person_at_the_keys() {
    let dir = scratch("carry-on-held");
    let (sock, asked) = instance(&dir);
    std::fs::write(
        dir.join("t.status"),
        "OK schema=1 hold=0 hand=- human_ms=800\n",
    )
    .expect("status");
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let fresh = || St {
        phase: Phase::Relaunched { at_s: now_s() },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    let mut st = fresh();
    let mut c = connect(&opts, TAB).expect("control connection");
    let report = carry_on_with_tab_probe(
        &opts,
        blank(&sf),
        &mut st,
        &mut c,
        SESSION,
        &sf,
        MODEL_WAIT,
        |_, _, _| true,
        None,
    );
    assert_eq!(report.step, "wait:held");
    assert_eq!(turns(&asked), 0, "nothing typed under a person's hands");
    assert!(matches!(st.phase, Phase::Relaunched { .. }));
    // Negative control: nobody at the keys.
    std::fs::write(
        dir.join("t.status"),
        "OK schema=1 hold=0 hand=- human_ms=-\n",
    )
    .expect("status");
    let mut st = fresh();
    let _ = carry_on_with_tab_probe(
        &opts,
        blank(&sf),
        &mut st,
        &mut c,
        SESSION,
        &sf,
        MODEL_WAIT,
        |_, _, _| true,
        None,
    );
    assert_eq!(turns(&asked), 1);
    assert!(
        asked
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains(" turn ") && l.contains(" yield=0.2 ")),
        "{:?}",
        asked.lock().unwrap()
    );
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// The continuation is typed under the harness's hand: a later visit with no hand takes
/// one for the typing and gives it back at once; the relaunch's own wait passes the hand
/// it kept, neither taken nor given back a second time. Negative control: a tab another
/// driver holds is `wait:held`, nothing typed or given back.
#[cfg(unix)]
#[test]
fn the_continuation_is_typed_under_a_hand_taken_or_passed() {
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let run = |name: &str, lease: &'static str, keep: bool| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                lease,
                ..Answers::default()
            },
        );
        std::fs::write(
            dir.join("t.status"),
            "OK schema=1 hold=0 hand=- human_ms=-\n",
        )
        .expect("status");
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        let mut st = St {
            phase: Phase::Relaunched { at_s: now_s() },
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            ..St::default()
        };
        let mut c = connect(&opts, TAB).expect("control connection");
        // The relaunch's own hand, kept through the wait, when `keep`.
        let mut hand = keep.then(|| Hand::take(&mut c, TAB).expect("nobody else's"));
        let report = carry_on_with_tab_probe(
            &opts,
            blank(&sf),
            &mut st,
            &mut c,
            SESSION,
            &sf,
            MODEL_WAIT,
            |_, _, _| true,
            hand.as_mut(),
        );
        if let Some(hand) = hand.as_mut() {
            hand.give_back(&mut c);
        }
        agent.kill().expect("stop agent");
        agent.wait().expect("reap agent");
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        let _ = std::fs::remove_dir_all(dir);
        (report.step, asked)
    };
    let count = |asked: &[String], what: &str| asked.iter().filter(|l| l.contains(what)).count();
    let at = |asked: &[String], what: &str| asked.iter().position(|l| l.contains(what));
    // No caller holds a hand: one for the typing alone.
    let (step, asked) = run("continuation-own-hand", held, false);
    assert_eq!(step, "continued", "{asked:#?}");
    let (acquire, turn, release) = (
        at(&asked, " lease acquire ").expect("a hand taken"),
        at(&asked, " turn ").expect("the continuation"),
        at(&asked, " lease release ").expect("and given back"),
    );
    assert!(acquire < turn && turn < release, "{asked:#?}");
    assert!(asked[acquire].ends_with(" hard"), "{}", asked[acquire]);
    // The caller's hand: taken once (by the caller), given back once.
    let (step, asked) = run("continuation-passed-hand", held, true);
    assert_eq!(step, "continued", "{asked:#?}");
    assert_eq!(count(&asked, " lease acquire "), 1, "{asked:#?}");
    assert_eq!(count(&asked, " lease release "), 1, "{asked:#?}");
    let (turn, release) = (
        at(&asked, " turn ").expect("the continuation"),
        at(&asked, " lease release ").expect("given back"),
    );
    assert!(turn < release, "still held when typed: {asked:#?}");
    // Negative control: the tab is another driver's.
    let (step, asked) = run(
        "continuation-theirs",
        "ERR lease held holder=orchestrator expires_in_ms=5000",
        false,
    );
    assert_eq!(step, "wait:held", "{asked:#?}");
    assert_eq!(count(&asked, " turn "), 0, "{asked:#?}");
    assert_eq!(count(&asked, " lease release "), 0, "{asked:#?}");
}

/// [`type_relaunch_line`]'s check that no live process holds the conversation reads the
/// records it is given (the relaunched agent's own home): a holder under `<dir>` keeps the
/// line waiting (`conversation-owner-ambiguous`). Negative control: the user's
/// `~/.claude`, naming no process of `<dir>`'s, lets it type.
#[cfg(unix)]
#[test]
fn a_relaunch_line_waits_on_a_live_owner_under_its_agents_own_home() {
    let dir = scratch("relaunch-owner-home");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("an empty roster");
    let claude_dir = dir.join("work home/.claude");
    let home = claude_dir.to_string_lossy().into_owned();
    let mut owner = parked().spawn().expect("owner");
    wait_exec(owner.id());
    register_in(&claude_dir, owner.id(), SESSION);
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let mut line = |records: &Records| {
        type_relaunch_line(
            &opts,
            records,
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude --resume",
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
            },
            |_, _, _| true,
        )
    };
    assert_eq!(
        line(&Records::of(&opts.home, Some(&home))),
        Err(RelaunchLineError::Wait("conversation-owner-ambiguous"))
    );
    assert_eq!(turns(&asked), 0, "nothing typed over the live owner");
    line(&Records::of(&opts.home, None)).expect("the user's home names no owner");
    assert_eq!(turns(&asked), 1, "the negative control types");
    drop(c);
    let _ = owner.kill();
    let _ = owner.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A shell can leave the tab's foreground group during the complete session
/// scan. The second PTY claim is the one directly guarding the relaunch turn.
#[cfg(unix)]
#[test]
fn relaunch_rechecks_shell_tab_after_the_session_scan() {
    let dir = scratch("relaunch-tab-race");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let mut reads = 0;
    let result = type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
        },
        |_, pid, tab| {
            assert_eq!((pid, tab), (shell, TAB));
            reads += 1;
            reads == 1
        },
    );
    assert_eq!(reads, 2, "the shell's tab was checked again after the scan");
    assert_eq!(
        result,
        Err(RelaunchLineError::Wait("tab-ownership-changed"))
    );
    assert_eq!(turns(&asked), 0, "the moved shell received no relaunch");

    type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
        },
        |_, _, _| true,
    )
    .expect("unchanged tab can receive a relaunch");
    assert_eq!(turns(&asked), 1, "positive control reaches the turn");
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// The relaunch line is typed under the harness's hand ([`Hand`]), so a retrying orchestrator's
/// `turn` cannot slip in around it, for bash's `send` and zsh's fenced `turn` alike; a turn
/// refused `ERR busy …` is a wait, never a failed relaunch (its agent is already gone).
/// Negative control: a server with no hard lease gives no hand and nothing to give back.
#[cfg(unix)]
#[test]
fn the_relaunch_line_is_typed_under_the_hand_never_let_go() {
    let run = |name: &str, lease: &'static str, turn: &'static str, shell: u32| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                lease,
                turn: vec![turn],
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
        let mut c = connect(&opts, TAB).expect("control connection");
        let mut hand = Hand::take(&mut c, TAB).expect("nobody else's");
        let typed = type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude --resume",
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
            },
            |_, _, _| true,
        );
        hand.give_back(&mut c);
        drop(c);
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        let _ = std::fs::remove_dir_all(dir);
        (typed, asked)
    };
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let mut bash = stand_in_shell();
    for (name, shell, typing) in [
        ("line-zsh", std::process::id(), " turn "),
        ("line-bash", bash.id(), " send "),
    ] {
        let (typed, asked) = run(name, held, "OK status=done", shell);
        assert_eq!(typed, Ok(()), "{name}: {asked:#?}");
        assert!(
            asked[0].contains(" lease acquire ") && asked[0].ends_with(" hard"),
            "{name}: the hard hold first: {asked:#?}"
        );
        let line = asked
            .iter()
            .position(|l| l.contains(typing))
            .unwrap_or_else(|| panic!("{name}: the line: {asked:#?}"));
        assert!(
            !asked[1..=line].iter().any(|l| l.contains(" lease ")),
            "{name}: never let go for the line: {asked:#?}"
        );
        assert!(
            asked.last().is_some_and(|l| l.contains(" lease release ")),
            "{name}: given back at the end: {asked:#?}"
        );
    }
    let _ = bash.kill();
    let _ = bash.wait();
    // A turn refused busy: a wait, not a failed relaunch.
    let (typed, _) = run(
        "line-busy",
        held,
        "ERR busy lease=orchestrator",
        std::process::id(),
    );
    assert_eq!(typed, Err(RelaunchLineError::Wait("held")));
    // Negative control: an older server, no hard lease — nothing to give back.
    let (typed, asked) = run(
        "line-no-hand",
        "ERR lease acquire: unknown arg 'hard' (ttl=<ms> holder=<name>)",
        "OK status=done",
        std::process::id(),
    );
    assert_eq!(typed, Ok(()), "{asked:#?}");
    assert!(
        !asked.iter().any(|l| l.contains(" lease release ")),
        "{asked:#?}"
    );
}

/// The hand outlasts every wait under it (a slow `--resume` may take 90 s to
/// register, its first idle 15 s more): each wait keeps it ([`Hand::keep`]),
/// renewed once [`HAND_RENEW`] has passed since it was last taken, and not
/// before; a hand that holds nothing is never renewed.
#[cfg(unix)]
#[test]
fn the_hand_is_kept_through_every_wait_under_it() {
    let dir = scratch("hand-kept");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1",
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let acquires = || {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" lease acquire ")).count())
            .unwrap_or_default()
    };
    let mut hand = Hand::take(&mut c, TAB).expect("taken");
    assert_eq!(acquires(), 1);
    hand.keep(&mut c);
    assert_eq!(acquires(), 1, "fresh: not renewed");
    hand.renewed = Instant::now().checked_sub(HAND_RENEW).expect("clock");
    hand.keep(&mut c);
    assert_eq!(acquires(), 2, "due: renewed");
    assert!(hand.held);
    hand.keep(&mut c);
    assert_eq!(acquires(), 2, "renewed just now");
    let mut unheld = Hand {
        tab: TAB.to_string(),
        held: false,
        renewed: Instant::now().checked_sub(HAND_RENEW).expect("clock"),
    };
    unheld.keep(&mut c);
    assert_eq!(acquires(), 2, "a hand that holds nothing is never renewed");
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The last look before the relaunch line is a `status` read as it is typed: a person's
/// keystroke within the grace, a halt or anyone's hand holds the tab and nothing is typed.
/// Negative control: a keystroke older than the grace, no hand, no halt: the line is typed.
#[cfg(unix)]
#[test]
fn the_relaunch_line_is_never_typed_over_a_person_or_a_holder() {
    let dir = scratch("relaunch-held");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        human_grace_s: 120,
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let status = dir.join("t.status");
    for held in [
        "OK schema=1 hold=0 hand=- human_ms=800",
        "OK schema=1 hold=1 hand=- human_ms=-",
        "OK schema=1 hold=0 hand=turn:9 human_ms=-",
        "ERR no such session",
    ] {
        std::fs::write(&status, held).expect("status");
        let result = type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude",
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
            },
            |_, _, _| true,
        );
        assert_eq!(result, Err(RelaunchLineError::Wait("held")), "{held}");
        assert_eq!(turns(&asked), 0, "{held}: nothing typed");
    }
    std::fs::write(&status, "OK schema=1 hold=0 hand=- human_ms=120000").expect("status");
    type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude",
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
        },
        |_, _, _| true,
    )
    .expect("nobody at the tab");
    assert_eq!(turns(&asked), 1, "negative control: typed");
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// At a bash prompt the relaunch line is typed as raw bytes (`send --`, then Enter), never
/// pasted: a crashed agent leaves bracketed paste on and macOS's bash 3.2 reads the paste frame
/// as text (`--resume <id>01~`). Negative control: another shell is pasted to with a `turn`.
#[cfg(unix)]
#[test]
fn a_bash_prompt_is_typed_the_relaunch_line_never_pasted() {
    let dir = scratch("relaunch-bash");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let mut bash = stand_in_shell();
    let line = "claude --settings '{\"a\":1}' --resume x";
    for _ in 0..6_000 {
        if atpkg::caller_shell::process_args(bash.id())
            .is_some_and(|a| Dialect::from_exe_name(&a.exec_path) == Some(Dialect::Bash))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        bash.id(),
        TAB,
        SESSION,
        line,
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
        },
        |_, _, _| true,
    )
    .expect("typed at bash");
    let requests = asked.lock().expect("log").clone();
    assert!(
        requests
            .iter()
            .any(|r| r.ends_with(&format!("send -- {line}"))),
        "{requests:?}"
    );
    assert!(
        requests.iter().any(|r| r.ends_with(" key enter")),
        "{requests:?}"
    );
    assert_eq!(turns(&asked), 0, "never a paste");
    type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        std::process::id(),
        TAB,
        SESSION,
        line,
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
        },
        |_, _, _| true,
    )
    .expect("pasted elsewhere");
    assert_eq!(turns(&asked), 1, "negative control: pasted");
    let _ = bash.kill();
    let _ = bash.wait();
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// Every look at the job before an act stands between a ctrl-z and that act: the agent
/// backgrounded from the Nth read on stops the sweep at exactly one look, nothing typed or
/// signalled (the re-read before the announce or the SIGTERM, N = 1; the restart's gate,
/// N = 2; the last look before the signal, N = 3). Control: all foreground, announced.
#[cfg(unix)]
#[test]
fn every_look_at_the_job_stands_between_a_ctrl_z_and_the_act() {
    let dir = scratch("jobs");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    // An argv a resume carries (the filter alone: a positional, dropped).
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    // The READY answer to the announcement already typed.
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
        ) + "\n",
    )
    .expect("transcript");
    // The shell: a pid no process holds now, so its name comes from the table
    // the sweep is handed.
    let shell = dead_pid();
    let t = vec![(shell, 1, "zsh".to_string())];
    let now = now_s();
    let put = |phase: Phase| {
        let st = St {
            phase,
            marker: marker.to_string(),
            tab: TAB.to_string(),
            notice_pid: sf.pid,
            notice_start: squash(&sf.proc_start),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            last_seq: 77,
            seq_since_s: now - 3600,
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    };
    let run = |phase: Phase, foreground: usize| {
        put(phase);
        let k = Script::new(shell, foreground, None);
        let r = visit(&opts, &sf, &t, &newer(), &k);
        (r.step, k.reads.get())
    };
    let announced = Phase::Announced {
        at_s: now - 60,
        asks: 1,
    };
    let before_announce = run(Phase::Pending, 1);
    let typed_before = turns(&asked);
    let control = run(Phase::Pending, usize::MAX);
    let typed_control = turns(&asked);
    let before_signal = run(announced.clone(), 1);
    let restart_gate = run(announced.clone(), 2);
    let last_look = run(announced, 3);
    let typed_after = turns(&asked);
    let still_running = alive(pid);
    let phase = load(&opts, SESSION).map(|s| s.phase);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(before_announce, ("wait:not-foreground".to_string(), 2));
    assert_eq!(
        typed_before, 0,
        "nothing typed at a suspended agent's shell"
    );
    assert_eq!(control, ("announced:1".to_string(), 2), "the path is live");
    assert_eq!(typed_control, 1);
    assert_eq!(before_signal, ("wait:not-foreground".to_string(), 2));
    assert_eq!(restart_gate, ("wait:not-foreground".to_string(), 3));
    assert_eq!(last_look, ("wait:changed".to_string(), 4));
    assert_eq!(typed_after, 1, "nothing typed after the control");
    assert!(still_running, "never signalled");
    assert!(
        matches!(phase, Some(Phase::Announced { .. })),
        "never exiting: {phase:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- the relaunch

/// A changed directory stamp is noticed on the next 250 ms poll, while a
/// stable directory avoids reparsing all old JSON files. The one-second
/// fallback covers in-place JSON writes and a filesystem that misses a stamp.
#[test]
fn relaunch_roster_reuses_stable_files_but_rechecks_changes_and_in_place_writes() {
    let mut roster = SessionRosterCache::default();
    let now = Instant::now();
    let stamp = UNIX_EPOCH + Duration::from_secs(1);
    let scans = std::cell::Cell::new(0);
    let scan = || {
        scans.set(scans.get() + 1);
        Some(Vec::new())
    };

    assert!(roster.files_at(Some(stamp), now, scan).is_some());
    assert!(
        roster
            .files_at(Some(stamp), now + Duration::from_millis(250), scan)
            .is_some()
    );
    assert_eq!(
        scans.get(),
        1,
        "a quiet quarter-second poll reuses the roster"
    );
    assert!(
        roster
            .files_at(
                Some(stamp + Duration::from_secs(1)),
                now + Duration::from_millis(500),
                scan
            )
            .is_some()
    );
    assert_eq!(scans.get(), 2, "a new filename bypasses the fallback");
    assert!(
        roster
            .files_at(
                Some(stamp + Duration::from_secs(1)),
                now + Duration::from_millis(500) + SESSION_ROSTER_RESCAN,
                scan
            )
            .is_some()
    );
    assert_eq!(scans.get(), 3, "an in-place edit gets a bounded re-read");
}

#[test]
fn relaunch_roster_retries_an_incomplete_or_unstatable_directory_every_poll() {
    let mut roster = SessionRosterCache::default();
    let now = Instant::now();
    let stamp = Some(UNIX_EPOCH + Duration::from_secs(1));
    assert!(roster.files_at(stamp, now, || None).is_none());
    assert!(
        roster
            .files_at(stamp, now + Duration::from_millis(250), || Some(Vec::new()))
            .is_some(),
        "a transient malformed JSON must not be cached for a second"
    );
    let mut rescanned = false;
    assert!(
        roster
            .files_at(None, now + Duration::from_millis(500), || {
                rescanned = true;
                Some(Vec::new())
            })
            .is_some()
    );
    assert!(rescanned, "missing metadata cannot suppress a roster read");
}

/// Only the relaunch itself (the same conversation, a child of the shell the line was
/// typed at) is adopted and told it was upgraded.
#[cfg(unix)]
#[test]
fn only_the_relaunch_itself_is_adopted_and_told_it_was_upgraded() {
    let dir = scratch("adopt");
    let opts = drive(&dir);
    // Its parent is this process, standing in for the shell.
    let mut agent = parked().spawn().expect("spawn");
    let (pid, me, dead) = (agent.id(), std::process::id(), dead_pid());
    wait_exec(pid);
    let s = |session: &str| SessionFile {
        pid,
        session_id: session.to_string(),
        cwd: "/".to_string(),
        version: "9.9.9".to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 1,
        proc_start: String::new(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    };
    let fresh = "11111111-2222-3333-4444-555555555555";
    let adopted = [
        relaunch_of(&Live, &s(SESSION), SESSION, dead, me),
        // A person's fresh `claude` at that prompt after a relaunch that
        // failed: the shell's child, another conversation.
        relaunch_of(&Live, &s(fresh), SESSION, dead, me),
        // The conversation resumed by hand under another shell.
        relaunch_of(&Live, &s(SESSION), SESSION, dead, 1),
        // The process that was ended is never its own relaunch.
        relaunch_of(&Live, &s(SESSION), SESSION, pid, me),
    ];
    // End to end, the in-flight branch of a sweep: a process the recorded shell did not
    // start holds the conversation.
    let sf = register(&opts.home, pid, SESSION);
    let put = |shell: u32| {
        let st = St {
            phase: Phase::Relaunched { at_s: now_s() },
            pid: dead,
            shell,
            tab: TAB.to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    };
    put(1);
    let elsewhere = visit(&opts, &sf, &table(), &newer(), &Live);
    let failed = load(&opts, SESSION).map(|s| s.phase);
    // Control: the recorded shell is its parent: adopted, on to the tab.
    put(me);
    let ours = visit(&opts, &sf, &table(), &newer(), &Live);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(
        adopted,
        [
            Relaunch::Ours,
            Relaunch::NotOurs,
            Relaunch::NotOurs,
            Relaunch::NotOurs
        ]
    );
    assert_eq!(elsewhere.step, "failed:resumed-elsewhere", "{elsewhere:?}");
    assert_eq!(failed, Some(Phase::Failed("resumed-elsewhere".to_string())));
    assert_eq!(ours.step, "wait:no-socket", "{ours:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunch is judged from a parent read: a parent that cannot be read (the process
/// ended as it was asked, or `ps` could not run) is no verdict at all.
#[test]
fn a_parent_that_cannot_be_read_is_no_verdict_on_the_relaunch() {
    assert_eq!(relaunch_by_parent(Some(1916), 1916), Relaunch::Ours);
    assert_eq!(relaunch_by_parent(Some(1), 1916), Relaunch::NotOurs);
    assert_eq!(relaunch_by_parent(None, 1916), Relaunch::Unread);
    // No shell recorded: nothing can be the relaunch, read or not.
    assert_eq!(relaunch_by_parent(Some(0), 0), Relaunch::NotOurs);
    assert_eq!(relaunch_by_parent(None, 0), Relaunch::NotOurs);
    // The kernel's reader: a process that is gone has no parent to read.
    assert_eq!(Live.parent(dead_pid()), None);
    let me = std::process::id();
    assert_eq!(Live.parent(me), ids(me).map(|(ppid, _, _)| ppid));
}

/// The in-flight branch end to end: a new process holding the conversation fails the upgrade
/// for good only when its parent reads as another than the recorded shell; unread, it is a wait
/// (phase kept, nothing ledgered), so the real relaunch is still carried on.
#[cfg(unix)]
#[test]
fn a_relaunch_whose_parent_cannot_be_read_is_waited_for_never_failed() {
    let dir = scratch("unread");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let (dead, shell, at_s) = (dead_pid(), 4242, now_s());
    let st = St {
        phase: Phase::Relaunched { at_s },
        pid: dead,
        shell,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    let parent = |p: Option<u32>| Script::new(shell, usize::MAX, p);
    let unread = visit(&opts, &sf, &table(), &newer(), &parent(None));
    let kept = load(&opts, SESSION).map(|s| s.phase);
    let unread_lines = ledger_lines(&opts);
    // Control: read as the recorded shell, it is the relaunch — on to the tab.
    let ours = visit(&opts, &sf, &table(), &newer(), &parent(Some(shell)));
    // Read as another: the one permanent verdict.
    let other = visit(&opts, &sf, &table(), &newer(), &parent(Some(shell + 1)));
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(unread.step, "wait:ids", "{unread:?}");
    assert!(!unread.is_act());
    assert_eq!(kept, Some(Phase::Relaunched { at_s }));
    assert_eq!(unread_lines, 0);
    assert_eq!(ours.step, "wait:no-socket", "{ours:?}");
    assert_eq!(other.step, "failed:resumed-elsewhere", "{other:?}");
    assert_eq!(
        load(&opts, SESSION).map(|s| s.phase),
        Some(Phase::Failed("resumed-elsewhere".to_string()))
    );
    assert_eq!(ledger_lines(&opts), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunch stopped `no-resume` whose agent came up after all is that relaunch landed: the
/// host's `due` asks about it ([`needs_upgrade_inspection`]) and the visit reopens the record
/// (`reopened:relaunch-landed`, once) and carries it on. Negative controls, left standing: a
/// holder another shell started, a `stale-exit` stop, a relaunch on exit's stop.
#[cfg(unix)]
#[test]
fn a_relaunch_stopped_no_resume_whose_agent_came_up_is_carried_on() {
    let current = Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("1.0.0").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let me = std::process::id();
    let case = |name: &str, shell: u32, why: &str, cause: &str| {
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        let st = St {
            phase: Phase::Failed(why.to_string()),
            pid: dead_pid(),
            shell,
            tab: TAB.to_string(),
            from: "0.9.0".to_string(),
            to: "1.0.0".to_string(),
            source: "managed".to_string(),
            cause: cause.to_string(),
            quiet: true,
            exited_at: now_s(),
            relaunched_at: now_s(),
            // Stopped as [`St::fail`] stamps a stop: after the holder started.
            failed_at: now_s(),
            streak_why: why.to_string(),
            stop_streak: 1,
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        save(&opts, SESSION, &st);
        let asks = needs_upgrade_inspection(&sf, &current, Some(&st));
        let r = visit(&opts, &sf, &table(), &current, &Live);
        let _ = agent.kill();
        let _ = agent.wait();
        let after = load(&opts, SESSION).expect("record");
        let typed = turns(&asked);
        let reopened = ledger_details(&opts, "reopened:relaunch-landed").len();
        let _ = std::fs::remove_dir_all(&dir);
        (asks, r, after, typed, reopened)
    };
    let (asks, r, after, typed, reopened) = case("landed-late", me, "no-resume", "");
    assert!(asks, "the host's due asks about it");
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert_eq!(after.phase, Phase::Done, "{after:?}");
    assert_eq!(typed, 0, "a quiet move types nothing");
    assert_eq!(reopened, 1);
    assert_eq!(
        (after.stop_streak, after.streak_why.as_str()),
        (0, ""),
        "the stop taken back is no stop that repeats"
    );
    for (name, shell, why, cause) in [
        ("landed-elsewhere", 1, "no-resume", ""),
        ("landed-stale", me, "stale-exit", ""),
        ("landed-exit", me, "no-resume", CAUSE_EXIT),
    ] {
        let (asks, r, after, typed, reopened) = case(name, shell, why, cause);
        assert!(!asks, "{name}");
        assert_eq!(r.step, "current", "{name}: {r:?}");
        assert_eq!(after.phase, Phase::Failed(why.to_string()), "{name}");
        assert_eq!((typed, reopened), (0, 0), "{name}");
        assert_eq!(after.stop_streak, 1, "{name}: its stop stands");
    }
}

/// A conversation resumed by hand after a `no-resume` stop is the person's, not a relaunch
/// that landed late, though the stop's own remedy (`claude --resume <id>` in that tab) starts
/// a child of the recorded shell as the line's agent would be: only a holder that started no
/// later than the stop, on the build the line ran, can be the line's agent
/// ([`St::landed_after_stop`]). Each case leaves the stop standing: nothing asks about it,
/// nothing is owed, nothing typed.
#[cfg(unix)]
#[test]
fn a_conversation_resumed_by_hand_after_a_no_resume_stop_is_never_reopened() {
    use super::super::relaunch::CAUSE_MEMORY;
    const DAY: u64 = 86_400;
    let me = std::process::id();
    // (name, cause, the build the line ran, quiet, how long before the
    // holder started the stop was said); the sweep knows the line's build.
    let cases = [
        // A restart in place's stop, the conversation resumed by hand a day
        // later: its continuation would be typed.
        ("hand-memory", CAUSE_MEMORY, "1.0.0", false, DAY),
        // An announced upgrade's stop, resumed by hand on the target a day
        // later: its continuation would be typed.
        ("hand-announced", "", "1.0.0", false, DAY),
        // A quiet upgrade to 9.9.9 whose conversation runs on the old build
        // (1.0.0) in a process of the recorded shell: not the line's agent,
        // whenever it started.
        ("hand-old-build", "", "9.9.9", true, 0),
    ];
    for (name, cause, to, quiet, before) in cases {
        let targets = Targets {
            managed: Some(Candidate {
                exe: PathBuf::from("/nonexistent/claude"),
                version: Version::parse(to).expect("version"),
                source: Source::Managed,
            }),
            native: None,
            unanswered: false,
        };
        let dir = scratch(name);
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        let stopped = now_s() - before;
        let st = St {
            phase: Phase::Failed("no-resume".to_string()),
            pid: dead_pid(),
            shell: me,
            tab: TAB.to_string(),
            from: "1.0.0".to_string(),
            to: to.to_string(),
            source: "managed".to_string(),
            cause: cause.to_string(),
            quiet,
            exited_at: stopped,
            relaunched_at: stopped,
            failed_at: stopped,
            streak_why: "no-resume".to_string(),
            stop_streak: 1,
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        save(&opts, SESSION, &st);
        if to == sf.version {
            assert!(
                !needs_upgrade_inspection(&sf, &targets, Some(&st)),
                "{name}: nothing asks about a current agent's standing stop"
            );
        }
        let r = visit(&opts, &sf, &table(), &targets, &Live);
        let after = load(&opts, SESSION).expect("record");
        let owes = owed(&opts);
        let _ = agent.kill();
        let _ = agent.wait();
        let typed = turns(&asked);
        let reopened = ledger_details(&opts, "reopened:relaunch-landed").len();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(reopened, 0, "{name}: {r:?}");
        assert_eq!(
            after.phase,
            Phase::Failed("no-resume".to_string()),
            "{name}: {r:?}"
        );
        assert!(!owes, "{name}: no continuation is owed");
        assert_eq!(typed, 0, "{name}: nothing typed");
    }
}

/// A build that failed to start for a conversation is not tried again for its hand
/// resume: the new process's round keeps the stop's rest and streak, bound to it, until
/// the rest runs out, also after a restart in place of the hand resume
/// ([`St::kept_by_restart`]). Controls: past the rest the round re-arms; a stop on an
/// older target holds no newer one back.
#[cfg(unix)]
#[test]
fn a_build_that_failed_to_start_is_not_retried_for_the_conversations_hand_resume() {
    // The stopped round, of the process the upgrade ended (not the one the
    // person started since), `ago` seconds after its stop, its target `to`.
    let stopped = |to: &'static str, ago: u64| {
        move |opts: &Opts| {
            let at = now_s() - ago;
            let st = St {
                phase: Phase::Failed("no-resume".to_string()),
                pid: dead_pid(),
                shell: dead_pid(),
                tab: TAB.to_string(),
                from: "1.0.0".to_string(),
                to: to.to_string(),
                source: "managed".to_string(),
                proc_start: "Thu Oct 1 00:00:00 2026".to_string(),
                salt: at - 600,
                pending_since: at - 600,
                exited_at: at - 5,
                relaunched_at: at - 4,
                failed_at: at,
                streak_why: "no-resume".to_string(),
                stop_streak: 1,
                ..St::default()
            };
            std::fs::write(state_path(opts, SESSION), st.to_json()).expect("the stop");
        }
    };
    let shell = dead_pid();
    let visit = |name: &str, seed: &dyn Fn(&Opts)| {
        quietly(|| {
            visit_launched(
                name,
                &[
                    user_row("fix the bug"),
                    turn_by("claude-haiku-4-5", "Fixed."),
                ],
                false,
                &Script::new(shell, usize::MAX, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                seed,
            )
        })
    };
    // 34 s after the stop, the hand resume holds the conversation, idle.
    let (r, saved, typed) = visit("hand-resume-rests", &stopped("9.9.9", 34));
    let saved = saved.expect("the record");
    assert_eq!(r.step, "wait:failed", "{r:?}");
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "nothing typed");
    assert_eq!(
        saved.phase,
        Phase::Failed("no-resume".to_string()),
        "the round rests: {saved:?}"
    );
    assert_eq!(
        (saved.streak_why.as_str(), saved.stop_streak),
        ("no-resume", 1),
        "the streak the next stop of this build lengthens"
    );
    assert_ne!(
        saved.proc_start, "Thu Oct 1 00:00:00 2026",
        "bound to the process that holds the conversation now"
    );
    assert_eq!(
        saved.exited_at, 0,
        "no agent of this record is down: the person's runs"
    );
    // The hand resume restarted in place on its own program (a memory
    // banner's restart), landed 10 minutes into the rest: its record over the
    // stop carries it ([`St::kept_by_restart`]), and the round still rests.
    let restarted = |opts: &Opts| {
        use super::super::relaunch::CAUSE_MEMORY;
        stopped("9.9.9", 600)(opts);
        let stop = load(opts, SESSION).expect("the stop");
        let mut restart = St {
            phase: Phase::Done,
            from: "1.0.0".to_string(),
            to: "1.0.0".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: CAUSE_MEMORY.to_string(),
            tab: TAB.to_string(),
            salt: now_s() - 60,
            done_at: now_s() - 30,
            ..St::default()
        };
        stop.kept_by_restart(TAB, &mut restart);
        std::fs::write(state_path(opts, SESSION), restart.to_json()).expect("the restart");
    };
    let (r, saved, typed) = visit("restarted-in-place-rests", &restarted);
    let saved = saved.expect("the record");
    assert_eq!(r.step, "wait:failed", "{r:?}");
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "nothing typed");
    assert_eq!(
        (
            saved.phase.clone(),
            saved.streak_why.as_str(),
            saved.stop_streak
        ),
        (Phase::Failed("no-resume".to_string()), "no-resume", 1),
        "the round rests after the restart in place: {saved:?}"
    );
    // Control: the rest over, the round re-arms onto the same build.
    let (r, saved, _) = visit(
        "hand-resume-rested",
        &stopped("9.9.9", upgrade::RETRY_S + 60),
    );
    assert_eq!(r.step, "rearmed:no-resume", "{r:?}");
    assert_eq!(saved.map(|st| st.phase), Some(Phase::Pending));
    // Control: a stop on an older target holds no newer build back: a new
    // round, pending (its screen yet to settle for this record).
    let (r, saved, _) = visit("hand-resume-newer", &stopped("9.9.8", 34));
    assert_eq!(r.step, "wait:settling", "{r:?}");
    assert_eq!(saved.map(|st| st.phase), Some(Phase::Pending));
}

/// A conversation's relaunch onto `to` (managed) that stopped `no-resume`
/// `ago` seconds ago, as the relaunch saves it ([`save`], the one writer):
/// the round of the process the upgrade ended, in another tab.
#[cfg(unix)]
fn failed_start_saved(opts: &Opts, session: &str, to: &str, ago: u64) {
    let at = now_s() - ago;
    let st = St {
        phase: Phase::Failed("no-resume".to_string()),
        pid: dead_pid(),
        shell: dead_pid(),
        tab: "s-0th3r0th3r0th3r0th3r".to_string(),
        from: "1.0.0".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        proc_start: "Thu Oct 1 00:00:00 2026".to_string(),
        salt: at - 600,
        pending_since: at - 600,
        exited_at: at - 5,
        relaunched_at: at - 4,
        failed_at: at,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    // Saved as a real visit saves it: the dry run's own options write nothing.
    let writes = Opts {
        dry_run: false,
        ..opts.clone()
    };
    save(&writes, session, &st);
}

/// A build that failed to start is the build's fact, not only its conversation's: no
/// agent (another conversation's, or the stopped one's own round past its rest) is moved
/// onto it until it is proved to start without one; here its probe cannot run it, so the
/// quiet restart waits `wait:build-fails`. Control: a stop on an older build holds no
/// newer one back.
#[cfg(unix)]
#[test]
fn a_build_that_failed_to_start_for_any_conversation_moves_no_agent_onto_it() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let visit = |name: &str, seed: &dyn Fn(&Opts)| {
        quietly(|| {
            visit_launched(
                name,
                &[
                    user_row("fix the bug"),
                    turn_by("claude-haiku-4-5", "Fixed."),
                ],
                true,
                &Script::new(dead_pid(), usize::MAX, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                seed,
            )
        })
    };
    // Another conversation's relaunch onto 9.9.9 stopped 34 s ago; this agent's own round
    // is pending, quiet, at an idle point.
    let (r, _, typed) = visit("another-conversations-stop", &|opts| {
        failed_start_saved(opts, OTHER, "9.9.9", 34);
    });
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "nothing typed");
    assert_eq!(r.step, "wait:build-fails", "another conversation: {r:?}");
    // This conversation's own stop, its rest run out and its round re-armed: the build is
    // no better for the rest.
    let (r, _, _) = visit("its-own-stop-rested", &|opts| {
        failed_start_saved(opts, SESSION, "9.9.9", upgrade::RETRY_S + 60);
        let writes = Opts {
            dry_run: false,
            ..opts.clone()
        };
        let mut st = load(&writes, SESSION).expect("the stop");
        st.rearm(now_s());
        // Re-armed for the process that holds the conversation (the stop
        // was kept bound to it, `St::keep_stop`), behind since long before.
        let holder = session_files(&opts.home)
            .and_then(|files| files.into_iter().find(|sf| sf.session_id == SESSION))
            .expect("the agent's record");
        st.proc_start = squash(&holder.proc_start);
        st.tab = TAB.to_string();
        st.last_seq = 77;
        st.seq_since_s = now_s() - 3600;
        save(&writes, SESSION, &st);
    });
    assert_eq!(r.step, "wait:build-fails", "its own rested round: {r:?}");
    // Control: a stop on an older build holds no newer one back.
    let (r, _, _) = visit("an-older-builds-stop", &|opts| {
        failed_start_saved(opts, OTHER, "9.9.8", 34);
    });
    assert_eq!(r.step, "would-restart", "{r:?}");
}

/// `build_start::note_failure`, from [`save_at`], notes a `no-resume` stop of a relaunch
/// onto a build once: the record saved again, a new process keeping the stop
/// ([`St::keep_stop`]) and a restart's record carrying it are the same failure; another
/// conversation's stop is another. Negative controls, none noted: another reason, a
/// relaunch that kept its program, a Codex record, a dry run.
#[cfg(unix)]
#[test]
fn a_failed_start_is_noted_against_its_build_once_by_the_one_writer() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let dir = scratch("build-note");
    let opts = drive(&dir);
    let failures =
        |opts: &Opts| build_start::load(opts, "managed", "9.9.9").map_or(0, |note| note.failures);
    failed_start_saved(&opts, SESSION, "9.9.9", 34);
    assert_eq!(failures(&opts), 1, "the stop is the build's failure");
    let stop = load(&opts, SESSION).expect("the stop");
    save(&opts, SESSION, &stop);
    assert_eq!(failures(&opts), 1, "saved again: the same failure");
    let mut kept = St {
        phase: Phase::Pending,
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    kept.keep_stop(stop.failed_start().expect("a failed start"));
    kept.source = "managed".to_string();
    save(&opts, SESSION, &kept);
    assert_eq!(failures(&opts), 1, "a hand resume that keeps the stop");
    let mut restart = St {
        phase: Phase::Done,
        from: "1.0.0".to_string(),
        to: "1.0.0".to_string(),
        source: SAME_PROGRAM.to_string(),
        ..St::default()
    };
    stop.kept_by_restart(TAB, &mut restart);
    save(&opts, SESSION, &restart);
    assert_eq!(failures(&opts), 1, "a restart's record that carries it");
    failed_start_saved(&opts, OTHER, "9.9.9", 10);
    assert_eq!(failures(&opts), 2, "another conversation's stop");
    // Negative controls.
    let unnoted = |name: &str, st: St, opts: &Opts| {
        save(opts, OTHER, &st);
        assert_eq!(
            build_start::load(opts, &st.source, &st.to),
            None,
            "{name}: no failure to start"
        );
    };
    let failed = |why: &str, source: &str, agent: upgrade::Agent| St {
        phase: Phase::Failed(why.to_string()),
        from: "1.0.0".to_string(),
        to: "9.9.7".to_string(),
        source: source.to_string(),
        failed_at: now_s(),
        agent,
        ..St::default()
    };
    unnoted(
        "another reason",
        failed("resumed-elsewhere", "managed", upgrade::Agent::Claude),
        &opts,
    );
    unnoted(
        "its own program",
        failed("no-resume", SAME_PROGRAM, upgrade::Agent::Claude),
        &opts,
    );
    unnoted(
        "a Codex",
        failed("no-resume", "managed", upgrade::Agent::Codex),
        &opts,
    );
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    unnoted(
        "a dry run",
        failed("no-resume", "managed", upgrade::Agent::Claude),
        &dry,
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The gate ([`build_start::starts`]): a build with no failure noted is taken without a
/// probe; one with a failure only once a probe run since passes (a failed probe is not
/// rerun within [`build_start::PROBE_GAP_S`]); a move failing after a passing probe leaves
/// it blind. Each probe is one ledger row (`build-probe:<what>`); a dry run runs none.
#[cfg(unix)]
#[test]
fn a_build_that_failed_to_start_is_moved_onto_only_once_proved_to_start() {
    use build_start::{BUILD_FAILS, PROBE_GAP_S, Probe, starts_with};
    let dir = scratch("build-gate");
    let opts = drive(&dir);
    let to = Candidate {
        exe: dir.join("claude"),
        version: Version::parse("9.9.9").expect("v"),
        source: Source::Managed,
    };
    let r = Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: String::new(),
    };
    let never = |_: &Path| -> Probe { panic!("no probe is run") };
    let now = now_s();
    // No failure noted: taken, nothing run.
    assert_eq!(starts_with(&opts, &r, &to, now, false, never), Ok(()));
    failed_start_saved(&opts, SESSION, "9.9.9", 34);
    // A dry run runs nothing and moves nothing.
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(
        starts_with(&dry, &r, &to, now, false, never),
        Err(BUILD_FAILS)
    );
    // The probe exits: no move, and not probed again inside the gap.
    let ran = std::cell::Cell::new(0);
    let exits = |_: &Path| {
        ran.set(ran.get() + 1);
        Probe::Exited
    };
    assert_eq!(
        starts_with(&opts, &r, &to, now, false, exits),
        Err(BUILD_FAILS)
    );
    assert_eq!(ran.get(), 1);
    assert_eq!(
        starts_with(&opts, &r, &to, now + PROBE_GAP_S - 1, false, never),
        Err(BUILD_FAILS),
        "inside the gap"
    );
    assert_eq!(ledger_details(&opts, "build-probe:exited").len(), 1);
    // Past the gap it is probed again; it starts: moved onto, and taken
    // without a probe after.
    assert_eq!(
        starts_with(&opts, &r, &to, now + PROBE_GAP_S, false, |_| Probe::Starts),
        Ok(())
    );
    assert_eq!(
        starts_with(&opts, &r, &to, now + PROBE_GAP_S + 1, false, never),
        Ok(())
    );
    assert_eq!(ledger_details(&opts, "build-probe:starts").len(), 1);
    // A move onto it made after that proof fails to start again: blind.
    let mut again = St {
        phase: Phase::Failed("no-resume".to_string()),
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        relaunched_at: now + PROBE_GAP_S + 5,
        failed_at: now + PROBE_GAP_S + 9,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    save_at(
        &opts,
        "0badf00d-7777-2222-3333-444455556666",
        &again,
        now + PROBE_GAP_S + 9,
    );
    let note = build_start::load(&opts, "managed", "9.9.9").expect("the note");
    assert!(note.blind, "{note:?}");
    assert_eq!(ledger_details(&opts, "build-probe:blind").len(), 1);
    assert_eq!(
        starts_with(&opts, &r, &to, now + 10 * PROBE_GAP_S, false, never),
        Err(BUILD_FAILS),
        "no probe proves a blind build again"
    );
    // Control: a move that began before the proof and failed after it is no
    // verdict on the probe: another build's note, proved, then that failure.
    let older = Candidate {
        version: Version::parse("9.9.8").expect("v"),
        ..to.clone()
    };
    failed_start_saved(&opts, SESSION, "9.9.8", 34);
    assert_eq!(
        starts_with(&opts, &r, &older, now, false, |_| Probe::Starts),
        Ok(())
    );
    again.to = "9.9.8".to_string();
    again.relaunched_at = now - 1;
    save_at(
        &opts,
        "0badf00d-8888-2222-3333-444455556666",
        &again,
        now + 1,
    );
    let note = build_start::load(&opts, "managed", "9.9.8").expect("the note");
    assert!(!note.blind, "{note:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`build_start::probe`] runs the build as its relaunch would, without an agent:
/// `<build> --help` exiting 0 starts (`--version` proves nothing: a build that crashes at
/// launch still names its version); exit 1, no exec, and a hang killed at the bound do not.
#[cfg(unix)]
#[test]
fn the_start_probe_runs_the_build_with_its_help_flag_bounded() {
    use build_start::{Probe, probe, probe_within};
    let dir = scratch("build-probe");
    let crashing = dir.join("crashing");
    sh_script(
        &crashing,
        "case \"$1\" in --version) echo \"2.1.999 (Claude Code)\"; exit 0 ;; esac\nexit 1",
    );
    let starts = dir.join("starts");
    sh_script(
        &starts,
        "case \"$1\" in --help) echo usage; exit 0 ;; esac\nexit 1",
    );
    let hangs = dir.join("hangs");
    sh_script(&hangs, "sleep 600");
    assert_eq!(
        version_of(&crashing),
        Probed::Version(Version::parse("2.1.999").expect("v"))
    );
    assert_eq!(
        probe(&crashing),
        Probe::Exited,
        "a build that crashes at start"
    );
    assert_eq!(probe(&starts), Probe::Starts);
    assert_eq!(probe(&dir.join("absent")), Probe::NoExec);
    assert_eq!(
        probe_within(&hangs, Duration::from_millis(300)),
        Probe::TimedOut
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A real visit waits on a build that failed to start ([`build_starts`] at the act): the
/// quiet restart of an idle agent onto a build another conversation's stop noted failed is
/// a recorded wait (`wait:build-fails`), nothing typed. Control: a build a probe proved
/// since is moved onto.
#[cfg(unix)]
#[test]
fn a_real_visit_waits_on_a_build_that_failed_to_start_and_says_it_once() {
    const OTHER: &str = "0badf00d-9999-2222-3333-444455556666";
    let rows = [
        user_row("fix the bug"),
        turn_by("claude-haiku-4-5", "Fixed."),
    ];
    let seen = std::cell::RefCell::new(None);
    let (r, saved, typed) = quietly(|| {
        visit_launched(
            "build-fails-visit",
            &rows,
            false,
            &Script::new(dead_pid(), usize::MAX, None),
            77,
            None,
            None,
            Answers::default(),
            &[],
            &|opts| {
                failed_start_saved(opts, OTHER, "9.9.9", 34);
                *seen.borrow_mut() = Some(opts.clone());
            },
        )
    });
    let _ = seen;
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "nothing typed");
    assert_eq!(r.step, "wait:build-fails", "{r:?}");
    let saved = saved.expect("the record");
    assert_eq!(saved.phase, Phase::Pending);
    assert_eq!(saved.wait, BUILD_FAILS);
    // Control: proved since its failure, the build is moved onto.
    let (r, _, _) = quietly(|| {
        visit_launched(
            "build-proved-visit",
            &rows,
            true,
            &Script::new(dead_pid(), usize::MAX, None),
            77,
            None,
            None,
            Answers::default(),
            &[],
            &|opts| {
                failed_start_saved(opts, OTHER, "9.9.9", 34);
                let writes = Opts {
                    dry_run: false,
                    ..opts.clone()
                };
                let to = newer().managed.expect("managed");
                let r = blank(&record("1.0.0"));
                assert_eq!(
                    build_start::starts_with(&writes, &r, &to, now_s(), false, |_| {
                        build_start::Probe::Starts
                    }),
                    Ok(())
                );
            },
        )
    });
    assert_eq!(r.step, "would-restart", "{r:?}");
}

/// A READY given for a move onto a build that fails to start stops the round
/// (`failed:build-fails`, [`upgrade::build_hold`]), its markers forgotten, and releases the
/// agent once in the same visit (the ask flow); its ledger row says why, once. A dry read
/// says `would-fail:build-fails` and writes nothing.
#[cfg(unix)]
#[test]
fn a_ready_for_a_build_that_failed_to_start_stops_the_round_and_releases_its_agent() {
    let rig = |name: &str| {
        let h = Parked::new(name, Answers::default(), settled(), 3600);
        asked_then_answered(&h, ASKED_MARKER, &format!("Saved.\\n{ASKED_MARKER}"));
        let st = St {
            markers: vec![ASKED_MARKER.to_string()],
            last_seq: 77,
            seq_since_s: now_s() - 3600,
            ..announced_to(&h, ASKED_MARKER)
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h
    };
    let mut dry = rig("ready-build-fails-dry");
    dry.opts.dry_run = true;
    failed_start_saved(&dry.opts, ANOTHER_CONVERSATION, "9.9.9", 34);
    assert_eq!(dry.visit().step, "would-fail:build-fails");
    assert!(dry.typed().is_empty() && dry.details("failed:build-fails").is_empty());
    drop(dry);
    let h = rig("ready-build-fails");
    let (steps, releases, after) = onto_a_failing_build(&h);
    assert_eq!(steps[0], "released:build-fails", "{steps:?}");
    assert_eq!(releases, 1, "{steps:?}");
    assert_eq!(after.phase, Phase::Failed(BUILD_FAILS.to_string()));
    assert_eq!(
        after.round_markers().count(),
        0,
        "the answer void: {after:?}"
    );
    assert!(after.release.is_empty(), "{after:?}");
    let why = h.details("failed:build-fails");
    assert_eq!(why.len(), 1, "said once: {why:?}");
    assert!(
        why[0].contains("failed to start") && why[0].contains("owed its release"),
        "{why:?}"
    );
}

/// The conversation whose relaunch onto a build found it failing to start,
/// in another tab ([`failed_start_saved`]).
#[cfg(unix)]
const ANOTHER_CONVERSATION: &str = "0badf00d-9999-2222-3333-444455556666";

/// The READY marker of the round the tests below asked.
#[cfg(unix)]
const ASKED_MARKER: &str = "ATERM-UPGRADE-READY-0badf00d";

/// `h`'s agent, its round as the test left it, visited three times after
/// [`ANOTHER_CONVERSATION`]'s relaunch onto 9.9.9 failed to start (34 s
/// ago; no probe can run `/nonexistent/claude`): each visit's step, how
/// many release lines were typed into the tab, and the record after.
#[cfg(unix)]
fn onto_a_failing_build(h: &Parked) -> (Vec<String>, usize, St) {
    failed_start_saved(&h.opts, ANOTHER_CONVERSATION, "9.9.9", 34);
    let steps = (0..3).map(|_| h.visit().step).collect();
    let releases = h
        .typed()
        .iter()
        .filter(|t| t.contains(upgrade::RELEASE_HEAD.trim_end()))
        .count();
    (steps, releases, load(&h.opts, SESSION).expect("the record"))
}

/// A given-up round's late READY onto a build that fails to start is released, once, its
/// answer void: the build's gate holds the restart and no re-arm is due while a READY
/// stands, so nothing else would release the agent. Control: with no failure noted, the
/// READY restarts it.
#[cfg(unix)]
#[test]
fn a_late_ready_onto_a_build_that_fails_to_start_is_released() {
    let rig = |name: &str| {
        let h = Parked::new(name, Answers::default(), settled(), 3600);
        asked_then_answered(&h, ASKED_MARKER, &format!("Done.\\n{ASKED_MARKER}"));
        let mut st = St {
            markers: vec![ASKED_MARKER.to_string()],
            last_seq: 77,
            seq_since_s: now_s() - 3600,
            ..announced_to(&h, ASKED_MARKER)
        };
        st.give_up(now_s() - 600);
        assert_eq!(st.release, "gave-up");
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h
    };
    let mut control = rig("late-ready-onto-a-build");
    control.opts.dry_run = true;
    assert_eq!(control.visit().step, "would-restart");
    drop(control);
    let h = rig("late-ready-onto-a-failing-build");
    let (steps, releases, after) = onto_a_failing_build(&h);
    assert_eq!(releases, 1, "released once: {steps:?}");
    assert!(after.release.is_empty(), "nothing owed: {after:?}");
    assert_eq!(
        after.round_markers().count(),
        0,
        "its READY void: {after:?}"
    );
}

/// A pending round owing its agent a release (a notice abandoned by a retarget) types it,
/// once, in both flows while its build fails to start: the gate's `wait:build-fails`
/// holds the next notice or restart, so nothing else tells the agent the upgrade is off.
/// Control: with no failure noted, the notice (ask flow) or the restart (quiet flow) goes.
#[cfg(unix)]
#[test]
fn a_pending_round_owing_a_release_types_it_while_its_build_fails_to_start() {
    for quiet in [false, true] {
        let rig = |name: &str| {
            let park = || Parked::new(name, Answers::default(), settled(), 3600);
            let h = if quiet { quietly(park) } else { park() };
            asked_then_answered(&h, ASKED_MARKER, &format!("Done.\\n{ASKED_MARKER}"));
            let st = St {
                phase: Phase::Pending,
                release: "retargeted".to_string(),
                asked: vec![ASKED_MARKER.to_string()],
                notice_pid: h.sf.pid,
                notice_start: squash(&h.sf.proc_start),
                pending_since: now_s() - 3 * 3600,
                ..settled()
            };
            std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
            h
        };
        let mut control = rig(&format!("owed-onto-a-build-{quiet}"));
        control.opts.dry_run = true;
        let would = if quiet {
            "would-restart"
        } else {
            "would-announce"
        };
        assert_eq!(control.visit().step, would, "quiet={quiet}");
        drop(control);
        let h = rig(&format!("owed-onto-a-failing-build-{quiet}"));
        let (steps, releases, after) = onto_a_failing_build(&h);
        assert_eq!(releases, 1, "quiet={quiet}: released once: {steps:?}");
        assert!(after.release.is_empty(), "quiet={quiet}: {after:?}");
        assert_eq!(after.phase, Phase::Pending, "quiet={quiet}");
        assert_eq!(after.wait, BUILD_FAILS, "quiet={quiet}");
    }
}

/// An announced round whose build fails to start releases its agent, once: the agent
/// wound down and answered "not yet", and the re-ask the build's gate holds
/// (`wait:build-fails`) would leave it wound down for as long as the build fails.
/// Control: with no failure noted, the re-ask goes.
#[cfg(unix)]
#[test]
fn an_announced_round_whose_build_fails_to_start_releases_its_agent() {
    let rig = |name: &str| {
        let h = Parked::new(name, Answers::default(), settled(), 3600);
        asked_then_answered(&h, ASKED_MARKER, "Not yet: the suite is still running.");
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - upgrade::REASK_S - 60,
                asks: 1,
            },
            markers: vec![ASKED_MARKER.to_string()],
            last_seq: 77,
            seq_since_s: now_s() - 3600,
            ..announced_to(&h, ASKED_MARKER)
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h
    };
    let mut control = rig("announced-onto-a-build");
    control.opts.dry_run = true;
    assert_eq!(control.visit().step, "would-announce");
    drop(control);
    let h = rig("announced-onto-a-failing-build");
    let (steps, releases, after) = onto_a_failing_build(&h);
    assert_eq!(releases, 1, "released once: {steps:?}");
    assert!(after.release.is_empty(), "nothing owed: {after:?}");
    assert_eq!(after.round_markers().count(), 0, "{after:?}");
}

/// A stop whose relaunch came up after all (`reopened:relaunch-landed`,
/// [`St::reopen_landed`]) is taken back from its build's note too, so the build is no
/// longer blind: only the other conversation's failure stands, which a probe proved since.
#[cfg(unix)]
#[test]
fn a_failure_whose_relaunch_came_up_after_all_is_taken_back_from_its_build() {
    let current = Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("1.0.0").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let to = current.managed.clone().expect("the build");
    let dir = scratch("landed-late-taken-back");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    // Another conversation's relaunch onto 1.0.0 failed to start; a probe
    // passed a minute ago.
    failed_start_saved(&opts, ANOTHER_CONVERSATION, "1.0.0", 600);
    let r = blank(&sf);
    let probed = now_s() - 60;
    assert_eq!(
        build_start::starts_with(&opts, &r, &to, probed, false, |_| {
            build_start::Probe::Starts
        }),
        Ok(())
    );
    // This conversation's relaunch, moved since, stopped `no-resume`: blind.
    let st = St {
        phase: Phase::Failed("no-resume".to_string()),
        pid: dead_pid(),
        shell: std::process::id(),
        tab: TAB.to_string(),
        from: "0.9.0".to_string(),
        to: "1.0.0".to_string(),
        source: "managed".to_string(),
        quiet: true,
        exited_at: now_s(),
        relaunched_at: now_s(),
        failed_at: now_s(),
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let blind = build_start::load(&opts, "managed", "1.0.0").expect("the note");
    assert!(blind.blind, "{blind:?}");
    let r = visit(&opts, &sf, &table(), &current, &Live);
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(r.step, "done:resumed", "{r:?}");
    let note = build_start::load(&opts, "managed", "1.0.0").expect("the note");
    let moves = build_start::starts_with(&opts, &r, &to, now_s(), false, |_| {
        panic!("proved since the failure that stands: no probe")
    });
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!note.blind, "{note:?}");
    assert_eq!(note.failures, 1, "the other conversation's alone: {note:?}");
    assert_eq!(moves, Ok(()));
}

/// The owner's `--now` probes a blind build again (one a move failed on after a passing
/// probe, which no probe proves again): a build that starts is moved onto, its note proved
/// again. Control: without the owner's word the blind build is not probed.
#[cfg(unix)]
#[test]
fn the_owners_now_probes_a_blind_build_again() {
    let rig = |name: &str, now_word: bool| {
        // Behind for hours: the owner has been told ([`upgrade::Facts::unnoticed`]).
        let behind = St {
            pending_since: now_s() - 3 * 3600,
            ..settled()
        };
        let h = quietly(|| Parked::new(name, Answers::default(), behind, 3600));
        // A build whose `--help` runs and exits 0.
        let exe = h.dir.join("claude-9.9.9");
        sh_script(&exe, "case \"$1\" in --help) exit 0 ;; esac\nexit 1");
        let targets = Targets {
            managed: Some(Candidate {
                exe,
                version: Version::parse("9.9.9").expect("version"),
                source: Source::Managed,
            }),
            native: None,
            unanswered: false,
        };
        // Failed, proved by a probe, failed again after the proof: blind.
        failed_start_saved(&h.opts, ANOTHER_CONVERSATION, "9.9.9", 600);
        let to = targets.managed.clone().expect("the build");
        let r = blank(&h.sf);
        assert_eq!(
            build_start::starts_with(&h.opts, &r, &to, now_s() - 300, false, |_| {
                build_start::Probe::Starts
            }),
            Ok(())
        );
        failed_start_saved(&h.opts, "0badf00d-7777-2222-3333-444455556666", "9.9.9", 30);
        assert!(build_start::load(&h.opts, "managed", "9.9.9").is_some_and(|n| n.blind));
        if now_word {
            let mut st = load(&h.opts, SESSION).expect("the round");
            st.request = Request::Now;
            st.request_tab = TAB.to_string();
            st.request_at = now_s();
            save(&h.opts, SESSION, &st);
        }
        // The probes run before the visit (the proof above).
        let before = h.details("build-probe:starts").len();
        let r = visit(
            &h.opts,
            &h.sf,
            &[(dead_pid(), 1, "zsh".to_string())],
            &targets,
            &Script::new(dead_pid(), usize::MAX, None),
        );
        let probed = h.details("build-probe:starts").len() - before;
        (h, r, probed)
    };
    let (h, r, probed) = rig("blind-build-unasked", false);
    assert_eq!(r.step, "wait:build-fails", "{r:?}");
    assert_eq!(probed, 0, "not probed");
    drop(h);
    let (_h, r, probed) = rig("blind-build-owners-now", true);
    assert_eq!(probed, 1, "probed: {r:?}");
    assert_ne!(r.step, "wait:build-fails", "moved: {r:?}");
}

/// The model rule reads an agent's `--settings` off its process's argv only where that
/// process is the record's (its kernel start against Claude's `procStart`), never a
/// recycled pid's. Control: the record's own process's `--settings` is read.
#[cfg(unix)]
#[test]
fn the_settings_chain_reads_no_recycled_pids_settings_flag() {
    let dir = scratch("model-settings-recycled-pid");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    let user = opts.home.join(".claude");
    let catalog = user.join("cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("work");
    let proj = user.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    // A process launched with `--settings` allowing Fable 5.1 alone.
    let flag = dir.join("team.json");
    std::fs::write(&flag, r#"{"availableModels":["claude-fable-5-1"]}"#).expect("team");
    let mut holder = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("sleep 600; :")
        .arg("--settings")
        .arg(&flag)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the process");
    // Until it runs `/bin/sh` with its flag (a fork not yet exec'd reads as
    // this test): a hang detector, never a budget.
    for _ in 0..3_000 {
        if atpkg::caller_shell::process_args(holder.id())
            .is_some_and(|a| a.argv.iter().any(|arg| arg == "--settings"))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let its_start = kernel_start(holder.id()).expect("its start");
    let verdict = |proc_start: &str| {
        let sf = SessionFile {
            pid: holder.id(),
            proc_start: proc_start.to_string(),
            claude_dir: Some(user.clone()),
            cwd: work.to_string_lossy().into_owned(),
            ..record("2.1.296")
        };
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(&opts, &sf, None, &mctx, false, None).verdict
    };
    let own = verdict(&its_start);
    let recycled = verdict("Thu Oct  1 00:00:00 2026");
    let _ = holder.kill();
    let _ = holder.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(own, ModelVerdict::Keep("model-current"), "its own flag");
    assert_eq!(
        recycled,
        ModelVerdict::Due {
            from: "claude-fable-5-1".to_string(),
            to: "claude-opus-5-5".to_string(),
        },
        "a recycled pid's flag is not the record's"
    );
}

/// [`St::keep_stop`] at [`St::retarget`]: a Claude Code relaunch onto the target that
/// stopped `no-resume` is that build's failure for the conversation, so another process of
/// it keeps the stop, its rest and its streak, bound to itself with nothing of the ended
/// agent's, said once (`kept-stopped:no-resume`). Negative controls: the same process keeps
/// its record; another reason, another install, a relaunch that kept its program, a Codex
/// record and a newer build each start a fresh pending round.
#[test]
fn a_hand_resume_keeps_the_stop_of_the_build_that_failed_to_start_for_it() {
    let running = Version::parse("2.1.296").expect("v");
    let target = |version: &str, source: Source| Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse(version).expect("v"),
        source,
    };
    let to = target("2.1.999", Source::Managed);
    let stopped = |why: &str| St {
        phase: Phase::Failed(why.to_string()),
        from: "2.1.296".to_string(),
        to: "2.1.999".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pid: 41,
        shell: 40,
        salt: 100,
        pending_since: 100,
        proc_start: "A".to_string(),
        exited_at: 990,
        relaunched_at: 991,
        relaunched_pid: 42,
        failed_at: 995,
        streak_why: why.to_string(),
        stop_streak: 1,
        notices: 2,
        ..St::default()
    };
    let (st, how) = St::retarget(
        Some(stopped("no-resume")),
        &running,
        &to,
        None,
        proc("B"),
        1_029,
    );
    assert_eq!(how, Retarget::Rested);
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(
        (st.failed_at, st.streak_why.as_str(), st.stop_streak),
        (995, "no-resume", 1),
        "the stop, its rest and its streak"
    );
    assert_eq!(
        (
            st.proc_start.as_str(),
            st.from.as_str(),
            st.to.as_str(),
            st.behind_since(),
            st.tab.as_str()
        ),
        ("B", "2.1.296", "2.1.999", 1_029, TAB),
        "bound to the process that holds the conversation now"
    );
    assert_eq!(
        (st.exited_at, st.relaunched_pid, st.pid, st.notices),
        (0, 0, 0, 0),
        "nothing of the ended agent's: its exit, what its relaunch brought up, its asks"
    );
    assert_eq!(
        retarget_how(how, &st),
        "kept stopped: that build failed to start for this conversation"
    );
    // The rest runs: the same process keeps the record as it is now.
    let (again, how) = St::retarget(Some(st.clone()), &running, &to, None, proc("B"), 1_100);
    assert_eq!(
        (how, again.phase.clone()),
        (Retarget::Kept, st.phase.clone())
    );
    // Past it, a new round; that build failing to start again rests longer.
    let mut next = again;
    next.rearm(995 + upgrade::RETRY_S + 1);
    assert_eq!(next.phase, Phase::Pending);
    let at = 995 + upgrade::RETRY_S + 600;
    next.stop("no-resume", at);
    assert_eq!(next.stop_streak, 2, "a stop that repeats");
    assert_eq!(next.failed_at, at + upgrade::rest_extension(2));
    // Said once.
    let dir = scratch("kept-stopped");
    let opts = drive(&dir);
    let r = Report {
        pid: 7,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "2.1.296".to_string(),
        to: "2.1.999(managed)".to_string(),
        step: String::new(),
    };
    rested_said(&opts, &r, &st, 1_029);
    let said = ledger_details(&opts, "kept-stopped:no-resume");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("2.1.999 failed to start"), "{}", said[0]);
    assert!(
        said[0].contains("rest ends in 1h59m"),
        "counted from the stop: {}",
        said[0]
    );
    assert_eq!(
        load(&opts, SESSION).map(|st| st.phase),
        Some(st.phase.clone()),
        "saved"
    );
    let _ = std::fs::remove_dir_all(&dir);
    // Negative controls.
    let fresh = |prior: St, to: &Candidate| {
        let (st, how) = St::retarget(Some(prior), &running, to, None, proc("B"), 1_029);
        (st.phase, how)
    };
    let pending = (Phase::Pending, Retarget::Anew);
    let (kept, how) = St::retarget(
        Some(stopped("no-resume")),
        &running,
        &to,
        None,
        proc("A"),
        1_029,
    );
    assert_eq!(
        (kept.phase, how),
        (Phase::Failed("no-resume".to_string()), Retarget::Kept),
        "its own process: as it was"
    );
    assert_eq!(
        fresh(stopped("relaunch-refused"), &to),
        pending,
        "the line's"
    );
    assert_eq!(
        fresh(stopped("resumed-elsewhere"), &to),
        pending,
        "the process's"
    );
    assert_eq!(
        fresh(stopped("no-resume"), &target("2.1.999", Source::Native)),
        pending,
        "another install of the build"
    );
    let same_program = St {
        source: SAME_PROGRAM.to_string(),
        ..stopped("no-resume")
    };
    assert_eq!(
        fresh(same_program, &to),
        pending,
        "a relaunch that kept its program"
    );
    let codex = St {
        agent: upgrade::Agent::Codex,
        ..stopped("no-resume")
    };
    assert_eq!(fresh(codex, &to).0, Phase::Pending, "a Codex record");
    assert_eq!(
        fresh(stopped("no-resume"), &target("2.1.1000", Source::Managed)),
        pending,
        "a newer build"
    );
}

/// A restart on its own program keeps the stop of a build that failed to start for the
/// conversation ([`St::failed_start`] on its record, through a second restart too), and
/// the retarget off it rests as the stopped arm does ([`Retarget::Rested`]): the stop, its
/// rest and its streak, a repeat resting longer. Negative controls: a newer build is a
/// new round at once; a restart over a record with no such stop carries none.
#[test]
fn a_restart_on_its_own_program_keeps_the_stop_of_a_build_that_failed_to_start() {
    use super::super::relaunch::{CAUSE_EXIT, CAUSE_MEMORY};
    let running = Version::parse("2.1.296").expect("v");
    let target = |version: &str| Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse(version).expect("v"),
        source: Source::Managed,
    };
    let to = target("2.1.999");
    // The upgrade's relaunch onto 2.1.999 stopped `no-resume` at 995; the
    // person resumed the conversation by hand (process B), which keeps it.
    let stopped = St {
        phase: Phase::Failed("no-resume".to_string()),
        from: "2.1.296".to_string(),
        to: "2.1.999".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pid: 41,
        salt: 100,
        pending_since: 100,
        proc_start: "A".to_string(),
        failed_at: 995,
        streak_why: "no-resume".to_string(),
        stop_streak: 1,
        ..St::default()
    };
    let (rested, how) = St::retarget(Some(stopped), &running, &to, None, proc("B"), 1_029);
    assert_eq!(how, Retarget::Rested, "precondition: the hand resume rests");
    // A restart of B on its own program, `cause`, its record over the stop.
    let restarted = |prior: &St, cause: &str, phase: Phase| {
        let mut st = St {
            phase,
            from: "2.1.296".to_string(),
            to: "2.1.296".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: cause.to_string(),
            tab: TAB.to_string(),
            salt: 1_500,
            ..St::default()
        };
        prior.kept_by_restart(TAB, &mut st);
        St::from_json(&st.to_json()).expect("the record reads back")
    };
    let landed = restarted(&rested, CAUSE_MEMORY, Phase::Done);
    // Inside the rest, the round after it rests for process C as it was.
    let (next, how) = St::retarget(Some(landed.clone()), &running, &to, None, proc("C"), 1_700);
    assert_eq!(how, Retarget::Rested, "{next:?}");
    assert_eq!(next.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(
        (next.failed_at, next.streak_why.as_str(), next.stop_streak),
        (995, "no-resume", 1),
        "the stop, its rest and its streak"
    );
    assert_eq!(
        (next.proc_start.as_str(), next.to.as_str()),
        ("C", "2.1.999"),
        "bound to the process that holds the conversation now"
    );
    // Through a second restart (a crash relaunched on its program), and one
    // that itself stopped.
    let twice = restarted(&landed, CAUSE_EXIT, Phase::Done);
    let (next, how) = St::retarget(Some(twice), &running, &to, None, proc("D"), 1_800);
    assert_eq!((how, next.failed_at), (Retarget::Rested, 995), "twice");
    let mut refused = restarted(&rested, CAUSE_MEMORY, Phase::Exiting { at_s: 1_500 });
    refused.stop("relaunch-refused", 1_510);
    let (next, how) = St::retarget(Some(refused), &running, &to, None, proc("B"), 1_600);
    assert_eq!(
        (how, next.phase, next.failed_at),
        (
            Retarget::Rested,
            Phase::Failed("no-resume".to_string()),
            995
        ),
        "a restart that stopped"
    );
    // Past the rest the round re-arms and a repeat of the stop rests longer.
    let (mut late, _) = St::retarget(
        Some(landed.clone()),
        &running,
        &to,
        None,
        proc("C"),
        995 + upgrade::RETRY_S + 1,
    );
    late.rearm(995 + upgrade::RETRY_S + 1);
    late.stop("no-resume", 995 + upgrade::RETRY_S + 600);
    assert_eq!(late.stop_streak, 2, "a stop that repeats");
    // Negative controls.
    let (newer, how) = St::retarget(
        Some(landed),
        &running,
        &target("2.1.1000"),
        None,
        proc("C"),
        1_700,
    );
    assert_eq!(
        (how, newer.phase),
        (Retarget::Anew, Phase::Pending),
        "a newer build"
    );
    let plain = St {
        phase: Phase::Pending,
        from: "2.1.296".to_string(),
        to: "2.1.999".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        ..St::default()
    };
    let (next, how) = St::retarget(
        Some(restarted(&plain, CAUSE_MEMORY, Phase::Done)),
        &running,
        &to,
        None,
        proc("C"),
        1_700,
    );
    assert_eq!(
        (how, next.phase),
        (Retarget::Anew, Phase::Pending),
        "no stop"
    );
}

/// A restart left in flight acts on its tab only while it is fresh and its
/// shell is there: an exit older than [`STALE_S`] is never relaunched (the
/// person may be typing at that prompt by now), an exit whose shell is gone has
/// no prompt to relaunch at, and a relaunch older than [`STALE_S`] is waited
/// for no longer — each said once, before any socket is dialled.
#[test]
fn a_restart_in_flight_too_long_or_without_its_shell_never_acts_on_the_tab() {
    let dir = scratch("stale");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (dead, me, now) = (dead_pid(), std::process::id(), now_s());
    let old = now - STALE_S - 1;
    let cases = [
        (
            "aaaaaaaa-0000-0000-0000-000000000001",
            Phase::Exiting { at_s: old },
            me,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000002",
            Phase::Exiting { at_s: now },
            dead,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000003",
            Phase::Relaunched { at_s: old },
            me,
        ),
        // Control: fresh, its shell alive — on to the tab.
        (
            "aaaaaaaa-0000-0000-0000-000000000004",
            Phase::Exiting { at_s: now },
            me,
        ),
    ];
    for (session, phase, shell) in &cases {
        let st = St {
            phase: phase.clone(),
            pid: dead,
            shell: *shell,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        std::fs::write(state_path(&opts, session), st.to_json()).expect("write");
    }
    let steps = |o: &Opts| {
        let mut got: Vec<(String, String)> = orphans(o, &[], None)
            .into_iter()
            .map(|r| (r.session, r.step))
            .collect();
        got.sort();
        got.into_iter().map(|(_, step)| step).collect::<Vec<_>>()
    };
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(
        steps(&dry),
        [
            "would-fail:stale-exit",
            "would-fail:shell-gone",
            "would-fail:no-resume",
            "would-resume:exiting"
        ]
    );
    assert_eq!(ledger_lines(&opts), 0, "a dry run records nothing");
    assert_eq!(
        steps(&opts),
        [
            "failed:stale-exit",
            "failed:shell-gone",
            "failed:no-resume",
            "wait:no-socket"
        ]
    );
    assert_eq!(ledger_lines(&opts), 3);
    assert_eq!(
        load(&opts, cases[0].0).map(|s| s.phase),
        Some(Phase::Failed("stale-exit".to_string()))
    );
    assert_eq!(
        load(&opts, cases[3].0).map(|s| s.phase),
        Some(Phase::Exiting { at_s: now })
    );
    // Said once: a failed restart is no longer in flight.
    assert_eq!(steps(&opts), ["wait:no-socket"]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The stale-exit bound counts from the exit a look saw, not the signal: an agent slow to
/// shut down, seen gone by the host's carry a moment ago, is carried by the orphan pass
/// too. Negative controls: seen gone past the bound, it is stale; the pass's own first
/// sighting (no stamp) decides on the signal, the one time known to come before the exit.
#[test]
fn a_restart_in_flight_is_stale_from_its_seen_exit_not_its_signal() {
    let dir = scratch("stale-from-exit");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (dead, me, now) = (dead_pid(), std::process::id(), now_s());
    let signal = now - STALE_S - 60;
    let cases = [
        ("aaaaaaaa-0000-0000-0000-000000000021", now - 10),
        ("aaaaaaaa-0000-0000-0000-000000000022", now - STALE_S - 1),
        ("aaaaaaaa-0000-0000-0000-000000000023", 0),
    ];
    for (session, exited_at) in cases {
        let st = St {
            phase: Phase::Exiting { at_s: signal },
            pid: dead,
            shell: me,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            exited_at,
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let mut got: Vec<(String, String)> = orphans(&opts, &[], None)
        .into_iter()
        .map(|r| (r.session, r.step))
        .collect();
    got.sort();
    let steps: Vec<&str> = got.iter().map(|(_, step)| step.as_str()).collect();
    assert_eq!(
        steps,
        ["wait:no-socket", "failed:stale-exit", "failed:stale-exit"]
    );
    assert_eq!(
        load(&opts, cases[0].0).map(|s| (s.phase, s.exited_at)),
        Some((Phase::Exiting { at_s: signal }, now - 10)),
        "carried, its exit as the carry saw it"
    );
    assert_ne!(
        load(&opts, cases[2].0).map_or(0, |s| s.exited_at),
        0,
        "the first sighting is stamped all the same"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The orphan pass stamps when a Claude Code restart's agent was seen gone
/// ([`St::exited_at`], as the Codex lane does), before expiry is decided or a socket
/// dialled: a failure after it has no process left to vet it by. Negative controls: an
/// agent still exiting is not stamped; a dry run writes nothing.
#[test]
fn a_restart_whose_agent_the_orphan_pass_sees_gone_says_when() {
    let dir = scratch("seen-gone");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (dead, me, now) = (dead_pid(), std::process::id(), now_s());
    let cases = [
        (
            "aaaaaaaa-0000-0000-0000-000000000011",
            Phase::Exiting {
                at_s: now - STALE_S - 1,
            },
            dead,
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000012",
            Phase::Exiting { at_s: now },
            dead,
        ),
        // Control: the agent still exiting.
        (
            "aaaaaaaa-0000-0000-0000-000000000013",
            Phase::Exiting { at_s: now },
            me,
        ),
    ];
    for (session, phase, pid) in &cases {
        let st = St {
            phase: phase.clone(),
            pid: *pid,
            shell: me,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let seen = |session: &str| load(&opts, session).map_or(0, |st| st.exited_at);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let _ = orphans(&dry, &[], None);
    assert!(cases.iter().all(|(s, _, _)| seen(s) == 0), "a dry run");
    let _ = orphans(&opts, &[], None);
    assert_ne!(seen(cases[0].0), 0, "expired: seen gone first");
    assert_eq!(
        load(&opts, cases[0].0).map(|s| s.phase),
        Some(Phase::Failed("stale-exit".to_string()))
    );
    assert_ne!(seen(cases[1].0), 0, "carried on: seen gone");
    assert_eq!(seen(cases[2].0), 0, "still exiting: not gone");
    // A new signal on a record an earlier restart stamped: its agent is not
    // seen gone yet, so the old stamp is cleared (it would read
    // `stuck:exited` for an agent still exiting). The orphan pass over it, the
    // agent alive, leaves it so.
    let mut again = load(&opts, cases[1].0).expect("stamped");
    assert_ne!(again.exited_at, 0);
    let _ = again.signalled(me, me, TAB, "claude --resume x".to_string(), now);
    assert_eq!(again.exited_at, 0, "a new signal: not seen gone");
    save(&opts, cases[1].0, &again);
    let _ = orphans(&opts, &[], None);
    assert_eq!(seen(cases[1].0), 0, "alive: still not gone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ledger rows of `opts` whose step is `step`, by the conversation each
/// names.
fn noted_for(opts: &Opts, step: &str, session: &str) -> usize {
    std::fs::read_to_string(ledger_path(opts))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("step").and_then(Value::as_str) == Some(step))
        .filter(|v| v.get("session").and_then(Value::as_str) == Some(session))
        .count()
}

/// A hung exit (a SIGTERMed Claude Code stuck in its own shutdown keeps its session file)
/// is said once past [`STALE_S`] (`stuck:exiting`, keyed on [`St::noted`]) and only noted:
/// the visit still waits and the record stays in flight (a failure would leave an agent that
/// exits later unrelaunched), nothing signalled or typed; the ledger and `--status` agree.
/// Negative control: within the bound, nothing is said.
#[cfg(unix)]
#[test]
fn a_hung_exit_past_its_bound_is_noted_once_and_nothing_is_forced() {
    for (case, age, notes) in [("past", STALE_S + 60, 1), ("within", 60, 0)] {
        let h = Parked::new(
            &format!("stuck-exiting-{case}"),
            Answers::default(),
            settled(),
            3600,
        );
        let at_s = now_s() - age;
        save(
            &h.opts,
            SESSION,
            &St {
                phase: Phase::Exiting { at_s },
                pid: h.sf.pid,
                ..settled()
            },
        );
        let steps = [h.visit().step, h.visit().step];
        assert_eq!(steps, ["wait:exiting", "wait:exiting"], "{case}");
        let said = h.details("stuck:exiting");
        assert_eq!(said.len(), notes, "{case}: said once: {said:?}");
        if let Some(detail) = said.first() {
            assert!(detail.contains("has not exited"), "{detail}");
            assert!(detail.contains("nothing is forced"), "{detail}");
        }
        let kept = load(&h.opts, SESSION).expect("state");
        assert_eq!(
            kept.phase,
            Phase::Exiting { at_s },
            "{case}: still in flight, never a failure"
        );
        let signalled = h
            .asked
            .lock()
            .map_or(true, |a| a.iter().any(|l| l.contains(" signal ")));
        assert!(!signalled, "{case}: nothing is signalled");
        assert!(h.typed().is_empty(), "{case}: nothing is typed");
        let shown = rows(&h.opts);
        assert_eq!(
            shown.first().and_then(|r| r.stall(now_s())).as_deref(),
            (notes == 1).then_some("stuck:exiting"),
            "{case}: the ledger and --status agree"
        );
    }
}

/// The orphan pass skips a hung agent that already removed its session file while its pid
/// lives, and past [`STALE_S`] says so once; nothing is acted on and its record stays in
/// flight. Negative controls: within the bound nothing is said; a dry run writes nothing.
#[test]
fn a_hung_exit_with_no_session_file_is_noted_once_by_the_orphan_pass() {
    let dir = scratch("stuck-orphan");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (me, now) = (std::process::id(), now_s());
    let cases = [
        (
            "aaaaaaaa-0000-0000-0000-000000000031",
            now - STALE_S - 60,
            1,
        ),
        ("aaaaaaaa-0000-0000-0000-000000000032", now - 60, 0),
    ];
    for (session, at_s, _) in cases {
        let st = St {
            phase: Phase::Exiting { at_s },
            pid: me,
            shell: me,
            tab: TAB.to_string(),
            to: "2.1.281".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert!(orphans(&dry, &[], None).is_empty());
    assert_eq!(
        noted_for(&opts, "stuck:exiting", cases[0].0),
        0,
        "a dry run"
    );
    let first = orphans(&opts, &[], None);
    let second = orphans(&opts, &[], None);
    assert!(
        first.is_empty() && second.is_empty(),
        "nothing acted on: {first:?} {second:?}"
    );
    for (session, at_s, notes) in cases {
        assert_eq!(
            noted_for(&opts, "stuck:exiting", session),
            notes,
            "{session}"
        );
        assert_eq!(
            load(&opts, session).map(|s| s.phase),
            Some(Phase::Exiting { at_s }),
            "{session}: still in flight"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunched agent whose carry-on keeps waiting is said once past [`STALE_S`]
/// (`stuck:relaunched`), nothing typed and the record still relaunched. Negative controls:
/// within the bound nothing is said; a relaunch on exit's record (in no `--status`) says
/// nothing.
#[cfg(unix)]
#[test]
fn a_relaunch_that_never_carries_on_is_noted_once_past_its_bound() {
    for (case, age, cause, notes) in [
        ("past", STALE_S + 60, "", 1),
        ("within", 60, "", 0),
        ("on-exit", STALE_S + 60, CAUSE_EXIT, 0),
    ] {
        // Short: the instance's socket path is bounded (`SUN_LEN`).
        let dir = scratch(&format!("stuck-rl-{case}"));
        let (sock, asked) = instance(&dir);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        let at_s = now_s() - age;
        let mut st = St {
            phase: Phase::Relaunched { at_s },
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            cause: cause.to_string(),
            ..St::default()
        };
        save(&opts, SESSION, &st);
        let mut c = connect(&opts, TAB).expect("control connection");
        let mut steps = Vec::new();
        for _ in 0..2 {
            let r = carry_on_with_tab_probe(
                &opts,
                blank(&sf),
                &mut st,
                &mut c,
                SESSION,
                &sf,
                MODEL_WAIT,
                |_, _, _| false,
                None,
            );
            steps.push(r.step);
        }
        let _ = agent.kill();
        let _ = agent.wait();
        let said = ledger_details(&opts, "stuck:relaunched");
        let kept = load(&opts, SESSION).map(|s| s.phase);
        let typed = turns(&asked);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(steps, ["wait:tab-ownership-changed"; 2], "{case}");
        assert_eq!(said.len(), notes, "{case}: said once: {said:?}");
        if let Some(detail) = said.first() {
            assert!(detail.contains("has not reached an idle point"), "{detail}");
        }
        assert_eq!(typed, 0, "{case}: nothing is typed");
        assert_eq!(kept, Some(Phase::Relaunched { at_s }), "{case}");
    }
}

/// The relaunch's own look: once the kernel says the signalled agent is gone
/// ([`super::super::upgrade_wake::wait_exit`]) its record says when, though
/// the shell's prompt is not back yet and nothing is typed.
#[cfg(unix)]
#[test]
fn a_restart_whose_agent_the_relaunch_sees_gone_says_when() {
    let st = relaunched_over("seen-gone-relaunch", dead_pid());
    assert_ne!(st.exited_at, 0, "seen gone");
}

/// So too for an agent its shell has not reaped yet: a zombie still answers
/// `kill(pid, 0)`, so the exit stamp follows `wait_exit`, which reads a zombie's exit.
/// (Where the kernel can say a process is a zombie: macOS's exit watch, Linux's
/// `/proc/<pid>/stat`.)
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_restart_whose_agent_is_not_reaped_yet_says_when_it_ended() {
    let mut zombie = Command::new("/bin/sleep")
        .arg("60")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the agent");
    let pid = zombie.id();
    let gone = libc::pid_t::try_from(pid).expect("pid");
    // SAFETY: the test's own child; reaped below, after the look.
    unsafe { libc::kill(gone, libc::SIGKILL) };
    assert!(
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || super::super::upgrade_wake::wait_exit(pid, Duration::ZERO).then_some(())
        )
        .is_some(),
        "the agent ended"
    );
    assert!(
        alive(pid),
        "a zombie, not reaped: kill(pid, 0) still finds it"
    );
    let st = relaunched_over("unreaped-relaunch", pid);
    let _ = zombie.wait();
    assert_ne!(st.exited_at, 0, "its exit seen");
}

/// One [`relaunch`] of a restart whose signalled agent is `pid`, at a shell
/// that never takes its terminal back: `wait:shell-prompt`, still in flight,
/// nothing typed; the record after it.
#[cfg(unix)]
fn relaunched_over(name: &str, pid: u32) -> St {
    let _quick = QuickWaits::new();
    let dir = scratch(name);
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    // A "shell" that never holds a terminal: its prompt is never back.
    let mut shell = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("stand-in shell");
    let mut st = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid,
        shell: shell.id(),
        tab: TAB.to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        ..St::default()
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let r = relaunch(
        &opts,
        Report {
            pid: st.pid,
            tab: TAB.to_string(),
            session: SESSION.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.283(managed)".to_string(),
            step: String::new(),
        },
        &mut st,
        &mut c,
        SESSION,
        &Live,
    );
    drop(c);
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(r.step, "wait:shell-prompt");
    assert!(matches!(st.phase, Phase::Exiting { .. }), "still in flight");
    assert_eq!(turns(&asked), 0, "nothing typed");
    let _ = std::fs::remove_dir_all(&dir);
    st
}

/// Every step word is taught and an untaught one is no last word: the last words are named
/// in [`after`], the words the lanes say are taught ([`unknown_step`] false; one of each family
/// below), and an untaught word is looked at again at the ladder's longest pause, the host
/// warning of it.
#[test]
fn every_step_word_is_taught_and_an_untaught_one_is_looked_at_again() {
    for last in [
        "current",
        "done",
        "done:fresh",
        "ended:one-shot",
        "skip:not-selected",
        "daemon-updated",
        "would-announce",
        "wait:skipped",
        "terminated",
        "relaunched",
        "exit-typed",
        "exited",
    ] {
        assert_eq!(after(last, 0), After::Finished, "{last}");
        assert!(!unknown_step(last), "{last}");
    }
    // The words each lane says as it acts or waits, none of them a guess.
    for word in [
        "announced:1",
        "continued",
        "adopted",
        "gave-up",
        "drain-expired:box",
        "released:gave-up",
        "rearmed:unanswered",
        "failed:no-resume",
        "refused:not-a-shell-job",
        "exit-refused",
        "busy:another-sweep",
        "busy:state-unwritable",
        "held-back:terminal:tmux",
        "left-typed-cleared",
        "left-typed:notice",
        "wait:not-idle:busy",
        "wait:failed",
        "wait:box",
        "reopened:notice-process-gone",
        // The Codex ladder's and the goal pause's words: each is looked at
        // again, none of them the last word.
        "goal-paused",
        "goal-esc",
        "goal-paused-seen",
        "goal-resumed:moved",
        "goal-released:by-hand",
        "goal-pause-refused",
        "wait:goal-held",
        "wait:goal:record",
    ] {
        assert!(!unknown_step(word), "{word}");
        assert_ne!(after(word, 0), After::Finished, "{word}");
    }
    // The goal pause's own acts hand the next move to the next idle point:
    // the paused goal's last turn ends there.
    for word in ["goal-paused", "goal-esc", "wait:goal-held"] {
        assert_eq!(after(word, 0), After::NextIdle, "{word}");
    }
    // An untaught word: looked at again, at the longest pause, however many
    // looks came before, and said to be untaught.
    for (word, waits) in [("brand-new", 0), ("brand-new:thing", 9), ("", 3)] {
        assert!(unknown_step(word), "{word}");
        assert_eq!(
            after(word, waits),
            After::Later(LATER[LATER.len() - 1]),
            "{word}"
        );
    }
}

/// The busy footer is read below the composer, not in the transcript: an idle
/// screen whose scrollback quotes `esc to interrupt` is not a turn running.
/// Negative control: an any-row rule, replayed, reads the quote as busy.
#[test]
fn a_footer_quoted_in_the_transcript_is_no_turn_running() {
    let rule = "─".repeat(40);
    let footer = "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt";
    let rows = |lines: &[&str]| -> Vec<String> { lines.iter().map(|l| (*l).to_string()).collect() };
    let quoted = rows(&[
        "⏺ The footer read:",
        "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt",
        "",
        &rule,
        "❯ ",
        &rule,
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
    ]);
    assert!(!busy_footer(&quoted), "a quote above the composer");
    let running = rows(&["⏺ Bash(sleep 600)", "", &rule, "❯ ", &rule, footer]);
    assert!(busy_footer(&running), "the footer under the composer");
    // The recorded reply the drive tests start from: its footer is under the
    // composer, with no full rule between them.
    assert!(busy_footer(&parse_screen(SCREEN).expect("parses").rows));
    // A multi-row draft that quotes the hint is the composer, not its footer.
    let drafted = rows(&[
        "",
        &rule,
        "❯ see the footer:",
        "  esc to interrupt",
        &rule,
        "  ⏵⏵ auto",
    ]);
    assert!(!busy_footer(&drafted));
    // No composer on screen (a box replaced it): no footer to read.
    assert!(!busy_footer(&rows(&[
        "Do you want to proceed?",
        "  esc to interrupt"
    ])));
    // The spinner row above the composer carries the hint over an idle footer
    // (the auto-continue fixture in `aterm-phase`): a turn running, read by
    // the live status row.
    let spinning = rows(&[
        "⏺ Bash(./run.sh --queue 8)",
        "  ⎿  queued 8 runs",
        "",
        "✶ Deliberating… (4s · ↑ 1.2k tokens · esc to interrupt)",
        "",
        &rule,
        "❯ ",
        &rule,
        "  ⏵⏵ auto mode on (shift+tab to cycle)",
    ]);
    assert!(busy_footer(&spinning), "the spinner row above the composer");
    // Negative control: an any-row rule reads the quote as busy.
    let any_row = |rows: &[String]| rows.iter().any(|r| r.contains("esc to interrupt"));
    assert!(any_row(&quoted));
}

/// A host's `help turn` that names the rows fence (`if-rows=`).
const ROWS_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] \
                         [if-rows=<first>-<last>:<hex16>] ...";

/// A host's `help turn` that names the typing budget (`cadence=`, `cap=`, `yield=`).
const CAP_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                        [yield=<floor>] [if-gen=<epoch>.<seq>] ...";

/// The Claude notice fences on its input box where the host's `help turn` names
/// `if-rows=`: the box's three rows (rule, caret row, rule), hashed as the host does, and
/// no `if-gen=`, so a spinner repainting above the box does not refuse it. Negative
/// controls: an older host, or a caret row not framed by rules, fences on the generation.
#[cfg(unix)]
#[test]
fn the_claude_notice_fences_on_its_input_box_where_the_host_takes_if_rows() {
    use aterm_types::control_verbs::RowsFence;
    let rule = "─".repeat(20);
    let screen = |framed: bool| {
        let edge = if framed {
            rule.clone()
        } else {
            "  ".to_string()
        };
        format!(
            r#"{{"rows":["✻ Pondering… (3s)","{edge}","❯ ","{edge}"],"cursor":{{"row":2,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        )
    };
    let turn_line = |help: &'static str, framed: bool, name: &str| -> String {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen: screen(framed),
                help,
                turn: vec!["OK 0 id=2 submitted=1 status=settled"],
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut c = connect(&opts, TAB).expect("control connection");
        let (fence, _) = claude_fence(&mut c, TAB).expect("empty composer");
        turn_fenced(&mut c, TAB, "[aterm harness] hi", fence.as_deref()).expect("typed");
        drop(c);
        let line = asked
            .lock()
            .expect("log")
            .iter()
            .find(|l| l.split_whitespace().find(|w| !w.starts_with('@')) == Some("turn"))
            .cloned()
            .expect("a turn was sent");
        let _ = std::fs::remove_dir_all(&dir);
        line
    };
    let want = RowsFence {
        first: 1,
        last: 3,
        hash: RowsFence::hash_rows([rule.as_str(), "❯ ", rule.as_str()]),
    };
    let narrow = turn_line(ROWS_HELP, true, "fence-rows");
    assert!(narrow.contains(&format!("if-rows={want} ")), "{narrow}");
    assert!(!narrow.contains("if-gen="), "{narrow}");
    // Negative controls: an older host, and a composer not framed by rules.
    let older = turn_line(FENCED_HELP, true, "fence-rows-older");
    assert!(
        older.contains("if-gen=4.77 ") && !older.contains("if-rows="),
        "{older}"
    );
    let unframed = turn_line(ROWS_HELP, false, "fence-rows-unframed");
    assert!(
        unframed.contains("if-gen=4.77 ") && !unframed.contains("if-rows="),
        "{unframed}"
    );
}

/// The fenced notice asks nothing between its read and its `turn` (the server types
/// nothing when output came after the generation it is handed): the `help turn` probe is
/// asked first and kept, and a read that needs the `cell` is read once more after it.
/// Negative control: the other order sends the `cell` and the probe after the fenced read.
#[cfg(unix)]
#[test]
fn the_fenced_notice_asks_nothing_between_its_read_and_its_turn() {
    let rule = "─".repeat(20);
    let at = |col: u32, text: &str| {
        format!(
            r#"{{"rows":["✻ Pondering… (3s)","{rule}","❯ {text}","{rule}"],"cursor":{{"row":2,"col":{col}}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        )
    };
    let verbs = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.split_whitespace().find(|w| !w.starts_with('@')))
                    .filter(|v| *v != "AUTH")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let rig = |name: &str, answers: Answers| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let c = connect(&opts, TAB).expect("control connection");
        (dir, c, asked)
    };
    let answers = |after: Option<(usize, String)>| Answers {
        screen: at(2, ""),
        after,
        help: FENCED_HELP,
        turn: vec!["OK 0 id=2 submitted=1 status=settled"],
        ..Answers::default()
    };
    // Twice on one connection: the probe once, and each `turn` straight
    // after the read its generation came from.
    let (dir, mut c, asked) = rig("fence-order", answers(None));
    for _ in 0..2 {
        let generation = typing_fence(&mut c, TAB).expect("empty composer");
        assert_eq!(generation.as_deref(), Some("4.77"));
        turn_fenced(&mut c, TAB, "[aterm harness] hi", generation.as_deref()).expect("typed");
    }
    drop(c);
    let got = verbs(&asked);
    assert_eq!(
        got,
        [
            "help", "text", "cell", "text", "turn", "text", "cell", "text", "turn"
        ],
        "{got:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    // A cursor off column 2 asks no `cell`, and reads once.
    let (dir, mut c, asked) = rig(
        "fence-no-cell",
        Answers {
            screen: at(4, "x"),
            ..answers(None)
        },
    );
    assert_eq!(typing_fence(&mut c, TAB), Err("draft"), "a typed draft");
    drop(c);
    assert_eq!(verbs(&asked), ["help", "text"]);
    let _ = std::fs::remove_dir_all(&dir);
    // The composer moved between the two reads: `changed`, nothing typed.
    let (dir, mut c, asked) = rig("fence-moved", answers(Some((1, at(3, "a")))));
    assert_eq!(typing_fence(&mut c, TAB), Err("changed"));
    drop(c);
    assert!(!verbs(&asked).contains(&"turn".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
    // Negative control: the other order (the read, then the `cell` and the probe).
    let (dir, mut c, asked) = rig("fence-old-order", answers(None));
    let scr = screen(&mut c, TAB).expect("a read");
    assert!(composer_empty(&mut c, TAB, &scr));
    turn_fenced(&mut c, TAB, "[aterm harness] hi", scr.generation.as_deref()).expect("typed");
    drop(c);
    let got = verbs(&asked);
    let read = got.iter().position(|v| v == "text").expect("read");
    let turn = got.iter().position(|v| v == "turn").expect("turn");
    assert_eq!(&got[read + 1..turn], ["cell", "help"], "{got:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The fence judges the composer where it is drawn, not at the cursor's row (a grid
/// displaced by a row puts the cursor on the rule below): the `cell` is asked at the caret
/// row, and two reads are the same composer when the caret row's text is the same.
/// Negative controls: typed text there is a draft; a composer changed between the reads
/// is `changed`, nothing typed.
#[cfg(unix)]
#[test]
fn the_fence_judges_the_composer_row_not_the_cursors_row() {
    let rule = "─".repeat(20);
    // The composer on row 2; the cursor one row below it, at column 2.
    let displaced = |text: &str| {
        format!(
            r#"{{"rows":["✻ Pondering… (3s)","{rule}","❯ {text}","{rule}"],"cursor":{{"row":3,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        )
    };
    let asked_cells = |asked: &std::sync::Mutex<Vec<String>>| -> Vec<String> {
        asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" cell ")).cloned().collect())
            .unwrap_or_default()
    };
    let rig = |name: &str, answers: Answers| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let c = connect(&opts, TAB).expect("control connection");
        (dir, c, asked)
    };
    let dim = "OK p 999999 111318 dim";
    let base = |text: &str, cell: &'static str| Answers {
        screen: displaced(text),
        help: FENCED_HELP,
        cell,
        ..Answers::default()
    };
    // The suggestion, dim: an empty composer, fenced on the read's generation.
    let (dir, mut c, asked) = rig("displaced-dim", base("try the tests", dim));
    assert_eq!(typing_fence(&mut c, TAB), Ok(Some("4.77".to_string())));
    drop(c);
    assert_eq!(asked_cells(&asked), [format!("@{TAB} cell 2 2")]);
    let _ = std::fs::remove_dir_all(&dir);
    // The same rows with the text typed: a draft.
    let (dir, mut c, _asked) = rig(
        "displaced-typed",
        base("try the tests", "OK p d0d0d0 111318 none"),
    );
    assert_eq!(typing_fence(&mut c, TAB), Err("draft"));
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
    // The composer's text changed between the two reads, the cursor row not.
    let (dir, mut c, asked) = rig(
        "displaced-moved",
        Answers {
            after: Some((1, displaced("try the tests, and then the lint"))),
            ..base("try the tests", dim)
        },
    );
    assert_eq!(typing_fence(&mut c, TAB), Err("changed"));
    drop(c);
    assert!(
        !asked
            .lock()
            .is_ok_and(|a| a.iter().any(|l| l.contains(" turn "))),
        "nothing typed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host's next move after one step: park again at the next idle point
/// after an announcement (the answer comes at a turn's end), look again on a
/// growing pause after any wait (every minute while a person's hold stands,
/// [`HOLD_LOOK`]), say a state that cannot hold the lock once, and stop for
/// this build on anything final. Negative control: another actor on the lock
/// is a wait, not a fault.
#[test]
fn after_a_step_the_host_parks_waits_longer_stops_or_says_it_is_broken() {
    assert_eq!(after("announced:1", 0), After::NextIdle);
    // The relaunched agent is its loop's: carried on where it parks.
    assert_eq!(after("adopted", 2), After::NextIdle);
    // The carry-on typed, its model still to be read: at the end of the
    // resumed session's turn.
    assert_eq!(after("continued", 3), After::NextIdle);
    assert_eq!(after("wait:settling", 0), After::Later(LATER[0]));
    assert_eq!(after("wait:not-ready", 1), After::Later(LATER[1]));
    assert_eq!(after("held-back:terminal:tmux", 2), After::Later(LATER[2]));
    assert_eq!(after("busy:another-sweep", 9), After::Later(LATER[3]));
    assert_eq!(after("busy:state-unwritable", 0), After::Broken);
    assert_eq!(after("busy:lock-unopenable", 0), After::Broken);
    // A person's hold is looked at every HOLD_LOOK however long it waits, so
    // the drain's dwell is measured (`upgrade::hold_since`); any other wait
    // grows past the gap the dwell tolerates — the control.
    for held in ["wait:box", "wait:draft"] {
        assert_eq!(after(held, 9), After::Later(HOLD_LOOK), "{held}");
    }
    assert!(LATER[3].as_secs() > upgrade::HOLD_GAP_S);
    // An upgrade that stopped asking, or voided a READY answer, is never the
    // last word: the agent is owed its release and a late READY is honoured,
    // both at the next idle point. A release typed is looked at again like
    // any wait, and so is one still owed (`wait:release:<why>`).
    assert_eq!(after("gave-up", 0), After::NextIdle);
    assert_eq!(after("drain-expired:box", 0), After::NextIdle);
    assert_eq!(after("released:gave-up", 0), After::Later(LATER[0]));
    assert_eq!(after("wait:release:limited", 2), After::Later(LATER[2]));
    // A stopped round says so as it stops (`failed:<why>`, `refused:<what>`) and, resting
    // with nothing owed, `wait:failed`; none is a last word, since a new round starts once
    // it has rested `upgrade::RETRY_S`, so each is looked at again up to the ladder's last
    // look; so is the new round's `rearmed:<why>`.
    for resting in [
        "wait:failed:unanswered",
        "refused:not-a-shell-job",
        "failed:no-resume",
        "wait:failed",
        "rearmed:unanswered",
    ] {
        assert_eq!(after(resting, 0), After::Later(LATER[0]), "{resting}");
        assert_eq!(after(resting, 9), After::Later(LATER[3]), "{resting}");
    }
    for last in ["current", "done", "done:no-continue", "wait:skipped"] {
        assert_eq!(after(last, 0), After::Finished, "{last}");
    }
    assert!(
        LATER[0] >= Duration::from_secs(upgrade::QUIET_S),
        "the first look waits out the settle a new idle point needs"
    );
    assert!(LATER.windows(2).all(|w| w[0] < w[1]), "each pause longer");
}

/// Whether a step moved the session and whether it typed a turn: one that typed into the agent
/// or ended it leaves no point to decide again, and a typed turn (notice, carry-on, release) is
/// awaited as the harness's own. Negative controls: ending the agent types no turn; a word that
/// typed and ended nothing leaves the point standing.
#[test]
fn a_step_that_typed_or_ended_the_agent_moved_the_session() {
    for turn in [
        "announced:1",
        "announced:3",
        "continued",
        "released:gave-up",
        "released:void",
    ] {
        assert!(moved(turn) && typed(turn), "{turn}");
    }
    for ended in [
        "terminated",
        "relaunched",
        "adopted",
        "done:fresh",
        "exit-typed",
        "exited",
        "done:unconfirmed",
    ] {
        assert!(moved(ended) && !typed(ended), "{ended}");
    }
    for left in [
        "current",
        "done",
        "done:model-unconfirmed",
        "wait:model",
        "wait:settling",
        "wait:awaiting-ready",
        "wait:fresh",
        "skip:not-selected",
        "busy:another-sweep",
        "would-announce",
        "would-restart:fresh",
        "held-back:terminal:tmux",
        "gave-up",
        "failed:no-resume",
        "refused:not-a-shell-job",
        "done:taskless",
        "done:no-continue",
        "left-typed-cleared",
        "wait:release:draft",
        "would-release:gave-up",
    ] {
        assert!(!moved(left) && !typed(left), "{left}");
    }
}

/// The upgrade owns a session's turn ends by its step's word ([`owns_turn_ends`]): after the
/// announcement, and while READY is given and the gate only settles or drains (bounded); never
/// after an answer without READY, a hold, an owed release or a last word. Negative controls:
/// past the bounded looks a drain or settle owns nothing; an owed release owns nothing.
#[test]
fn the_upgrade_owns_turn_ends_only_while_its_step_says_so() {
    const GRACE: u32 = 120;
    assert!(owns_turn_ends("announced", 0, GRACE));
    assert!(owns_turn_ends("announced:2", 0, GRACE));
    assert!(owns_turn_ends("wait:settling", 0, GRACE));
    assert!(!owns_turn_ends("wait:settling", OWNED_SETTLE_LOOKS, GRACE));
    // Claude's status catching up with its idle screen settles as its own
    // `not-idle` does, bounded the same.
    assert!(owns_turn_ends("wait:status-stale", 0, GRACE));
    assert!(!owns_turn_ends(
        "wait:status-stale",
        OWNED_SETTLE_LOOKS,
        GRACE
    ));
    assert!(owns_turn_ends(
        "wait:background",
        OWNED_BACKGROUND_LOOKS - 1,
        GRACE
    ));
    assert!(!owns_turn_ends(
        "wait:background",
        OWNED_BACKGROUND_LOOKS,
        GRACE
    ));
    for last in [
        "wait:awaiting-ready",
        "wait:release:settling",
        "wait:release:ready",
        "released:gave-up",
        "held-back:person",
        "done",
        "gave-up",
        "failed:x",
        "wait:failed",
        "adopted",
        "continued",
        "current",
    ] {
        assert!(!owns_turn_ends(last, 0, GRACE), "{last}");
    }
}

/// Under the quiet flow a wait on the agent's own work owns no turn end and bounds none
/// ([`owns_turn_ends_as`], [`bounded_wait_as`]): drain ownership is for an agent that
/// answered READY, which the quiet flow never asks. Negative controls: the ask flow's
/// drain owns and bounds; the settle, a notice and a person's hold own in both flows.
#[test]
fn a_quiet_wait_on_the_agents_work_owns_no_turn_end() {
    const GRACE: u32 = 120;
    for drain in ["wait:background", "wait:background-terminal"] {
        assert!(!owns_turn_ends_as(drain, 0, GRACE, true), "{drain}");
        assert!(!bounded_wait_as(drain, true), "{drain}");
        assert!(owns_turn_ends_as(drain, 0, GRACE, false), "{drain}");
        assert!(bounded_wait_as(drain, false), "{drain}");
    }
    for owned in [
        "wait:settling",
        "wait:status-stale",
        "announced:1",
        "wait:attended",
    ] {
        for quiet in [true, false] {
            assert_eq!(
                owns_turn_ends_as(owned, 0, GRACE, quiet),
                owns_turn_ends(owned, 0, GRACE),
                "{owned} quiet={quiet}"
            );
            assert_eq!(bounded_wait_as(owned, quiet), bounded_wait(owned));
        }
    }
}

/// A person at the tab owns the turn end for their grace, so the loop types nothing over a
/// READY, and the attended wait is looked at again at the first rung ([`after_attended`]).
/// Negative controls: past the grace plus [`ATTENDED_OWNED_S`] it owns nothing and its looks
/// go back on the ladder; `awaiting-ready`, a hold, a give-up and an adoption own nothing; a
/// break gives it back.
#[test]
fn a_person_at_the_tab_owns_the_turn_end_for_their_grace() {
    const GRACE: u32 = 120;
    let look = LATER[0].as_secs();
    // The live sequence: held back once, then waiting, every look owned —
    // including a person who keeps reading well past one grace.
    assert!(owns_turn_ends("held-back:attended", 0, GRACE));
    for waits in [0, 1, 2, 7, 8, 30] {
        assert!(owns_turn_ends("wait:attended", waits, GRACE), "{waits}");
    }
    // The backstop: (120 s + 30 min) / 20 s = 96 looks, then given back.
    let last = u32::try_from((u64::from(GRACE) + ATTENDED_OWNED_S) / look).unwrap();
    assert_eq!(last, 96);
    assert!(owns_turn_ends("wait:attended", last, GRACE));
    assert!(!owns_turn_ends("wait:attended", last + 1, GRACE));
    assert!(!owns_turn_ends("held-back:attended", last + 1, GRACE));
    // A longer grace owns longer.
    assert!(owns_turn_ends("wait:attended", last + 1, 600));
    // Looked at again at the first rung, however long it waits.
    for waits in [0, 1, 2, 3, 9, 40] {
        assert_eq!(
            after("wait:attended", waits),
            After::Later(LATER[0]),
            "{waits}"
        );
        assert_eq!(
            after("held-back:attended", waits),
            After::Later(LATER[0]),
            "{waits}"
        );
    }
    // The host, counting its looks: the first rung while the next look owns
    // the point, then back on the ladder.
    assert_eq!(after_attended(1, 0, GRACE), After::Later(LATER[0]));
    assert_eq!(after_attended(last, 40, GRACE), After::Later(LATER[0]));
    assert_eq!(after_attended(last + 1, 0, GRACE), After::Later(LATER[0]));
    assert_eq!(after_attended(last + 1, 1, GRACE), After::Later(LATER[1]));
    assert_eq!(after_attended(last + 1, 95, GRACE), After::Later(LATER[3]));
    assert!(attended("held-back:attended") && attended("wait:attended"));
    assert!(!attended("wait:awaiting-ready") && !attended("held-back:person"));
    // Negative controls.
    for owns_nothing in [
        "wait:awaiting-ready",
        "held-back:person",
        "gave-up",
        "adopted",
    ] {
        assert!(!owns_turn_ends(owns_nothing, 0, GRACE), "{owns_nothing}");
    }
    assert!(released_at_break("wait:attended"), "a break gives it back");
    assert_eq!(
        after("wait:settling", 1),
        After::Later(LATER[1]),
        "the ladder stands"
    );
    assert_eq!(after("held-back:terminal:tmux", 2), After::Later(LATER[2]));
}

/// The release is typed only where it is the upgrade's next act ([`release_is_next`]): the
/// upgrade's own give-up, void, stop and waits on nothing of its own leave it next, while
/// a gate's wait keeps its word. A step leaving one owed never reads as the host's last
/// word ([`owed_word`]): a stop says `wait:release:<why>` instead. Negative controls: an
/// act the host looks on after keeps its word.
#[test]
fn the_release_is_the_next_act_only_where_nothing_else_is() {
    for (step, word, next) in [
        (Step::GiveUp, "gave-up", true),
        (Step::Void("box"), "drain-expired:box", true),
        (Step::Wait("failed"), "wait:failed", true),
        (Step::Wait("awaiting-ready"), "wait:awaiting-ready", true),
        (Step::Wait("skipped"), "wait:skipped", true),
        (Step::Wait("deferred"), "wait:deferred", true),
        (Step::Terminate, "failed:signal-refused", true),
        (Step::Terminate, "refused:argv", true),
        (Step::Announce, "refused:line", true),
        // The restart's or the notice's own gate: its wait goes first.
        (Step::Wait("settling"), "wait:settling", false),
        (Step::Wait("background"), "wait:background", false),
        (Step::Wait("draft"), "wait:draft", false),
        (Step::Wait("limited"), "wait:limited", false),
        (Step::Terminate, "wait:changed-before-signal", false),
        (Step::Announce, "wait:announce-refused:x", false),
        (Step::Announce, "announced:2", false),
    ] {
        assert_eq!(release_is_next(&step, word), next, "{step:?} {word}");
    }
    // At a break: the release goes as at an idle point, and an announced
    // upgrade with no READY in hand that waits on the agent's own work there
    // (`background`, an idle point's `awaiting-ready`) leaves the release a
    // void owes next. Negative controls: nothing owed; a READY the restart
    // will act on; a gave-up upgrade's READY; the same wait at an idle point
    // (the restart's gate).
    let owed = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        release: "void".to_string(),
        ..St::default()
    };
    let brk = Facts {
        status: "shell".to_string(),
        background: vec!["zsh".to_string()],
        background_point: true,
        ..Facts::default()
    };
    let waits = Step::Wait("background");
    let word = "wait:background";
    assert!(release_next(&waits, word, &owed, &brk, false));
    assert!(release_next(&Step::GiveUp, "gave-up", &owed, &brk, false));
    assert!(release_next(
        &Step::Wait("failed"),
        "wait:failed",
        &owed,
        &brk,
        false
    ));
    let nothing = St {
        release: String::new(),
        ..owed.clone()
    };
    assert!(!release_next(&waits, word, &nothing, &brk, false));
    assert!(!release_next(&waits, word, &owed, &brk, true), "READY");
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        ..owed.clone()
    };
    assert!(!release_next(&waits, word, &gave_up, &brk, true));
    let idle_point = Facts {
        background_point: false,
        ..brk.clone()
    };
    assert!(!release_next(&waits, word, &owed, &idle_point, false));
    let r = |step: &str| Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: step.to_string(),
    };
    for (word, holds, said) in [
        ("failed:signal-refused", "limited", "wait:release:limited"),
        ("refused:argv", "draft", "wait:release:draft"),
        ("wait:failed", "settling", "wait:release:settling"),
        ("wait:release", "box", "wait:release:box"),
        ("gave-up", "not-idle", "gave-up"),
        ("drain-expired:draft", "draft", "drain-expired:draft"),
    ] {
        let out = owed_word(r(word), holds);
        assert_eq!(out.step, said, "{word}");
        assert_ne!(after(&out.step, 0), After::Finished, "{word}");
    }
}

/// [`due`] parks a session only for an upgrade to take (this tab's restart in flight is one)
/// and says what it could not read rather than "not due". Negative controls: another tab's
/// restart and a finished one are not `Yes`; with no socket they are `Unread(no-socket)`.
#[test]
fn due_is_this_tabs_upgrade_or_restart_and_nothing_unread() {
    let dir = scratch("due");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    assert_eq!(
        due(&opts),
        Due::Unread(UNREAD_SOCKET),
        "no socket, no roster: nothing read, nothing decided"
    );
    let other = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: dead_pid(),
        shell: std::process::id(),
        tab: "s-0ther".to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), other.to_json()).expect("write");
    assert_eq!(
        due(&opts),
        Due::Unread(UNREAD_SOCKET),
        "another tab's restart is not this tab's"
    );
    let ours = St {
        tab: TAB.to_string(),
        ..other
    };
    std::fs::write(state_path(&opts, SESSION), ours.to_json()).expect("write");
    assert_eq!(due(&opts), Due::Yes, "this tab's restart in flight is due");
    // Done, its carry-on typed and its model still owed: due until read.
    let owed = St {
        phase: Phase::Done,
        confirm_by: now_s() + 60,
        ..ours.clone()
    };
    std::fs::write(state_path(&opts, SESSION), owed.to_json()).expect("write");
    assert_eq!(due(&opts), Due::Yes, "a model still to be read is due");
    let settled = St {
        confirm_by: 0,
        ..owed
    };
    std::fs::write(state_path(&opts, SESSION), settled.to_json()).expect("write");
    assert_eq!(
        due(&opts),
        Due::Unread(UNREAD_SOCKET),
        "a finished restart is not due: what decides is the roster"
    );
    std::fs::write(state_path(&opts, SESSION), ours.to_json()).expect("write");
    assert_eq!(
        step(&Opts {
            only_sid: Some("s-0ther".to_string()),
            ..opts.clone()
        })
        .step,
        "wait:tab-not-live",
        "a tab the instance does not list is not stepped"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// One Claude Code record of the tab's agent, `pid` 101, on `version`.
fn record(version: &str) -> SessionFile {
    SessionFile {
        pid: 101,
        session_id: SESSION.to_string(),
        cwd: "/tmp".to_string(),
        version: version.to_string(),
        status: "idle".to_string(),
        status_updated_at_ms: 0,
        proc_start: "start".to_string(),
        kind: "interactive".to_string(),
        entrypoint: "cli".to_string(),
        claude_dir: None,
        profile: None,
    }
}

/// The instance's one tab, [`TAB`], its foreground group 101.
fn the_tab() -> [LiveTab; 1] {
    [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(101),
    }]
}

/// An announced round whose notice carried a model is looked at with the build current
/// (the pre-filter [`due`] asks), so its READY, barred ride or owed release is acted on.
/// Negative controls: the same round with no model, and a gave-up round, are not.
#[test]
fn an_announced_round_carrying_a_model_is_looked_at_with_the_build_current() {
    let dir = scratch("due-announced-model");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    let tabs = the_tab();
    let current = || Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("1.0.0").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let look = |st: &St| {
        save(&opts, SESSION, st);
        due_among(
            &opts,
            &[record("1.0.0")],
            &tabs,
            current,
            |_| Some(atpkg::caller_shell::ProcArgs::default()),
            |_| Some(101),
            |_| Some((10, 101, 101)),
        )
    };
    let announced = St {
        phase: Phase::Announced { at_s: 1, asks: 1 },
        from: "1.0.0".to_string(),
        to: "1.0.0".to_string(),
        source: "managed".to_string(),
        model_list: "claude-opus-5-5".to_string(),
        ..St::default()
    };
    assert_eq!(look(&announced), Due::Yes, "the model's round is looked at");
    // Negative controls.
    let no_model = St {
        model_list: String::new(),
        ..announced.clone()
    };
    assert_eq!(look(&no_model), Due::No, "no model carried");
    let gave_up = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        ..announced.clone()
    };
    assert_eq!(look(&gave_up), Due::No, "a give-up, its release typed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record whose agent leads the tab and is behind, but whose process could not be read
/// whole (its ids, its argv, or the foreground caught moving), decides nothing:
/// [`UNREAD_PROCESS`], never "not due". Negative controls: read whole is `Yes`; an
/// environment naming another tab is `No`; a current record needs no process read.
#[test]
fn a_record_whose_process_could_not_be_read_is_unread_never_not_due() {
    let dir = scratch("due-process");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    seed_builds(&opts);
    let tabs = the_tab();
    let args = |tab: Option<&str>| {
        Some(atpkg::caller_shell::ProcArgs {
            env: tab
                .map(|t| vec![format!("ATERM_PARENT_SESSION_ID={t}")])
                .unwrap_or_default(),
            ..atpkg::caller_shell::ProcArgs::default()
        })
    };
    // A version nobody can compare takes the full path, as a behind one does.
    let behind = [record("unknown")];
    let look = |files: &[SessionFile],
                read_args: Option<atpkg::caller_shell::ProcArgs>,
                read_ids: Option<(u32, i64, i64)>| {
        due_among(
            &opts,
            files,
            &tabs,
            || Targets::read(&opts.home),
            |_| read_args.clone(),
            |_| Some(101),
            |_| read_ids,
        )
    };
    let whole = Some((10, 101, 101));
    assert_eq!(look(&behind, args(None), whole), Due::Yes, "read whole");
    assert_eq!(
        look(&behind, args(None), None),
        Due::Unread(UNREAD_PROCESS),
        "the ids unread: a `ps` that could not spawn"
    );
    assert_eq!(
        look(&behind, None, whole),
        Due::Unread(UNREAD_PROCESS),
        "the argv unread"
    );
    assert_eq!(
        look(&behind, args(None), Some((10, 101, 202))),
        Due::Unread(UNREAD_PROCESS),
        "the tab's foreground caught moving"
    );
    assert_eq!(
        look(&behind, args(None), Some((10, 303, 101))),
        Due::Unread(UNREAD_PROCESS),
        "the process caught leaving its group"
    );
    // Negative controls.
    assert_eq!(
        look(&behind, args(Some("s-another")), whole),
        Due::No,
        "another tab's by its environment: read, and none of this tab's"
    );
    assert_eq!(
        look(&behind, args(Some(TAB)), whole),
        Due::Yes,
        "its own tab by its environment"
    );
    assert_eq!(
        look(&[record("999.0.0")], None, None),
        Due::No,
        "a current record decides with no process read"
    );
    // The step's own pass says the same wait for it, on the tab.
    let seen = host_candidates(
        &behind,
        &tabs,
        |_, _| true,
        |_| args(None),
        |_| Some(101),
        |_| None,
    );
    let [Err(wait)] = seen.as_slice() else {
        panic!("{seen:?}");
    };
    assert_eq!(
        (wait.tab.as_str(), wait.session.as_str(), wait.step.as_str()),
        (TAB, SESSION, "wait:no-process")
    );
    assert_eq!(
        after(&wait.step, 0),
        After::Later(LATER[0]),
        "looked at again"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`no_record`]: no record of the tab's agent is a wait ([`UNREAD_RECORD`]) only while the
/// agent is younger than [`RECORD_WINDOW_S`], then nothing to take (a Claude Code under
/// another home, or a non-CLI session, never writes one this look finds). A leader whose
/// start cannot be read is unread while it lives; a gone tab is nothing; records caught
/// half-written are unread.
#[test]
fn a_launch_is_waited_on_for_its_record_only_within_its_window() {
    let tabs = the_tab();
    let now = 1_000_000;
    let at = |age: u64| move |pid: u32| (pid == 101).then_some(now - age);
    let lives = |_| true;
    assert_eq!(
        no_record(&tabs, TAB, at(1), lives, now),
        Due::Unread(UNREAD_RECORD),
        "a launch a second old"
    );
    assert_eq!(
        no_record(&tabs, TAB, at(RECORD_WINDOW_S - 1), lives, now),
        Due::Unread(UNREAD_RECORD)
    );
    assert_eq!(
        no_record(&tabs, TAB, at(RECORD_WINDOW_S), lives, now),
        Due::No,
        "past its launch: no record will come"
    );
    assert_eq!(
        no_record(&tabs, TAB, |_| None, lives, now),
        Due::Unread(UNREAD_PROCESS),
        "a live leader whose start could not be read"
    );
    assert_eq!(
        no_record(&tabs, TAB, |_| None, |_| false, now),
        Due::No,
        "a leader gone from its group: no launch to wait on"
    );
    let unknown = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: None,
    }];
    assert_eq!(
        no_record(&unknown, TAB, at(1), lives, now),
        Due::Unread(UNREAD_PROCESS),
        "a foreground group the kernel did not name"
    );
    assert_eq!(
        no_record(&[], TAB, at(1), lives, now),
        Due::No,
        "a tab gone"
    );
    // The records themselves.
    let dir = scratch("records");
    assert_eq!(claude_records(&dir), Some(Vec::new()), "none kept here");
    std::fs::create_dir_all(dir.join(".claude/sessions")).expect("sessions");
    // Half-written by a live process (this one): only a running process's record can be
    // mid-write.
    let me = std::process::id();
    std::fs::write(
        dir.join(format!(".claude/sessions/{me}.json")),
        "{\"pid\": 1",
    )
    .expect("half");
    assert_eq!(claude_records(&dir), None, "caught half-written: unread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tier-1 of `HarnessUpgradeLook`, its classifier half (the host's bind in aterm-gui fakes
/// `due`): the real [`due_among`] and [`no_record`] over every combination of what a look
/// reads of one tab, each verdict taken as the model's action (`Yes` is `Look`, `No` is
/// `NotDue`, `Unread` is `Unread`) at the state it projects to, which the model must
/// enable. Negative control: counting the record read before its process makes `NotDue`
/// where the model forbids it.
#[test]
fn the_real_classifier_conforms_to_the_upgrade_look_model() {
    let model = aterm_spec::derive::harness_upgrade_look_model();
    let dir = scratch("look-bind");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    seed_builds(&opts);
    let tabs = the_tab();
    let now = 1_000_000;
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Ids {
        Whole,
        Unread,
        Moved,
    }
    let at = |due: bool, readable: bool| {
        let mut state = model.init_state();
        state.insert("due", i64::from(due));
        state.insert("readable", i64::from(readable));
        state.insert("looks", 1);
        state.insert("done", 0);
        state
    };
    let action = |due: Due| match due {
        Due::Yes => "Look",
        Due::No => "NotDue",
        Due::Unread(_) => "Unread",
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut looks = 0;
    for present in [false, true] {
        for behind in [false, true] {
            for group in [false, true] {
                for ids in [Ids::Whole, Ids::Unread, Ids::Moved] {
                    for argv in [false, true] {
                        for env in [None, Some(TAB), Some("s-another")] {
                            for old in [false, true] {
                                let files = if present {
                                    vec![record(if behind { "unknown" } else { "999.0.0" })]
                                } else {
                                    Vec::new()
                                };
                                let args = argv.then(|| atpkg::caller_shell::ProcArgs {
                                    env: env
                                        .map(|t| vec![format!("ATERM_PARENT_SESSION_ID={t}")])
                                        .unwrap_or_default(),
                                    ..atpkg::caller_shell::ProcArgs::default()
                                });
                                let verdict = match due_among(
                                    &opts,
                                    &files,
                                    &tabs,
                                    || Targets::read(&opts.home),
                                    |_| args.clone(),
                                    |_| group.then_some(101),
                                    |_| match ids {
                                        Ids::Whole => Some((10, 101, 101)),
                                        Ids::Unread => None,
                                        Ids::Moved => Some((10, 101, 202)),
                                    },
                                ) {
                                    Due::Unread(UNREAD_RECORD) => no_record(
                                        &tabs,
                                        TAB,
                                        |_| Some(now - if old { RECORD_WINDOW_S } else { 1 }),
                                        |_| true,
                                        now,
                                    ),
                                    due => due,
                                };
                                // The projection: what the look could read.
                                let leads = present && group;
                                let process = ids == Ids::Whole && argv;
                                let (due, readable) = if !leads {
                                    // No record of the tab's agent: final
                                    // past its launch, unknown before.
                                    (!old, old)
                                } else if !behind {
                                    (false, true)
                                } else if process && env == Some("s-another") {
                                    // Read whole: another tab's agent.
                                    (false, true)
                                } else {
                                    (true, process)
                                };
                                let name = action(verdict);
                                assert!(
                                    model.action_enabled(name, &at(due, readable)),
                                    "{name} at due={due} readable={readable}: present={present} \
                                     behind={behind} group={group} ids={ids:?} argv={argv} \
                                     env={env:?} old={old} -> {verdict:?}"
                                );
                                seen.insert(name);
                                looks += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(looks, 288);
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        ["Look", "NotDue", "Unread"],
        "every verdict reached"
    );
    // Negative control: a record counted as read once its group led the tab,
    // before its process was, makes a behind record whose ids did not read
    // `No`.
    let counted_early = |process: bool| if process { Due::Yes } else { Due::No };
    assert_eq!(
        due_among(
            &opts,
            &[record("unknown")],
            &tabs,
            || Targets::read(&opts.home),
            |_| Some(atpkg::caller_shell::ProcArgs::default()),
            |_| Some(101),
            |_| None,
        ),
        Due::Unread(UNREAD_PROCESS),
        "the real classifier"
    );
    assert!(!model.action_enabled(action(counted_early(false)), &at(true, false)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A session is behind from its worker's attach ([`note_behind`]): minted `Pending` there,
/// and the first step reuses it with that age. Negative controls: a state already there is
/// left; a session on the newest build gets none.
#[test]
fn a_session_behind_is_minted_pending_at_attach_and_its_step_keeps_the_age() {
    let dir = scratch("behind");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    let st = load(&opts, SESSION).expect("minted");
    assert_eq!(
        (st.phase.clone(), st.to.as_str(), st.tab.as_str()),
        (Phase::Pending, "9.9.9", TAB)
    );
    assert!(now_s().saturating_sub(st.behind_since()) <= 1, "{st:?}");
    assert!(
        !mint_behind(&opts, &sf, TAB, &newer(), false, now_s()),
        "left as it is"
    );
    // The first step, later: the same upgrade, its age the attach's.
    let attached = St {
        pending_since: now_s() - 150,
        ..st
    };
    save(&opts, SESSION, &attached);
    let running = Version::parse("1.0.0").expect("version");
    let target = newer().managed.expect("target");
    let stepped = St::for_target(
        load(&opts, SESSION),
        &running,
        &target,
        None,
        Proc::default(),
        now_s(),
    );
    assert_eq!(stepped.pending_since, attached.pending_since);
    // Negative control: nothing newer to move to.
    let current = Targets {
        managed: Some(Candidate {
            version: running.clone(),
            ..target
        }),
        native: None,
        unanswered: false,
    };
    let _ = std::fs::remove_file(state_path(&opts, SESSION));
    assert!(!mint_behind(&opts, &sf, TAB, &current, false, now_s()));
    assert!(load(&opts, SESSION).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh launch is noted behind once Claude Code has written its record, its age from
/// the second the host first saw it ([`mint_behind`]'s `since`); a look that cannot read
/// the session says why ([`Behind::Unread`]). Negative control: a `since` in the future
/// never makes a session younger than now.
#[test]
fn a_launch_noted_once_its_record_is_there_is_behind_from_the_attach() {
    let dir = scratch("behind-since");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    assert_eq!(
        note_behind(&opts, now_s()),
        Behind::Unread(UNREAD_SOCKET),
        "no socket: nothing read, nothing noted"
    );
    let sf = register(&opts.home, std::process::id(), SESSION);
    let attached = now_s() - 90;
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, attached));
    let st = load(&opts, SESSION).expect("minted");
    assert_eq!(
        (st.phase.clone(), st.pending_since),
        (Phase::Pending, attached),
        "behind from the attach"
    );
    // The first step, later, keeps it.
    let running = Version::parse("1.0.0").expect("version");
    let target = newer().managed.expect("target");
    let stepped = St::for_target(
        load(&opts, SESSION),
        &running,
        &target,
        None,
        Proc::default(),
        now_s(),
    );
    assert_eq!(stepped.pending_since, attached);
    // Negative control.
    let _ = std::fs::remove_file(state_path(&opts, SESSION));
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, now_s() + 600));
    let st = load(&opts, SESSION).expect("minted");
    assert!(st.pending_since <= now_s(), "{st:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A state an earlier upgrade left (done, or failed on another build) is minted behind
/// anew when a newer build lands ([`behind_anew`]), its age from now. Negative controls: a
/// state in flight, a carry-on awaiting its model, a failure on this very build, and a
/// pending one are left as they are.
#[test]
fn a_session_upgraded_before_is_behind_anew_when_a_newer_build_lands() {
    let dir = scratch("behind-anew");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let earlier = |phase: Phase, to: &str| St {
        phase,
        from: "0.9.0".to_string(),
        to: to.to_string(),
        tab: TAB.to_string(),
        pending_since: now_s() - 3_600,
        done_at: now_s() - 1_800,
        ..St::default()
    };
    for prior in [
        earlier(Phase::Done, "1.0.0"),
        earlier(Phase::Failed("no-resume".to_string()), "1.0.0"),
    ] {
        save(&opts, SESSION, &prior);
        assert!(
            mint_behind(&opts, &sf, TAB, &newer(), false, now_s()),
            "{prior:?}"
        );
        let st = load(&opts, SESSION).expect("minted");
        assert_eq!(
            (st.phase.clone(), st.to.as_str()),
            (Phase::Pending, "9.9.9")
        );
        assert!(now_s().saturating_sub(st.behind_since()) <= 1, "{st:?}");
    }
    for prior in [
        earlier(Phase::Exiting { at_s: now_s() }, "9.9.9"),
        St {
            confirm_by: now_s() + 60,
            ..earlier(Phase::Done, "1.0.0")
        },
        earlier(Phase::Failed("no-resume".to_string()), "9.9.9"),
        earlier(Phase::Pending, "9.9.9"),
    ] {
        save(&opts, SESSION, &prior);
        assert!(
            !mint_behind(&opts, &sf, TAB, &newer(), false, now_s()),
            "{prior:?}"
        );
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            (&st.phase, &st.to, st.pending_since, st.confirm_by),
            (
                &prior.phase,
                &prior.to,
                prior.pending_since,
                prior.confirm_by
            )
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A conversation resumed in another tab is moved into this tab at the host's attach
/// ([`mint_behind`]) and onto the build the visit would choose (`retargeted:…`, said
/// once), its round and age kept; a resting stopped round moves too. Negative controls: a
/// record already here, or an announced round in another tab (its tab the notice's
/// fence), stays.
#[test]
fn a_conversation_resumed_in_another_tab_is_retabbed_at_the_attach() {
    let dir = scratch("behind-retab");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let elsewhere = "s-0123456789abcdef0123";
    let waiting = |phase: Phase, to: &str| St {
        phase,
        from: "1.0.0".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        tab: elsewhere.to_string(),
        salt: 100,
        pending_since: 100,
        ..St::default()
    };
    save(&opts, SESSION, &waiting(Phase::Pending, "9.9.8"));
    assert!(
        mint_behind(&opts, &sf, TAB, &newer(), false, now_s()),
        "noted"
    );
    let st = load(&opts, SESSION).expect("the record");
    assert_eq!(
        (st.tab.as_str(), st.to.as_str(), &st.phase),
        (TAB, "9.9.9", &Phase::Pending)
    );
    assert_eq!(
        (st.salt, st.behind_since()),
        (100, 100),
        "its round and age"
    );
    assert_eq!(noted_for(&opts, "retargeted:9.9.8->9.9.9", SESSION), 1);
    let here = Opts {
        only_sid: Some(TAB.to_string()),
        ..opts.clone()
    };
    assert!(
        rows(&here).iter().any(|r| r.session == SESSION),
        "this tab's rows name it"
    );
    // In this tab now: left as it is, nothing said again.
    assert!(!mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    assert_eq!(ledger_lines(&opts), 1);
    // A stopped round resting on this build: moved into this tab, unchanged.
    let resting = waiting(Phase::Failed("no-resume".to_string()), "9.9.9");
    save(&opts, SESSION, &resting);
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    let st = load(&opts, SESSION).expect("the record");
    assert_eq!((st.tab.as_str(), &st.phase), (TAB, &resting.phase));
    assert_eq!(ledger_lines(&opts), 1, "no build changed, nothing said");
    // Negative control: an announced round in another tab.
    let announced = waiting(Phase::Announced { at_s: 100, asks: 1 }, "9.9.8");
    save(&opts, SESSION, &announced);
    assert!(!mint_behind(&opts, &sf, TAB, &newer(), false, now_s()));
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!((st.tab.as_str(), st.to.as_str()), (elsewhere, "9.9.8"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A bash, a dialect the relaunch line is written for, alive until the test lets it go: it
/// blocks in its own `read` of a pipe the handle holds, so it never ends on a clock a slow
/// test outlives (a gone bash is a `<defunct>` zombie, a shell with no dialect). It holds
/// that pipe and its stdio, nothing else (see this file's `Command` import).
fn stand_in_shell() -> aterm_uds::spawnfd::Child {
    Command::new("/bin/bash")
        .args(["-c", "read -r _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("bash")
}

/// The owner's `Skip` and `Not today` hold a crash's relaunch: a build the conversation's
/// record holds the session off for this tab, or one that failed to start for it while its
/// stop's rest stands ([`St::holds_off`]), is no relaunch target; the agent comes back on
/// the program it ran. Negative controls: no word, a lapsed deferral, another tab's word,
/// `--now`, a newer build, a rest run out, or a stop for another reason or build, move it.
#[cfg(unix)]
#[test]
fn a_build_the_conversations_record_holds_it_off_is_no_relaunch_target() {
    // Canonical: the native link is read through `canonicalize`, which a
    // temp dir's `/var` → `/private/var` would otherwise move out from under
    // the install's root.
    let dir = std::fs::canonicalize(scratch("relaunch-hold")).expect("scratch");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    // A native install whose current build is far newer than the session's.
    let versions = native_root(&opts.home);
    std::fs::create_dir_all(&versions).expect("versions");
    for build in ["2.1.281", "99.9.9"] {
        std::fs::write(versions.join(build), "").expect("a build");
    }
    std::fs::create_dir_all(opts.home.join(".local/bin")).expect("bin");
    std::os::unix::fs::symlink(versions.join("99.9.9"), opts.home.join(".local/bin/claude"))
        .expect("the native link");
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("sessions");
    file_transcript_in(&opts.home.join(".claude"), "p");
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let mut shell = stand_in_shell();
    let dead = dead_pid();
    let start = "Thu Sep 24 01:02:03 2026";
    // The crash left Claude's record of it.
    std::fs::write(
        opts.home.join(format!(".claude/sessions/{dead}.json")),
        format!(
            r#"{{"pid":{dead},"sessionId":"{SESSION}","cwd":"/","version":"2.1.281","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("the crash's record");
    let program = versions.join("2.1.281");
    let snap = Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: dead,
        start: start.to_string(),
        shell: shell.id(),
        program: program.clone(),
        argv: vec![
            program.to_string_lossy().into_owned(),
            "--verbose".to_string(),
        ],
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("2.1.281".to_string()),
        codex: None,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let now = now_s();
    let relaunched_to = |request: Request, request_tab: &str| {
        let st = St {
            phase: Phase::Pending,
            from: "2.1.281".to_string(),
            to: "99.9.9".to_string(),
            source: "native".to_string(),
            tab: TAB.to_string(),
            request,
            request_tab: request_tab.to_string(),
            request_at: now - 60,
            pending_since: now - 7_200,
            salt: now - 7_200,
            ..St::default()
        };
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("the record");
        let r = after_exit(&opts, &snap, &ExitRecord::Unread, true, None);
        assert_eq!(r.step, "would-relaunch", "{r:?}");
        r.to
    };
    let held = "2.1.281(same)";
    let moved = "99.9.9(native)";
    assert_eq!(
        relaunched_to(Request::Skip("99.9.9".into()), TAB),
        held,
        "skip"
    );
    assert_eq!(
        relaunched_to(Request::DeferUntil(now + 3_600), TAB),
        held,
        "not today"
    );
    // Negative controls.
    assert_eq!(relaunched_to(Request::None, ""), moved, "no word");
    assert_eq!(
        relaunched_to(Request::DeferUntil(now - 1), TAB),
        moved,
        "a deferral run out"
    );
    assert_eq!(
        relaunched_to(Request::Skip("99.9.9".into()), "s-0ther"),
        moved,
        "a word on another tab"
    );
    assert_eq!(relaunched_to(Request::Now, TAB), moved, "--now");
    assert_eq!(
        relaunched_to(Request::Skip("99.9.8".into()), TAB),
        moved,
        "a build newer than the skipped one"
    );
    // A build that failed to start for the conversation holds its relaunch the same way
    // while the stop's rest stands ([`St::holds_off`]), whatever ended the agent.
    use super::super::restart_stamp::Cause;
    let stopped = |why: &str, to: &str, ago: u64| St {
        phase: Phase::Failed(why.to_string()),
        from: "2.1.281".to_string(),
        to: to.to_string(),
        source: "native".to_string(),
        tab: TAB.to_string(),
        proc_start: "B".to_string(),
        pending_since: now - 7_200,
        salt: now - 7_200,
        failed_at: now - ago,
        streak_why: why.to_string(),
        stop_streak: 1,
        ..St::default()
    };
    let relaunched_over = |st: &St, how: &str| {
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("the record");
        let r = match how {
            "crash" => after_exit(&opts, &snap, &ExitRecord::Unread, true, None),
            "stall" => after_exit(&opts, &snap, &ExitRecord::Unread, true, Some(Cause::Stall)),
            "asked" => after_exit(&opts, &snap, &ExitRecord::Unread, true, Some(Cause::Asked)),
            _ => after_host_ended(&opts, &snap, true),
        };
        assert_eq!(r.step, "would-relaunch", "{how}: {r:?}");
        r.to
    };
    for how in ["crash", "stall", "asked", "host-ended"] {
        assert_eq!(
            relaunched_over(&stopped("no-resume", "99.9.9", 600), how),
            held,
            "{how}: inside the stop's rest"
        );
        // Negative controls: the rest run out; another reason, the process's
        // or its line's; a stop of an older build.
        assert_eq!(
            relaunched_over(&stopped("no-resume", "99.9.9", upgrade::RETRY_S + 60), how),
            moved,
            "{how}: the rest run out"
        );
        assert_eq!(
            relaunched_over(&stopped("resumed-elsewhere", "99.9.9", 600), how),
            moved,
            "{how}: another reason"
        );
        assert_eq!(
            relaunched_over(&stopped("no-resume", "99.9.8", 600), how),
            moved,
            "{how}: a stop of an older build"
        );
    }
    // Past the rest, the relaunch onto that build tries it again: its record
    // keeps the stop in a row, so a stop there again repeats and rests longer
    // ([`St::retried_by`]); onto another build, it is a first stop.
    let stopped_on = |prior: &St, to: &str| {
        let mut st = St {
            from: "2.1.281".to_string(),
            to: to.to_string(),
            source: "native".to_string(),
            cause: CAUSE_EXIT.to_string(),
            ..St::default()
        };
        prior.retried_by(&mut st);
        st.stop("no-resume", now);
        st.stop_streak
    };
    let rested = stopped("no-resume", "99.9.9", upgrade::RETRY_S + 60);
    assert_eq!(stopped_on(&rested, "99.9.9"), 2, "a stop that repeats");
    assert_eq!(stopped_on(&rested, "99.9.10"), 1, "another build");
    // A build that failed to start for another conversation, not proved to start since
    // ([`build_starts`]): this conversation's record holds nothing, yet its crash comes
    // back on the program it ran. Control: proved to start since, it is moved onto.
    let _ = std::fs::remove_file(state_path(&opts, SESSION));
    let writes = Opts {
        dry_run: false,
        ..opts.clone()
    };
    let elsewhere = St {
        agent: upgrade::Agent::Claude,
        source: "native".to_string(),
        relaunched_at: now - 40,
        ..stopped("no-resume", "99.9.9", 34)
    };
    save(&writes, "0badf00d-9999-2222-3333-444455556666", &elsewhere);
    let crash = || after_exit(&opts, &snap, &ExitRecord::Unread, true, None);
    let r = crash();
    assert_eq!((r.step.as_str(), r.to.as_str()), ("would-relaunch", held));
    let native = Candidate {
        exe: versions.join("99.9.9"),
        version: Version::parse("99.9.9").expect("v"),
        source: Source::Native,
    };
    assert_eq!(
        build_start::starts_with(&writes, &r, &native, now, false, |_| {
            build_start::Probe::Starts
        }),
        Ok(())
    );
    let r = crash();
    assert_eq!((r.step.as_str(), r.to.as_str()), ("would-relaunch", moved));
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A restart or relaunch on its own program keeps the owner's hold and the move clock
/// ([`St::kept_by_restart`]), and the round after it keeps them too ([`St::for_target`]),
/// so the quiet flow never moves the session onto a build the owner skipped. Negative
/// controls: a skip does not follow onto a newer build; a lapsed deferral, `--now` and a
/// word on another tab are not kept; a finished move keeps no clock.
#[test]
fn a_restart_on_its_own_program_keeps_the_owners_hold_and_the_round_after_it_too() {
    use super::super::relaunch::CAUSE_MEMORY;
    let now = 1_790_000_000_u64;
    let running = Version::parse("2.1.281").expect("running");
    let build = |v: &str| Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: Version::parse(v).expect("target"),
        source: Source::Managed,
    };
    let upgrade_record = |request: Request, request_tab: &str| St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.300".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        request,
        request_tab: request_tab.to_string(),
        request_at: now - 60,
        pending_since: now - 7_200,
        salt: now - 7_200,
        ..St::default()
    };
    let restarted = |prior: &St| {
        let mut st = St {
            phase: Phase::Done,
            from: "2.1.281".to_string(),
            to: "2.1.281".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: CAUSE_MEMORY.to_string(),
            salt: now,
            ..St::default()
        };
        prior.kept_by_restart(TAB, &mut st);
        st
    };
    let skip = Request::Skip("2.1.300".to_string());
    let restart = restarted(&upgrade_record(skip.clone(), TAB));
    assert_eq!(restart.request_for(TAB), skip, "the restart's record");
    assert_eq!(restart.behind_since(), now - 7_200);
    let next = St::for_target(
        Some(restart.clone()),
        &running,
        &build("2.1.300"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.phase, Phase::Pending);
    assert_eq!(next.request_for(TAB), skip, "the round after it");
    assert!(next.request.holds(&next.phase, "2.1.300", now));
    assert_eq!(next.behind_since(), now - 7_200, "the move clock");
    // Negative control: a skip names one build.
    let newer = St::for_target(
        Some(restart),
        &running,
        &build("2.1.301"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(newer.request, Request::None);
    assert_eq!(newer.behind_since(), now - 7_200, "still behind since then");
    // A deferral standing is kept; one run out, `--now`, another tab's word
    // are not.
    let standing = Request::DeferUntil(now + 3_600);
    let kept = restarted(&upgrade_record(standing.clone(), TAB));
    let next = St::for_target(
        Some(kept),
        &running,
        &build("2.1.300"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.request_for(TAB), standing);
    for (word, tab) in [
        (Request::DeferUntil(now - 1), TAB),
        (Request::Now, TAB),
        (skip.clone(), "s-0ther"),
    ] {
        let r = restarted(&upgrade_record(word.clone(), tab));
        let next = St::for_target(
            Some(r),
            &running,
            &build("2.1.300"),
            None,
            Proc::default(),
            now,
        );
        assert!(
            !next.request.holds(&next.phase, "2.1.300", now),
            "{word:?} {tab}: {next:?}"
        );
    }
    // Negative control: a finished move onto its build is behind anew.
    let moved = St {
        phase: Phase::Done,
        from: "2.1.280".to_string(),
        to: "2.1.281".to_string(),
        source: "managed".to_string(),
        pending_since: now - 86_400,
        salt: now - 86_400,
        ..St::default()
    };
    let next = St::for_target(
        Some(moved),
        &running,
        &build("2.1.300"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.behind_since(), now);
}

/// A restart on the session's own program that stops keeps the owner's `Skip` of the
/// build it held off ([`St::kept_by_restart`]), with the move clock: the retarget's stopped
/// arm keeps a skip that still names the new target (`St::word_onto`). Negative controls:
/// an upgrade round's own skip of its target, and the restart's onto a newer build, do
/// not follow.
#[test]
fn a_restart_that_stops_keeps_the_owners_skip_of_the_build_it_held_off() {
    use super::super::relaunch::CAUSE_EXIT;
    let now = 1_790_000_000_u64;
    let running = Version::parse("2.1.281").expect("running");
    let build = |v: &str| Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: Version::parse(v).expect("target"),
        source: Source::Managed,
    };
    let skip = Request::Skip("2.1.300".to_string());
    let upgrade_round = St {
        phase: Phase::Pending,
        from: "2.1.281".to_string(),
        to: "2.1.300".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        request: skip.clone(),
        request_tab: TAB.to_string(),
        request_at: now - 60,
        pending_since: now - 7_200,
        salt: now - 7_200,
        ..St::default()
    };
    for why in ["no-resume", "resumed-elsewhere", "relaunch-refused"] {
        let mut restart = St {
            phase: Phase::Exiting { at_s: now - 30 },
            from: "2.1.281".to_string(),
            to: "2.1.281".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: CAUSE_EXIT.to_string(),
            salt: now - 30,
            ..St::default()
        };
        upgrade_round.kept_by_restart(TAB, &mut restart);
        restart.stop(why, now - 10);
        assert!(matches!(restart.phase, Phase::Failed(_)), "{why}");
        let (next, how) = St::retarget(
            Some(restart.clone()),
            &running,
            &build("2.1.300"),
            None,
            Proc::default(),
            now,
        );
        assert_eq!(how, Retarget::Anew, "{why}: the stopped arm");
        assert_eq!(next.phase, Phase::Pending, "{why}");
        assert_eq!(next.request_for(TAB), skip, "{why}: the owner's skip");
        assert!(next.request.holds(&next.phase, "2.1.300", now), "{why}");
        assert_eq!(next.behind_since(), now - 7_200, "{why}: the move clock");
        // Negative control: a skip names one build.
        let newer = St::for_target(
            Some(restart),
            &running,
            &build("2.1.301"),
            None,
            Proc::default(),
            now,
        );
        assert_eq!(newer.request, Request::None, "{why}");
    }
    // Negative control: the upgrade round's own skip, its round stopped, does not follow
    // onto the newer build (it names the build that round moved to).
    let mut stopped = upgrade_round.clone();
    stopped.stop("relaunch-refused", now - 10);
    let next = St::for_target(
        Some(stopped),
        &running,
        &build("2.1.301"),
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.request, Request::None);
    assert!(!next.request.holds(&next.phase, "2.1.301", now));
}

/// [`St::kept_by_restart`] keeps the move clock only of a record about the build the
/// restart keeps the session on: a stopped round for another build starts the clock afresh,
/// as the retarget's stopped arm does, so the next build's round begins at its first rung.
/// Negative controls: a round about the kept build, and a stopped restart of it, keep the
/// clock; the owner's skip is kept whatever build its record is about.
#[test]
fn a_restart_keeps_the_move_clock_only_of_a_record_about_the_build_it_keeps() {
    use super::super::relaunch::{CAUSE_EXIT, CAUSE_MEMORY};
    let now = 1_790_000_000_u64;
    let t0 = now - 5 * 86_400;
    let running = Version::parse("2.1.290").expect("running");
    let newer = Candidate {
        exe: PathBuf::from("/p/agents/claude"),
        version: Version::parse("2.1.300").expect("target"),
        source: Source::Managed,
    };
    // The restart's own record, on the build the session runs now.
    let restarted = |prior: &St| {
        let mut st = St {
            phase: Phase::Done,
            from: "2.1.290".to_string(),
            to: "2.1.290".to_string(),
            source: SAME_PROGRAM.to_string(),
            cause: CAUSE_MEMORY.to_string(),
            salt: now,
            ..St::default()
        };
        prior.kept_by_restart(TAB, &mut st);
        st
    };
    // The round for 2.1.290, stopped: the person resumed it there by hand.
    let resumed_by_hand = St {
        phase: Phase::Failed("resumed-elsewhere".to_string()),
        from: "2.1.281".to_string(),
        to: "2.1.290".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: t0,
        salt: t0,
        failed_at: now - 3_600,
        ..St::default()
    };
    let restart = restarted(&resumed_by_hand);
    assert_eq!(restart.behind_since(), now, "behind anew from the restart");
    let next = St::for_target(Some(restart), &running, &newer, None, Proc::default(), now);
    assert_eq!(next.behind_since(), now, "the next build's round");
    assert_eq!(next.rung_at(now), upgrade::Rung::Prefer, "its first rung");
    // As the retarget rules it with no restart in between.
    let direct = St::for_target(
        Some(resumed_by_hand.clone()),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(direct.behind_since(), now);
    // An older restart's record, left from before the move by hand.
    let older_restart = St {
        phase: Phase::Done,
        from: "2.1.281".to_string(),
        to: "2.1.281".to_string(),
        source: SAME_PROGRAM.to_string(),
        cause: CAUSE_MEMORY.to_string(),
        pending_since: t0,
        salt: t0,
        ..St::default()
    };
    assert_eq!(restarted(&older_restart).behind_since(), now);
    // Negative control: a round about the build the restart keeps.
    let round_on_it = St {
        phase: Phase::Pending,
        from: "2.1.290".to_string(),
        to: "2.1.300".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: t0,
        salt: t0,
        ..St::default()
    };
    assert_eq!(restarted(&round_on_it).behind_since(), t0);
    // A stopped restart of the same program is still behind: its clock is kept by the
    // restart after it, as by the retarget.
    let mut stopped_relaunch = St {
        phase: Phase::Exiting { at_s: now - 60 },
        from: "2.1.290".to_string(),
        to: "2.1.290".to_string(),
        source: SAME_PROGRAM.to_string(),
        cause: CAUSE_EXIT.to_string(),
        tab: TAB.to_string(),
        pending_since: t0,
        salt: now - 60,
        ..St::default()
    };
    stopped_relaunch.stop("exited-before-continuing", now - 30);
    assert_eq!(restarted(&stopped_relaunch).behind_since(), t0);
    let direct = St::for_target(
        Some(stopped_relaunch.clone()),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(direct.behind_since(), t0, "the retarget keeps it too");
    // Negative control: a stopped restart of another build keeps no clock.
    let other = St {
        from: "2.1.281".to_string(),
        to: "2.1.281".to_string(),
        ..stopped_relaunch
    };
    assert_eq!(restarted(&other).behind_since(), now);
    // The owner's skip of 2.1.300 on a stopped round about 2.1.281: kept,
    // its clock not.
    let skip = Request::Skip("2.1.300".to_string());
    let skipped = St {
        to: "2.1.300".to_string(),
        request: skip.clone(),
        request_tab: TAB.to_string(),
        request_at: now - 60,
        ..resumed_by_hand
    };
    let restart = restarted(&skipped);
    assert_eq!(restart.request_for(TAB), skip);
    assert_eq!(restart.behind_since(), now);
    let next = St::for_target(Some(restart), &running, &newer, None, Proc::default(), now);
    assert!(next.request.holds(&next.phase, "2.1.300", now), "{next:?}");
    // As the retarget holds it with no restart in between.
    let direct = St::for_target(Some(skipped), &running, &newer, None, Proc::default(), now);
    assert!(direct.request.holds_build("2.1.300", now), "{direct:?}");
}

/// A stopped round's retarget keeps what tells the Codex lane its TUI was told to wind
/// down (that lane types no release, and carries a TUI on after its relaunch only where a
/// notice reached it, [`St::noticed_at`], or its round before gave up, [`St::last_stop`]):
/// the stopped and abandoning arms of [`St::retarget`] carry both. Negative controls: a
/// Codex stop for another reason with no notice carries nothing; a Claude Code record
/// keeps no stop.
#[test]
fn a_stopped_rounds_retarget_keeps_what_says_its_tui_was_told_to_wind_down() {
    let now = 1_790_000_000_u64;
    let running = Version::parse("0.157.0").expect("running");
    let newer = Candidate {
        exe: PathBuf::from("/p/agents/codex"),
        version: Version::parse("0.157.2").expect("target"),
        source: Source::Managed,
    };
    let round = |agent: upgrade::Agent, phase: Phase, noticed_at: u64| St {
        agent,
        phase,
        from: "0.157.0".to_string(),
        to: "0.157.1".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        noticed_at,
        failed_at: now - 60,
        pending_since: now - 7_200,
        salt: now - 7_200,
        ..St::default()
    };
    let gave_up = Phase::Failed(upgrade::GAVE_UP.to_string());
    let codex = upgrade::Agent::Codex;
    // A stamped notice, given up on: both kept.
    let next = St::for_target(
        Some(round(codex, gave_up.clone(), now - 3_600)),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.phase, Phase::Pending);
    assert_eq!(next.noticed_at, now - 3_600);
    assert_eq!(next.last_stop, upgrade::GAVE_UP);
    // A give-up an older build recorded with no stamp: the stop alone.
    let next = St::for_target(
        Some(round(codex, gave_up.clone(), 0)),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(
        (next.noticed_at, next.last_stop.as_str()),
        (0, upgrade::GAVE_UP)
    );
    // Another stop after a notice: its stamp.
    let refused = Phase::Failed("signal-refused".to_string());
    let next = St::for_target(
        Some(round(codex, refused.clone(), now - 3_600)),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(
        (next.noticed_at, next.last_stop.as_str()),
        (now - 3_600, "")
    );
    // The abandoning arm (a round for another model): the stamp, and a re-armed
    // give-up's stop.
    let announced = St {
        model_list: "gpt-5.5".to_string(),
        ..round(
            codex,
            Phase::Announced {
                at_s: now - 3_600,
                asks: 1,
            },
            now - 3_600,
        )
    };
    let (next, how) = St::retarget(
        Some(announced),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(how, Retarget::Anew);
    assert_eq!(next.noticed_at, now - 3_600);
    let rearmed = St {
        model_list: "gpt-5.5".to_string(),
        last_stop: upgrade::GAVE_UP.to_string(),
        ..round(codex, Phase::Pending, 0)
    };
    let next = St::for_target(Some(rearmed), &running, &newer, None, Proc::default(), now);
    assert_eq!(next.last_stop, upgrade::GAVE_UP);
    // Negative controls: nothing asked, nothing carried; a Claude Code
    // record keeps no stop.
    let next = St::for_target(
        Some(round(codex, refused, 0)),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!((next.noticed_at, next.last_stop.as_str()), (0, ""));
    let next = St::for_target(
        Some(round(upgrade::Agent::Claude, gave_up, now - 3_600)),
        &running,
        &newer,
        None,
        Proc::default(),
        now,
    );
    assert_eq!(next.last_stop, "");
}

/// The relaunch on exit relaunches only a crashed agent (its own record left behind) on
/// the conversation that record names, and carries on one in flight rather than typing a
/// second. Dry runs. Negative controls: an agent still running; no conversation on record
/// and a one-shot run (the launch's own end); a graceful exit (record removed), someone's
/// decision.
#[test]
fn a_relaunch_on_exit_needs_an_exit_and_a_conversation_on_record() {
    let dir = scratch("after-exit");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("home");
    // The conversations a crash leaves have messages: their transcripts.
    let project = opts.home.join(".claude/projects/-w");
    let later = "0badf00d-9999-2222-3333-444455556666";
    let transcripts = || {
        std::fs::create_dir_all(&project).expect("projects");
        for s in [SESSION, later] {
            std::fs::write(project.join(format!("{s}.jsonl")), "{}\n").expect("transcript");
        }
    };
    transcripts();
    let mut shell = stand_in_shell();
    let shell_pid = shell.id();
    let snap = |pid: u32, start: &str, session: Option<&str>| Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid,
        start: start.to_string(),
        shell: shell_pid,
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec![
            "/opt/claude/bin/claude".to_string(),
            "--verbose".to_string(),
        ],
        session: session.map(str::to_string),
        cwd: "/".to_string(),
        version: None,
        codex: None,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    // Each step reads Claude's record at the attempt: an exit nothing could
    // read as it was seen ([`ExitRecord::Unread`]; the look at the exit is the
    // next test's).
    let at_attempt = |snap: &Snapshot, stalled: bool| {
        after_exit(
            &opts,
            snap,
            &ExitRecord::Unread,
            true,
            stalled.then_some(super::super::restart_stamp::Cause::Stall),
        )
    };
    // Still running: nothing exited.
    let mut child = parked().spawn().expect("agent");
    wait_exec(child.id());
    let start = kernel_start(child.id()).expect("lstart");
    let r = at_attempt(&snap(child.id(), &start, Some(SESSION)), false);
    assert_eq!(r.step, "wait:not-exited");
    child.kill().expect("kill");
    child.wait().expect("reap");
    // Gone, and no record names its conversation.
    let dead = dead_pid();
    let start = "Thu Sep 24 01:02:03 2026";
    let r = at_attempt(&snap(dead, start, None), false);
    assert_eq!(r.step, "ended:no-conversation");
    // A one-shot run, whatever record names it: its exit was its end.
    let mut once = snap(dead, start, Some(SESSION));
    once.argv
        .extend(["-p".to_string(), "fix the bug".to_string()]);
    assert_eq!(at_attempt(&once, false).step, "ended:one-shot");
    // A graceful exit — Claude removed its record — was someone's...
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "ended:graceful-exit");
    // ...unless its stall was held: the stall's remedy ended it (`signal
    // term`, whose handler ran), and it is relaunched on its conversation.
    let r = at_attempt(&snap(dead, start, Some(SESSION)), true);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("would-relaunch", SESSION)
    );
    // A crash left its record: relaunched on the conversation it names.
    let crash_record = |pid: u32, session: &str| {
        std::fs::write(
            opts.home.join(format!(".claude/sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"x","status":"busy","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the crash's record");
    };
    crash_record(dead, SESSION);
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("would-relaunch", SESSION)
    );
    // A conversation Claude filed no transcript for (no message yet) cannot
    // be resumed: the agent is started afresh — and so in a Claude home with
    // no projects at all (Claude makes them with the home's first message).
    // Negative control: projects that exist and cannot be listed say nothing,
    // and the conversation is resumed (Claude says if it cannot).
    let projects = opts.home.join(".claude/projects");
    std::fs::remove_file(project.join(format!("{SESSION}.jsonl"))).expect("rm");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch:fresh");
    std::fs::remove_dir_all(&projects).expect("rm");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch:fresh", "no projects yet");
    std::fs::write(&projects, "").expect("not a directory");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch", "unreadable projects");
    std::fs::remove_file(&projects).expect("rm");
    transcripts();
    // ...and the crash's surviving record wins over the snapshot: it names
    // the conversation as the agent last held it.
    crash_record(dead, later);
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("would-relaunch", later)
    );
    // A record of another process's start is not this agent's: its own is gone, a
    // graceful exit.
    let r = at_attempt(
        &snap(dead, "Mon Jan  1 00:00:00 2024", Some(SESSION)),
        false,
    );
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("ended:graceful-exit", SESSION)
    );
    // A relaunch already in flight for the conversation is carried on...
    let st = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: dead,
        shell: shell_pid,
        tab: TAB.to_string(),
        cause: super::super::relaunch::CAUSE_EXIT.to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, later), st.to_json()).expect("in flight");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-resume:exiting");
    // ...unless it is another process's (with no relaunch line typed yet, one
    // a person resumed by hand in the tab): closed, and this exit relaunched
    // afresh.
    let other = dead_pid();
    crash_record(other, later);
    let r = at_attempt(&snap(other, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch", "{r:?}");
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{other}.json"))).expect("rm");
    // ...unless it is too old to act on: closed, and a fresh one planned.
    let stale = St {
        phase: Phase::Exiting {
            at_s: now_s() - STALE_S - 1,
        },
        ..st.clone()
    };
    std::fs::write(state_path(&opts, later), stale.to_json()).expect("stale");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch");
    std::fs::write(state_path(&opts, later), st.to_json()).expect("in flight");
    // ...in its own tab only.
    let elsewhere = Snapshot {
        tab: "s-0ther".to_string(),
        ..snap(dead, start, Some(SESSION))
    };
    assert_eq!(
        at_attempt(&elsewhere, false).step,
        "wait:conversation-in-other-tab"
    );
    // The shell gone: there is no prompt to relaunch at, ever.
    std::fs::remove_file(state_path(&opts, later)).expect("rm");
    shell.kill().expect("kill");
    shell.wait().expect("reap");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "refused:shell-gone");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dead agent of the relaunch tests' tab, its stand-in shell alive, and
/// the path of Claude's own record of it, with the writer of that record.
struct DeadAgent {
    opts: Opts,
    dir: ScratchDir,
    shell: aterm_uds::spawnfd::Child,
    snap: Snapshot,
    record: PathBuf,
}

impl DeadAgent {
    fn new(tag: &str) -> Self {
        let dir = scratch(tag);
        let opts = Opts {
            dry_run: true,
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        let sessions = opts.home.join(".claude/sessions");
        std::fs::create_dir_all(&sessions).expect("home");
        file_transcript_in(&opts.home.join(".claude"), "p");
        let shell = stand_in_shell();
        let dead = dead_pid();
        let snap = Snapshot {
            stored_login: false,
            tab: TAB.to_string(),
            pid: dead,
            start: "Thu Sep 24 01:02:03 2026".to_string(),
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: None,
            codex: None,
            dialect: Some(Dialect::Zsh),
            login: None,
            home: None,
            claude_dir: None,
        };
        let record = sessions.join(format!("{dead}.json"));
        DeadAgent {
            opts,
            dir,
            shell,
            snap,
            record,
        }
    }

    /// Claude's own record of the agent, as it wrote it while it ran.
    fn write_record(&self) {
        std::fs::write(&self.record, self.record_text(SESSION)).expect("the record");
    }

    /// [`DeadAgent::write_record`] holding conversation `session`, filed in
    /// the Claude Code directory `claude_dir` itself — where an agent started
    /// with `CLAUDE_CONFIG_DIR=<claude_dir>` files it.
    fn write_record_in(&self, claude_dir: &Path, session: &str) {
        let sessions = claude_dir.join("sessions");
        std::fs::create_dir_all(&sessions).expect("sessions");
        std::fs::write(
            sessions.join(format!("{}.json", self.snap.pid)),
            self.record_text(session),
        )
        .expect("the record");
    }

    /// The record's text, naming conversation `session`.
    fn record_text(&self, session: &str) -> String {
        format!(
            r#"{{"pid":{},"sessionId":"{session}","cwd":"/","version":"2.1.283","status":"busy","statusUpdatedAt":1,"procStart":"{}","kind":"interactive","entrypoint":"cli"}}"#,
            self.snap.pid, self.snap.start
        )
    }

    /// The record removed: by the exit itself, or by another Claude Code.
    fn remove_record(&self) {
        std::fs::remove_file(&self.record).expect("the record was there");
    }

    /// The host's look at the exit, live, its waits simulated (`wait` sees
    /// each look's number before the next).
    fn look(&self, mut wait: impl FnMut(usize)) -> ExitRecord {
        let looks = std::cell::Cell::new(0);
        exit_record(
            || {
                looks.set(looks.get() + 1);
                Some(look_at_exit(&self.opts.home, &self.snap))
            },
            |step| {
                assert_eq!(step, EXIT_LOOK);
                wait(looks.get());
                true
            },
        )
    }

    /// One attempt at the relaunch, handed what the exit left: its word.
    fn attempt(&self, left: &ExitRecord) -> String {
        after_exit(&self.opts, &self.snap, left, true, None).step
    }
}

impl Drop for DeadAgent {
    fn drop(&mut self) {
        let _ = self.shell.kill();
        let _ = self.shell.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What the exit left is read as the exit is seen, and kept: a crash's record another
/// Claude Code removes during the back-off is still relaunched, on the conversation the
/// look kept. Negative controls: a graceful exit whose record goes inside the settle
/// (`/exit` removes its own in its last moments) is left; a read at the attempt reads the
/// crash as graceful.
#[test]
fn a_crash_read_at_its_exit_is_relaunched_whatever_removes_its_record_later() {
    let a = DeadAgent::new("exit-record");
    // The same process still running reads as running; this one is gone.
    let mut child = parked().spawn().expect("agent");
    wait_exec(child.id());
    let running = Snapshot {
        pid: child.id(),
        start: kernel_start(child.id()).expect("lstart"),
        ..a.snap.clone()
    };
    assert!(look_at_exit(&a.opts.home, &running).running);
    child.kill().expect("kill");
    child.wait().expect("reap");
    assert!(!look_at_exit(&a.opts.home, &a.snap).running);
    // The crash: its record survives the settle, and the look keeps it.
    a.write_record();
    let mut waits = 0_u32;
    let left = a.look(|_| waits += 1);
    assert!(
        matches!(&left, ExitRecord::Survived(sf) if sf.session_id == SESSION),
        "{left:?}"
    );
    assert_eq!(
        u128::from(waits),
        EXIT_SETTLE.as_millis() / EXIT_LOOK.as_millis(),
        "decided once the settle passed"
    );
    // Another Claude Code starts during the back-off and removes it.
    a.remove_record();
    assert_eq!(a.attempt(&left), "would-relaunch");
    let r = after_exit(&a.opts, &a.snap, &left, true, None);
    assert_eq!(r.session, SESSION, "the conversation the look kept");
    // Read at the attempt instead, the record says graceful.
    assert_eq!(a.attempt(&ExitRecord::Unread), "ended:graceful-exit");
    // …unless an attempt of this exit relaunched it and the relaunched agent was gone
    // again before anything read it (the window's host tries the exit again): the crash
    // that relaunch read stands, since the relaunched Claude Code's start swept the record.
    let writes = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&writes)).expect("state");
    save(
        &writes,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now_s() - 10 },
            pid: a.snap.pid,
            shell: a.snap.shell,
            tab: TAB.to_string(),
            cause: super::super::relaunch::CAUSE_EXIT.to_string(),
            relaunched_pid: dead_pid(),
            ..St::default()
        },
    );
    assert_eq!(a.attempt(&ExitRecord::Unread), "would-relaunch");
    remove(&writes, SESSION);
    // The graceful exit: its record still there at the first look, removed
    // by the exit a moment later, inside the settle.
    a.write_record();
    let mut first_saw = false;
    let left = a.look(|looks| {
        if looks == 1 {
            first_saw = a.record.exists();
            a.remove_record();
        }
    });
    assert!(first_saw, "the first look saw the record");
    assert_eq!(left, ExitRecord::Removed);
    assert_eq!(a.attempt(&left), "ended:graceful-exit");
    // …unless the stall's remedy ended it: relaunched all the same.
    let r = after_exit(
        &a.opts,
        &a.snap,
        &left,
        true,
        Some(super::super::restart_stamp::Cause::Stall),
    );
    assert_eq!(r.step, "would-relaunch");
    // A record of another process's start is no record of this agent's.
    a.write_record();
    let reused = Snapshot {
        start: "Mon Jan  1 00:00:00 2024".to_string(),
        ..a.snap.clone()
    };
    assert_eq!(look_at_exit(&a.opts.home, &reused).record, None);
}

/// An exit the harness's own restart made (a graceful end, its record removed) is never
/// read as someone's: a restart left in flight past [`STALE_S`] is stopped, its agent's
/// end stamped, and said (`told:<why>` for the upgrade's causes, `refused:<why>` for the
/// restart in place's), again while it stands; one relaunched and never registered is never
/// typed afresh. Negative controls: a relaunch on exit's record, another process's record,
/// and a refused signal fall through.
#[test]
fn an_exit_the_harness_own_restart_made_is_never_read_as_graceful() {
    use super::super::relaunch::{CAUSE_EXIT, CAUSE_HOST, CAUSE_MEMORY, CAUSE_UPGRADE_FRESH};
    let a = DeadAgent::new("own-exit");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let old = now_s() - STALE_S - 1;
    let record = |phase: Phase, cause: &str, pid: u32| St {
        phase,
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        cause: cause.to_string(),
        ..St::default()
    };
    let attempt = || after_exit(&opts, &a.snap, &ExitRecord::Removed, true, None).step;
    // An upgrade's record, which the owner's view lists, is said by its own
    // row (`told:`); a restart in place's by the host (`refused:`).
    for (cause, said) in [
        ("", "told:stale-exit"),
        (CAUSE_UPGRADE_FRESH, "told:stale-exit"),
        (CAUSE_MEMORY, "refused:stale-exit"),
        ("model:claude-fable-5-1", "refused:stale-exit"),
        ("model-back:", "refused:stale-exit"),
    ] {
        save(
            &opts,
            SESSION,
            &record(Phase::Exiting { at_s: old }, cause, a.snap.pid),
        );
        assert_eq!(attempt(), said, "{cause:?}");
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            st.phase,
            Phase::Failed("stale-exit".to_string()),
            "{cause:?}"
        );
        assert_ne!(st.exited_at, 0, "{cause:?}: its agent was seen gone");
        // Stopped after its exit: the next attempt says so again.
        assert_eq!(attempt(), said, "{cause:?}");
    }
    // Relaunched and never registered: said, never typed a second time.
    save(
        &opts,
        SESSION,
        &record(Phase::Relaunched { at_s: old }, "", a.snap.pid),
    );
    assert_eq!(attempt(), "told:no-resume");
    // Negative controls. A relaunch on exit's record (its agent crashed): this exit
    // reads as graceful, and the record too old to act on is closed as a stop
    // ([`St::stop`]), with the moment its agent was seen gone.
    save(
        &opts,
        SESSION,
        &St {
            marker: "ATERM-UPGRADE-READY-0badf00d".to_string(),
            markers: vec!["ATERM-UPGRADE-READY-0badf00d".to_string()],
            ..record(Phase::Exiting { at_s: old }, CAUSE_EXIT, a.snap.pid)
        },
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Failed("stale-exit".to_string()));
    assert_ne!(st.exited_at, 0, "its agent was seen gone");
    assert!(st.marker.is_empty() && st.markers.is_empty(), "{st:?}");
    // Another process's restart: not this exit's. Its relaunch line never
    // typed, the process that left is one a person resumed by hand: closed as
    // `resumed-elsewhere`, never as a relaunched agent of aterm's.
    save(
        &opts,
        SESSION,
        &record(Phase::Exiting { at_s: old }, "", dead_pid()),
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    assert_eq!(
        load(&opts, SESSION).expect("kept").phase,
        Phase::Failed("resumed-elsewhere".to_string())
    );
    // The relaunched agent left before its continuation, gracefully: the record closes as
    // no failure of the move (no stamp), so the status view shows no stall for a relaunch
    // that landed.
    for seen in [now_s() - 30, 0] {
        save(
            &opts,
            SESSION,
            &St {
                exited_at: seen,
                ..record(Phase::Relaunched { at_s: now_s() - 10 }, "", dead_pid())
            },
        );
        assert_eq!(attempt(), "ended:graceful-exit", "{seen}");
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            st.phase,
            Phase::Failed("exited-before-continuing".to_string()),
            "{seen}"
        );
        assert_eq!(st.exited_at, 0, "{seen}: {st:?}");
        let rows = super::upgrade_status::rows(&opts);
        assert!(
            rows.iter().all(|r| !r.failed_after_exit()),
            "{seen}: {rows:?}"
        );
        assert!(
            super::upgrade_status::status_rows(&opts, true).0.is_empty(),
            "{seen}: no stall shown"
        );
    }
    // The same, the exit tried again on the agent before it (the window's host): a
    // relaunch on exit that landed on a process gone already is closed as one overtaken.
    // Negative control: one whose adopted process lives is carried on.
    for (adopted, carried) in [(dead_pid(), false), (std::process::id(), true)] {
        save(
            &opts,
            SESSION,
            &St {
                relaunched_pid: adopted,
                ..record(
                    Phase::Relaunched { at_s: now_s() - 10 },
                    CAUSE_EXIT,
                    a.snap.pid,
                )
            },
        );
        let step = attempt();
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            (step.as_str(), st.in_flight()),
            if carried {
                ("wait:no-socket", true)
            } else {
                ("ended:graceful-exit", false)
            },
            "{adopted}: {st:?}"
        );
    }
    // A signal the kernel refused: its agent lived on, and its later exit is
    // someone's.
    save(
        &opts,
        SESSION,
        &record(Phase::Failed("signal-refused".to_string()), "", a.snap.pid),
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    // A relaunch after aterm's own end (`host`): its record ended no agent of its own,
    // so it is closed and the exit reads as graceful.
    save(
        &opts,
        SESSION,
        &record(Phase::Exiting { at_s: old }, CAUSE_HOST, a.snap.pid),
    );
    assert_eq!(attempt(), "ended:graceful-exit");
    // A tab aterm's own end took (`after_host_ended`): the upgrade's restart
    // left in flight for this agent in this tab is too old to act on, and the
    // tab restored after the crash is relaunched afresh, never refused as the
    // restart's (`stale-exit`).
    save(
        &opts,
        SESSION,
        &record(Phase::Exiting { at_s: old }, "", a.snap.pid),
    );
    assert_eq!(
        after_host_ended(&a.opts, &a.snap, true).step,
        "would-relaunch"
    );
}

/// The carry of an upgrade stopped after its exit answers `told:<why>` (the host's
/// `Cannot`): the owner's view already raises `agent-down` on the upgrade's own row with
/// the one remedy, so the host makes no second report. Negative control: a restart in
/// place's record (a model move, listed nowhere) is still `refused:<why>`, the host's.
#[test]
fn the_carry_of_an_upgrade_stopped_after_its_exit_leaves_the_ask_to_its_row() {
    use super::super::relaunch::{CAUSE_UPGRADE_FRESH, Outcome, carry_restart, outcome, restarted};
    let a = DeadAgent::new("carry-told");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    for (cause, why, want) in [
        ("", "no-resume", "told:no-resume"),
        (CAUSE_UPGRADE_FRESH, "no-resume", "told:no-resume"),
        ("", "stale-exit", "told:stale-exit"),
        ("model:claude-sonnet-5-5", "no-resume", "refused:no-resume"),
    ] {
        save(
            &opts,
            SESSION,
            &St {
                phase: Phase::Failed(why.to_string()),
                pid,
                shell: a.snap.shell,
                tab: TAB.to_string(),
                from: "2.1.281".to_string(),
                to: "2.1.283".to_string(),
                source: "managed".to_string(),
                cause: cause.to_string(),
                exited_at: now_s(),
                ..St::default()
            },
        );
        assert!(
            restarted(&opts, upgrade::Agent::Claude, Some(pid)),
            "{cause:?}"
        );
        assert_eq!(
            carry_restart(&opts, upgrade::Agent::Claude, Some(pid)).step,
            want,
            "{cause:?} {why}"
        );
    }
    assert_eq!(
        outcome("told:no-resume"),
        Outcome::Cannot("no-resume".to_string())
    );
}

/// [`restarted`] finds the restart that ended the agent that left (both lanes: a restart that
/// ended that process itself, filed for it in its tab) and [`carry_restart`] carries it on within
/// [`STALE_S`] of the exit, then `told:<why>`, never typed afresh. Negative controls: another
/// process, snapshot, tab or lane; a stop long past its exit; in the Codex lane a person's Codex
/// or a relaunch on exit's record.
#[test]
fn the_host_finds_and_carries_the_restart_that_ended_the_agent_that_left() {
    use super::super::relaunch::{carry_restart, restarted};
    let a = DeadAgent::new("carry-restart");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    let now = now_s();
    let claude = |phase: Phase, exited_at: u64| St {
        phase,
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        exited_at,
        ..St::default()
    };
    save(&opts, SESSION, &claude(Phase::Exiting { at_s: now }, 0));
    assert!(restarted(&opts, upgrade::Agent::Claude, Some(pid)));
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, Some(dead_pid())),
        "another process"
    );
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, None),
        "no snapshot proves nothing"
    );
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, None),
        "no Codex record"
    );
    let other_tab = Opts {
        only_sid: Some("s-0ther".to_string()),
        ..opts.clone()
    };
    assert!(
        !restarted(&other_tab, upgrade::Agent::Claude, Some(pid)),
        "another tab"
    );
    // Carried to the tab (no socket here).
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid)).step,
        "wait:no-socket"
    );
    // A shutdown that outlived the bound (a hung SessionEnd hook, an MCP
    // teardown): signalled more than STALE_S ago, its agent seen gone only by
    // this carry. The bound counts from that exit, never from the signal:
    // carried, and when it was seen gone kept for the next try.
    let signal = now - STALE_S - 60;
    save(&opts, SESSION, &claude(Phase::Exiting { at_s: signal }, 0));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid)).step,
        "wait:no-socket",
        "carried from its exit"
    );
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Exiting { at_s: signal });
    assert!(st.exited_at >= now, "seen gone at the carry: {st:?}");
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid)).step,
        "wait:no-socket",
        "and again at the next try"
    );
    // Past its bound from the exit: stopped, said, and still this exit's.
    save(
        &opts,
        SESSION,
        &claude(Phase::Exiting { at_s: signal }, now - STALE_S - 1),
    );
    let r = carry_restart(&opts, upgrade::Agent::Claude, Some(pid));
    // The upgrade's own row asks the owner about it (`told:`).
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("told:stale-exit", SESSION)
    );
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Failed("stale-exit".to_string()));
    assert_ne!(st.exited_at, 0);
    assert!(restarted(&opts, upgrade::Agent::Claude, Some(pid)));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid)).step,
        "told:stale-exit"
    );
    // Long past its exit: a pid the kernel may have handed on.
    save(
        &opts,
        SESSION,
        &claude(
            Phase::Failed("stale-exit".to_string()),
            now - 2 * STALE_S - 1,
        ),
    );
    assert!(!restarted(&opts, upgrade::Agent::Claude, Some(pid)));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(pid)).step,
        "refused:no-restart-in-flight"
    );
    std::fs::remove_file(state_path(&opts, SESSION)).expect("rm");
    // A Codex restart in flight, found by its TUI and carried by its lane: past its bound,
    // said. The bound counts from its `/exit`, not the TUI's exit: a TUI still alive the
    // Codex lane's `EXIT_TAKEN_S` after its `/exit` is one the `/exit` did not take.
    let codex = format!("codex-{TAB}");
    // The TUI its `/exit` was typed into: the Codex that left.
    let tui = dead_pid();
    save(
        &opts,
        &codex,
        &St {
            agent: upgrade::Agent::Codex,
            phase: Phase::Exiting {
                at_s: now - STALE_S - 1,
            },
            exited_at: now,
            pid: tui,
            shell: a.snap.shell,
            tab: TAB.to_string(),
            from: "0.157.0".to_string(),
            to: "0.157.1".to_string(),
            source: "managed".to_string(),
            mode: "daemon".to_string(),
            ..St::default()
        },
    );
    assert!(restarted(&opts, upgrade::Agent::Codex, Some(tui)));
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, Some(pid)),
        "not Claude Code's"
    );
    let r = carry_restart(&opts, upgrade::Agent::Codex, Some(tui));
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("told:stale-exit", codex.as_str())
    );
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, Some(tui)),
        "stopped: no longer in flight"
    );
    // Relaunched: the old TUI's exit may be read after the line was typed, so a relaunch
    // no TUI was adopted from is still the restart's. Negative control: once its
    // supervisor adopted the new TUI (`resumed_pid`), that TUI leaving is its own exit.
    let relaunched = |resumed_pid: u32, relaunched_pid: u32| St {
        agent: upgrade::Agent::Codex,
        phase: Phase::Relaunched { at_s: now },
        pid: tui,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "0.157.0".to_string(),
        to: "0.157.1".to_string(),
        source: "managed".to_string(),
        mode: "embedded".to_string(),
        resumed_pid,
        relaunched_pid,
        ..St::default()
    };
    save(&opts, &codex, &relaunched(0, 0));
    assert!(
        restarted(&opts, upgrade::Agent::Codex, Some(tui)),
        "relaunched, none found yet"
    );
    // Which TUI left, and what filed the record (Claude Code's rule, `ended_by_restart`):
    // no snapshot names none; a Codex a person started at the prompt is not the one the
    // restart ended; and a record the relaunch on exit filed (a crash, a stall, a fresh
    // relaunch, aterm's own end) is no restart of the harness's, even for its own TUI.
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, None),
        "no snapshot proves nothing"
    );
    let person = tui.wrapping_add(1);
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, Some(person)),
        "a person's Codex"
    );
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Codex, Some(person)).step,
        "refused:no-restart-in-flight"
    );
    {
        use super::super::relaunch::{CAUSE_FRESH, CAUSE_STALL};
        for cause in [CAUSE_EXIT, CAUSE_FRESH, CAUSE_STALL, CAUSE_HOST] {
            for phase in [
                Phase::Exiting { at_s: now },
                Phase::Relaunched { at_s: now },
            ] {
                let on_exit = St {
                    cause: cause.to_string(),
                    phase: phase.clone(),
                    ..relaunched(0, 0)
                };
                save(&opts, &codex, &on_exit);
                assert!(
                    !restarted(&opts, upgrade::Agent::Codex, Some(tui)),
                    "{cause} {phase:?}"
                );
                assert_eq!(
                    carry_restart(&opts, upgrade::Agent::Codex, Some(tui)).step,
                    "refused:no-restart-in-flight",
                    "{cause} {phase:?}"
                );
            }
        }
    }
    // Found, not adopted (its adoption ran out of time, or its carry-on found
    // a person at the keys): alive, it is not what left, and the old TUI's
    // exit read late is still the restart's; gone, it is what left, and its
    // exit is its own.
    save(&opts, &codex, &relaunched(0, std::process::id()));
    assert!(
        restarted(&opts, upgrade::Agent::Codex, Some(tui)),
        "the relaunched TUI lives: the old one's exit, read late"
    );
    // Gone, a carry of the old TUI's exit made after it (the host's carry tried again, or
    // another actor's adoption overtook) finds a restart that landed: closed as the Codex
    // lane's relaunch on exit closes it, never `refused:no-restart-in-flight`.
    let landed_gone = |st: &St| {
        save(&opts, &codex, st);
        let lines = ledger_lines(&opts);
        let r = carry_restart(&opts, upgrade::Agent::Codex, Some(tui));
        assert_eq!(
            (r.step.as_str(), r.session.as_str()),
            ("landed", codex.as_str())
        );
        assert_eq!(
            load(&opts, &codex).expect("kept").phase,
            Phase::Failed("exited-before-continuing".to_string())
        );
        assert_eq!(ledger_lines(&opts), lines + 1, "said once, in the ledger");
    };
    save(&opts, &codex, &relaunched(0, dead_pid()));
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, Some(tui)),
        "found and gone: the TUI that left was the relaunched one"
    );
    landed_gone(&relaunched(0, dead_pid()));
    save(&opts, &codex, &relaunched(4_000_002, 4_000_002));
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, Some(tui)),
        "adopted: the TUI that left was the relaunched one"
    );
    landed_gone(&relaunched(4_000_002, 4_000_002));
    // Adopted, it is its own even alive — a person's Ctrl-Z of it is theirs.
    save(
        &opts,
        &codex,
        &relaunched(std::process::id(), std::process::id()),
    );
    assert!(
        !restarted(&opts, upgrade::Agent::Codex, Some(tui)),
        "adopted and suspended: theirs"
    );
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Codex, Some(tui)).step,
        "refused:no-restart-in-flight"
    );
    // A new round forgets the old restart's TUI and line block ([`St::rearm`]): else the
    // old adoption would read the new restart's exit as the adopted TUI's own, and no
    // line of the new round was typed in that block.
    let mut again = St {
        phase: Phase::Failed("no-resume".to_string()),
        line_block: Some(7),
        ..relaunched(4_000_002, 4_000_002)
    };
    let _ = again.rearm(now);
    assert_eq!(
        (
            again.resumed_pid,
            again.relaunched_pid,
            again.exited_at,
            again.line_block
        ),
        (0, 0, 0, None)
    );
    again.phase = Phase::Relaunched { at_s: now };
    save(&opts, &codex, &again);
    assert!(
        restarted(&opts, upgrade::Agent::Codex, Some(tui)),
        "re-armed: {again:?}"
    );
}

/// The carry of an exit whose move another actor (a hand-run `aterm harness upgrade`)
/// finished meanwhile answers `landed` (the host's `Relaunched`), nothing said or
/// written, where a record Done for that process has its conversation held by a live
/// process. Negative controls: another process's exit, and nothing holding the conversation.
#[cfg(unix)]
#[test]
fn the_carry_of_an_exit_whose_move_landed_meanwhile_says_nothing() {
    use super::super::relaunch::{Outcome, carry_restart, outcome};
    let dir = scratch("carry-landed");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (old, me) = (dead_pid(), std::process::id());
    let now = now_s();
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Done,
            pid: old,
            shell: me,
            tab: TAB.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            exited_at: now - 3,
            done_at: now,
            ..St::default()
        },
    );
    let saved = load(&opts, SESSION).expect("the record");
    register(&opts.home, me, SESSION);
    let r = carry_restart(&opts, upgrade::Agent::Claude, Some(old));
    assert_eq!((r.step.as_str(), r.session.as_str()), ("landed", SESSION));
    assert_eq!(outcome(&r.step), Outcome::Relaunched, "nothing said");
    assert_eq!(load(&opts, SESSION), Some(saved), "nothing written");
    assert_eq!(ledger_lines(&opts), 0);
    // Negative controls: an exit this move did not end, and the move whose
    // conversation nothing holds now.
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(dead_pid())).step,
        "refused:no-restart-in-flight"
    );
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{me}.json")))
        .expect("remove the holder's session file");
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(old)).step,
        "refused:no-restart-in-flight"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The carry of a restart already adopted answers `adopted` at once, dialling and writing
/// nothing however old the relaunch, while the adopted process holds the conversation
/// (the record stays in flight until its loop carries it on). Negative controls: one not
/// adopted yet, or whose adopted process holds it no longer, is carried (here dialling a
/// socket that is not there).
#[cfg(unix)]
#[test]
fn the_carry_of_a_restart_already_adopted_answers_without_waiting() {
    use super::super::relaunch::carry_restart;
    let dir = scratch("carry-adopted");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        ..drive(&dir)
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (old, me) = (dead_pid(), std::process::id());
    let now = now_s();
    let relaunched = |at_s: u64, relaunched_pid: u32| St {
        phase: Phase::Relaunched { at_s },
        pid: old,
        shell: me,
        tab: TAB.to_string(),
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        source: "managed".to_string(),
        exited_at: now - 3,
        relaunched_pid,
        ..St::default()
    };
    register(&opts.home, me, SESSION);
    for at_s in [now, now - STALE_S - 1] {
        save(&opts, SESSION, &relaunched(at_s, me));
        let saved = load(&opts, SESSION).expect("the record");
        let r = carry_restart(&opts, upgrade::Agent::Claude, Some(old));
        assert_eq!((r.step.as_str(), r.pid), ("adopted", me), "{at_s}");
        assert_eq!(load(&opts, SESSION), Some(saved), "{at_s}: nothing written");
    }
    assert_eq!(ledger_lines(&opts), 0, "nothing said");
    save(&opts, SESSION, &relaunched(now, 0));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(old)).step,
        "wait:no-socket",
        "not adopted yet"
    );
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{me}.json")))
        .expect("remove the adopted process's session file");
    save(&opts, SESSION, &relaunched(now, me));
    assert_eq!(
        carry_restart(&opts, upgrade::Agent::Claude, Some(old)).step,
        "wait:no-socket",
        "holding it no longer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The carry of a restart whose adopted process ended before its continuation closes it
/// at once (`exited-before-continuing`, as `after_exit` closes one overtaken) and answers
/// `landed`, rather than holding the tab's hand for a gone process until `STALE_S`.
/// Negative control: a dry run writes nothing.
#[cfg(unix)]
#[test]
fn the_carry_of_a_restart_whose_adopted_agent_is_gone_closes_it() {
    use super::super::relaunch::{Outcome, carry_restart, outcome, restarted};
    let dir = scratch("carry-adopted-gone");
    let opts = Opts {
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        ..drive(&dir)
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let (old, gone) = (dead_pid(), dead_pid());
    let now = now_s();
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now },
            pid: old,
            shell: std::process::id(),
            tab: TAB.to_string(),
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            exited_at: now - 3,
            relaunched_pid: gone,
            ..St::default()
        },
    );
    let saved = load(&opts, SESSION).expect("the record");
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    assert_eq!(
        carry_restart(&dry, upgrade::Agent::Claude, Some(old)).step,
        "landed"
    );
    assert_eq!(
        load(&opts, SESSION),
        Some(saved),
        "a dry run writes nothing"
    );
    let r = carry_restart(&opts, upgrade::Agent::Claude, Some(old));
    assert_eq!((r.step.as_str(), r.session.as_str()), ("landed", SESSION));
    assert_eq!(outcome(&r.step), Outcome::Relaunched, "nothing said");
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(
        (st.phase, st.exited_at),
        (Phase::Failed("exited-before-continuing".to_string()), 0)
    );
    assert_eq!(ledger_lines(&opts), 1, "said once, in the ledger");
    assert!(
        !restarted(&opts, upgrade::Agent::Claude, Some(old)),
        "the restart's no longer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent aterm's own crash took (its shell's hangup made it remove its record, which
/// reads as a graceful exit) is relaunched in its reopened tab by `after_host_ended`; a
/// relaunch the crashed run had in flight for its old tab is closed, not waited on.
/// Negative controls: the same record through `after_exit` is graceful; the continuation
/// says why.
#[test]
fn an_agent_whose_host_ended_is_relaunched_in_its_reopened_tab() {
    let a = DeadAgent::new("host-ended");
    assert_eq!(a.attempt(&ExitRecord::Removed), "ended:graceful-exit");
    let r = after_host_ended(&a.opts, &a.snap, true);
    assert_eq!(r.step, "would-relaunch");
    assert_eq!(r.session, SESSION);
    // The crashed run's own relaunch, in flight for a tab that is gone.
    let old = St {
        tab: "s-0000000000000000dead".to_string(),
        pid: a.snap.pid,
        phase: super::super::upgrade::Phase::Exiting { at_s: now_s() },
        ..St::default()
    };
    save(
        &Opts {
            dry_run: false,
            ..a.opts.clone()
        },
        SESSION,
        &old,
    );
    assert_eq!(
        after_exit(
            &a.opts,
            &a.snap,
            &ExitRecord::Removed,
            true,
            Some(super::super::restart_stamp::Cause::Stall)
        )
        .step,
        "wait:conversation-in-other-tab",
        "a relaunch on exit waits for the tab its record names"
    );
    assert_eq!(
        after_host_ended(&a.opts, &a.snap, true).step,
        "would-relaunch"
    );
    // …and the relaunch's carry-on types that, never the upgrade's words.
    assert!(super::super::relaunch::relaunch_cause(CAUSE_HOST));
    assert!(
        resumed_prompt("2.1.283", CAUSE_HOST).contains("aterm ended while this session ran"),
        "{}",
        resumed_prompt("2.1.283", CAUSE_HOST)
    );
}

/// Tier-1 of `HarnessExitRecord` (aterm-spec): every maximal path of the model, replayed
/// through the real look ([`look_at_exit`], [`exit_record`]) and attempt ([`after_exit`],
/// dry) over a real record, leaves the model's state, every invariant holding. Negative
/// controls: the buggy witness read at the attempt breaks `CrashIsRelaunched`; a removal
/// inside the look's settle reads as `OwnRemoval` then `Look`, never the `LookAtInstant`
/// that breaks `GracefulIsLeft`.
#[test]
fn the_real_exit_read_conforms_to_the_model() {
    let model = aterm_spec::derive::harness_exit_record_model();
    // Every maximal path of the model at Buggy=0 (a small acyclic space).
    let mut paths: Vec<Vec<&'static str>> = Vec::new();
    let mut open = vec![(model.init_state(), Vec::new())];
    while let Some((state, path)) = open.pop() {
        let next: Vec<&'static str> = model
            .actions
            .iter()
            .map(|a| a.name)
            .filter(|a| model.action_enabled(a, &state))
            .collect();
        if next.is_empty() {
            paths.push(path);
            continue;
        }
        for action in next {
            let mut s = state.clone();
            assert!(model.fire(action, &mut s));
            let mut p = path.clone();
            p.push(action);
            open.push((s, p));
        }
    }
    assert!(paths.len() >= 3, "{paths:?}");
    let a = DeadAgent::new("exit-record-model");
    // The state the real code leaves: the exit (the one fact the code does
    // not hold), Claude's record on disk, what the look kept, what the
    // attempt decided.
    let project = |exit: i64, left: &Option<ExitRecord>, decided: i64| {
        [
            ("exit", exit),
            (
                "record",
                i64::from(look_at_exit(&a.opts.home, &a.snap).record.is_some()),
            ),
            ("seen", i64::from(left.is_some())),
            (
                "kept",
                i64::from(matches!(left, Some(ExitRecord::Survived(_)))),
            ),
            ("decided", decided),
        ]
        .into_iter()
        .collect::<aterm_spec::interp::State>()
    };
    // Replay one path through the real code against `m`: the state the real
    // code leaves after each action, checked against the model's.
    let replay = |m: &aterm_spec::derive::Model, path: &[&'static str]| {
        let _ = std::fs::remove_file(&a.record);
        a.write_record();
        let mut expect = m.init_state();
        let (mut exit, mut decided) = (0, 0);
        let mut left: Option<ExitRecord> = None;
        let mut seen = expect.clone();
        for &action in path {
            match action {
                "Crash" => exit = 1,
                "Graceful" => exit = 2,
                "OwnRemoval" | "Sweep" => a.remove_record(),
                "Look" => left = Some(a.look(|_| {})),
                "Decide" | "DecideAtAttempt" => {
                    let handed = if action == "Decide" {
                        left.clone().expect("looked at")
                    } else {
                        ExitRecord::Unread
                    };
                    decided = match a.attempt(&handed).as_str() {
                        "would-relaunch" => 1,
                        "ended:graceful-exit" => 2,
                        other => panic!("{path:?}: {action} said {other}"),
                    };
                }
                other => unreachable!("{other}"),
            }
            assert!(
                m.fire(action, &mut expect),
                "{action} disabled at {expect:?}"
            );
            seen = project(exit, &left, decided);
            assert_eq!(seen, expect, "{path:?}: {action}: observed vs model");
        }
        seen
    };
    for path in &paths {
        let end = replay(&model, path);
        for inv in &model.invariants {
            assert!(
                model.check_invariant(inv.name, &end),
                "{path:?}: {} broken by {end:?}",
                inv.name
            );
        }
    }
    // Negative control: the host that read the record at the attempt.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let witness = ["Crash", "Look", "Sweep", "DecideAtAttempt"];
    let end = replay(&buggy, &witness);
    assert_eq!(end["decided"], 2, "the crash left as a graceful exit");
    assert!(!buggy.check_invariant("CrashIsRelaunched", &end), "{end:?}");
    // A graceful exit's own removal landing inside the real look's settle
    // (`/exit` removes its own in its last moments): the real look reads it as
    // the model's `OwnRemoval` then `Look`, never as `LookAtInstant`, which
    // keeps the record the exit is still removing and relaunches the exit.
    let _ = std::fs::remove_file(&a.record);
    a.write_record();
    let left = Some(a.look(|looks| {
        if looks == 1 {
            a.remove_record();
        }
    }));
    let mut expect = model.init_state();
    let mut instant = buggy.init_state();
    for (action, naive) in [
        ("Graceful", "Graceful"),
        ("OwnRemoval", "LookAtInstant"),
        ("Look", "OwnRemoval"),
    ] {
        assert!(model.fire(action, &mut expect) && buggy.fire(naive, &mut instant));
    }
    let seen = project(2, &left, 0);
    assert_eq!(
        seen, expect,
        "the removal inside the settle: observed vs model"
    );
    assert_ne!(
        seen, instant,
        "the real look is not the look at the instant"
    );
    assert!(buggy.fire("Decide", &mut instant));
    assert!(
        !buggy.check_invariant("GracefulIsLeft", &instant),
        "{instant:?}"
    );
    // Every action of the model was driven, or contrasted.
    for act in &model.actions {
        assert!(
            paths.iter().any(|p| p.contains(&act.name))
                || witness.contains(&act.name)
                || act.name == "LookAtInstant",
            "{} is never driven",
            act.name
        );
    }
}

/// The restart in place restarts the tab's own agent on the same program, resuming its
/// conversation with nothing typed first (dry runs over real processes and files).
/// Negative controls: an unread agent, another tab's, no record, a one-shot run (each a
/// wait or refusal); a restart already in flight is carried on, never doubled.
#[cfg(unix)]
#[test]
fn a_restart_in_place_is_planned_on_the_same_program_and_carried_on_once() {
    let dir = scratch("restart-here");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let mut shell = stand_in_shell();
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    let snap = |argv: &[&str]| Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: agent.id(),
        start: start.clone(),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: argv.iter().map(|a| (*a).to_string()).collect(),
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("1.0.0".to_string()),
        codex: None,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    };
    let claude = ["/opt/claude/bin/claude", "--verbose"];
    // Not read: a wait, nothing else.
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Err("no-foreground")).step,
        "wait:no-foreground"
    );
    // No record of this process names its conversation yet.
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(snap(&claude))).step,
        "wait:no-session-record"
    );
    register(&opts.home, agent.id(), SESSION);
    let r = restart_from(&opts, &Restart::Memory, Ok(snap(&claude)));
    assert_eq!(
        (r.step.as_str(), r.session.as_str(), r.to.as_str()),
        ("would-restart", SESSION, "1.0.0(same)"),
        "{r:?}"
    );
    // Another tab's agent is never this tab's to restart.
    let other = Snapshot {
        tab: "s-0ther".to_string(),
        ..snap(&claude)
    };
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(other)).step,
        "wait:tab-identity-conflict"
    );
    // A one-shot run is no session to bring back.
    assert_eq!(
        restart_from(
            &opts,
            &Restart::Memory,
            Ok(snap(&["/opt/claude/bin/claude", "-p", "hi"]))
        )
        .step,
        "refused:one-shot"
    );
    // A bucket's fallback asks for its model on the line; the way back asks for the model
    // the launch named before the fallback (its record kept it), else the notice's alias,
    // else none: the launch's `--model` dropped.
    let opus = Restart::Model {
        to: "opus".to_string(),
    };
    assert_eq!(
        restart_from(&opts, &opus, Ok(snap(&claude))).step,
        "would-restart:model=opus"
    );
    let back = |to: Option<&str>| Restart::ModelBack {
        to: to.map(str::to_string),
    };
    assert_eq!(
        restart_from(&opts, &back(Some("fable")), Ok(snap(&claude))).step,
        "would-restart:model=fable"
    );
    assert_eq!(
        restart_from(&opts, &back(None), Ok(snap(&claude))).step,
        "would-restart:model="
    );
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let fallen_back = St {
        phase: Phase::Done,
        cause: "model:opus".to_string(),
        launch_model: "claude-fable-5-1".to_string(),
        ..St::default()
    };
    std::fs::write(state_path(&opts, SESSION), fallen_back.to_json()).expect("record");
    assert_eq!(
        restart_from(&opts, &back(None), Ok(snap(&claude))).step,
        "would-restart:model=claude-fable-5-1"
    );
    // The notice's alias does not override the recorded origin.
    assert_eq!(
        restart_from(&opts, &back(Some("fable")), Ok(snap(&claude))).step,
        "would-restart:model=claude-fable-5-1"
    );
    // A restart in flight for the conversation is carried on, not doubled.
    let st = St {
        phase: Phase::Exiting { at_s: now_s() },
        pid: agent.id(),
        shell: shell.id(),
        tab: TAB.to_string(),
        cause: "memory".to_string(),
        ..St::default()
    };
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("in flight");
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(snap(&claude))).step,
        "would-resume:exiting"
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A main-chain answer by `model` stamped `at` (RFC 3339): what the live
/// model is read from past a process's start ([`models::live_model_at`]).
#[cfg(unix)]
fn answer_at(model: &str, at: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"assistant","timestamp":"{at}","message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"Done."}}]}},"version":"2.1.283"}}"#
    )
}

/// The relaunch snapshot of an agent under the user's own Claude home
/// holding conversation `session`: what the window host hands the readers of
/// its limit and its wall ([`named_standing_wall`]).
#[cfg(unix)]
fn users_agent(session: &str) -> Snapshot {
    Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(session.to_string()),
        cwd: "/".to_string(),
        version: None,
        codex: None,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    }
}

/// The conversation `SESSION`'s transcript under `home`: `rows`, one a line.
#[cfg(unix)]
fn write_transcript(home: &Path, rows: &[String]) {
    let project = home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        rows.join("\n") + "\n",
    )
    .expect("transcript");
}

/// The model a person chose for `SESSION` with `/model`, as the harness's
/// record remembers it past the answers that follow ([`ModelRecord::human`]).
#[cfg(unix)]
fn chose_by_hand(opts: &Opts, model: &str) {
    let dir = state_dir(opts).join("models");
    std::fs::create_dir_all(&dir).expect("models");
    let rec = ModelRecord {
        human: model.to_string(),
        ..ModelRecord::default()
    };
    std::fs::write(dir.join(format!("{SESSION}.json")), rec.render()).expect("record");
}

/// A `/model` choice survives a memory restart (a command-line `--model` overrides the
/// default `/model` saves): the restart asks for the hand choice, not the launch's flag.
/// Negative controls: nobody chose one; a launch that named no model keeps its flags.
#[cfg(unix)]
#[test]
fn a_memory_restart_carries_a_model_chosen_by_hand_not_the_launchs() {
    let dir = scratch("memory-hand-model");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let mut shell = stand_in_shell();
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    let snap = |argv: &[&str]| Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: agent.id(),
        start: start.clone(),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: argv.iter().map(|a| (*a).to_string()).collect(),
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("1.0.0".to_string()),
        codex: None,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    register(&opts.home, agent.id(), SESSION);
    let sonnet = ["/opt/claude/bin/claude", "--model", "claude-sonnet-5-5"];
    // Answered past this process's start (its clock is now).
    let later = "2099-01-01T00:00:00Z";
    // Negative control: the launch's model runs, nobody chose another.
    write_transcript(&opts.home, &[answer_at("claude-sonnet-5-5", later)]);
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(snap(&sonnet))).step,
        "would-restart"
    );
    // A person moved it to Opus with `/model`, and it has answered on Opus.
    write_transcript(&opts.home, &[answer_at("claude-opus-5-5", later)]);
    chose_by_hand(&opts, "claude-opus-5-5");
    assert_eq!(
        restart_from(&opts, &Restart::Memory, Ok(snap(&sonnet))).step,
        "would-restart:model=claude-opus-5-5"
    );
    // Negative control: a launch that named no model keeps its flags.
    assert_eq!(
        restart_from(
            &opts,
            &Restart::Memory,
            Ok(snap(&["/opt/claude/bin/claude", "--verbose"]))
        )
        .step,
        "would-restart"
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `/model` choice survives an exit's relaunch: a crashed agent launched
/// with `--model claude-sonnet-5` and moved to Opus by hand is relaunched
/// asking for Opus, not the launch's Sonnet. Negative control: nobody chose
/// one: relaunched on the launch's own flags.
#[cfg(unix)]
#[test]
fn an_exit_relaunch_carries_a_model_chosen_by_hand_not_the_launchs() {
    let mut a = DeadAgent::new("exit-hand-model");
    a.snap.argv = ["/opt/claude/bin/claude", "--model", "claude-sonnet-5"]
        .iter()
        .map(|w| (*w).to_string())
        .collect();
    // Answered past the agent's start (Sep 24 2026).
    let later = "2026-09-25T00:00:00Z";
    write_transcript(&a.opts.home, &[answer_at("claude-opus-5-5", later)]);
    a.write_record();
    let left = a.look(|_| {});
    assert!(matches!(&left, ExitRecord::Survived(_)), "{left:?}");
    // Negative control: no `/model` remembered — the answer on another model
    // is no one's choice this module can name, and the launch's flags stand.
    assert_eq!(a.attempt(&left), "would-relaunch");
    chose_by_hand(&a.opts, "claude-opus-5-5");
    assert_eq!(a.attempt(&left), "would-relaunch:model=claude-opus-5-5");
}

/// A stalled agent ended with no record left takes its process start from the host's
/// snapshot, so a `/model` made for an earlier process is not carried. Negative control:
/// a choice answered on since the process started is carried.
#[cfg(unix)]
#[test]
fn an_exit_relaunch_with_no_record_carries_no_choice_older_than_the_process() {
    let mut a = DeadAgent::new("exit-hand-older");
    a.snap.argv = ["/opt/claude/bin/claude", "--model", "claude-sonnet-5"]
        .iter()
        .map(|w| (*w).to_string())
        .collect();
    chose_by_hand(&a.opts, "claude-opus-5-5");
    let stalled = |a: &DeadAgent| {
        after_exit(
            &a.opts,
            &a.snap,
            &ExitRecord::Removed,
            true,
            Some(super::super::restart_stamp::Cause::Stall),
        )
    };
    // Answered on Opus the day before this agent started (Sep 24 2026).
    write_transcript(
        &a.opts.home,
        &[answer_at("claude-opus-5-5", "2026-09-23T00:00:00Z")],
    );
    assert_eq!(stalled(&a).step, "would-relaunch");
    // Negative control: answered on Opus since it started.
    write_transcript(
        &a.opts.home,
        &[answer_at("claude-opus-5-5", "2026-09-25T00:00:00Z")],
    );
    assert_eq!(stalled(&a).step, "would-relaunch:model=claude-opus-5-5");
}

/// The way back outlives a memory restart: the fallback's origin rides every record while
/// the fallback stands ([`St::fallback_from`]) and is spent by the way back. Negative
/// controls: no fallback standing drops `--model`; a second fallback keeps the first's
/// origin; an older build's fallback record returns to its kept launch model.
#[test]
fn the_way_back_from_a_fallback_returns_to_the_launched_model_across_a_memory_restart() {
    let launched = "claude-fable-5-1";
    let fallback = "claude-opus-5";
    let fell = model_restart(
        &Restart::Model {
            to: fallback.to_string(),
        },
        None,
        launched.to_string(),
        None,
        None,
    );
    assert_eq!(fell.model.as_deref(), Some(fallback));
    // The fallback's process runs `--model <fallback>`: a memory restart of
    // it keeps the launch's (the fallback's) flags and the origin.
    let memory = model_restart(
        &Restart::Memory,
        Some(&saved_restart(&fell)),
        fallback.to_string(),
        None,
        None,
    );
    assert_eq!(memory.cause, "memory");
    assert_eq!(memory.model, None);
    assert_eq!(memory.fallback_from.as_deref(), Some(launched));
    let back = model_restart(
        &Restart::ModelBack { to: None },
        Some(&saved_restart(&memory)),
        fallback.to_string(),
        None,
        None,
    );
    assert_eq!(back.model.as_deref(), Some(launched));
    assert_eq!(back.cause, format!("model-back:{launched}"));
    // The line: the fallback's `--model` replaced by the launched one.
    let argv = |w: &[&str]| w.iter().map(|a| (*a).to_string()).collect::<Vec<_>>();
    assert_eq!(
        with_model(
            argv(&["--model", fallback, "--resume", SESSION]),
            back.model.as_deref()
        ),
        argv(&["--model", launched, "--resume", SESSION])
    );
    // Spent: no fallback stands after the way back.
    assert_eq!(fallback_origin(&saved_restart(&back)), None);
    let again = model_restart(
        &Restart::ModelBack { to: None },
        Some(&saved_restart(&back)),
        launched.to_string(),
        None,
        None,
    );
    assert_eq!(again.model.as_deref(), Some(""), "no fallback: none");
    // A second fallback keeps the first's origin.
    let second = model_restart(
        &Restart::Model {
            to: "claude-sonnet-5".to_string(),
        },
        Some(&saved_restart(&memory)),
        fallback.to_string(),
        None,
        None,
    );
    assert_eq!(second.fallback_from.as_deref(), Some(launched));
    // An older build's fallback record: its kept launch model.
    let legacy = St {
        phase: Phase::Done,
        cause: format!("model:{fallback}"),
        launch_model: launched.to_string(),
        ..St::default()
    };
    assert_eq!(fallback_origin(&legacy).as_deref(), Some(launched));
}

/// The way back prefers what the person had over the notice's alias: a hand choice since
/// the fallback, else the exact origin (a `[1m]` window included), else the alias.
/// Negative control: nothing on record and no `/model`, the alias.
#[test]
fn the_way_back_prefers_the_persons_model_over_the_notices_alias() {
    let launched = "claude-fable-5-1[1m]";
    let fell = model_restart(
        &Restart::Model {
            to: "claude-opus-5-5".to_string(),
        },
        None,
        launched.to_string(),
        None,
        None,
    );
    let named = || Restart::ModelBack {
        to: Some("fable".to_string()),
    };
    let back = model_restart(
        &named(),
        Some(&saved_restart(&fell)),
        "claude-opus-5-5".to_string(),
        None,
        None,
    );
    assert_eq!(back.model.as_deref(), Some(launched), "the exact origin");
    let chosen = model_restart(
        &named(),
        Some(&saved_restart(&fell)),
        "claude-opus-5-5".to_string(),
        None,
        Some("claude-sonnet-5-5".to_string()),
    );
    assert_eq!(
        chosen.model.as_deref(),
        Some("claude-sonnet-5-5"),
        "a /model since the fallback is kept"
    );
    // A launch that named no model: the way back drops `--model`.
    let bare = model_restart(
        &Restart::Model {
            to: "claude-opus-5-5".to_string(),
        },
        None,
        String::new(),
        None,
        None,
    );
    let to_default = model_restart(
        &named(),
        Some(&saved_restart(&bare)),
        "claude-opus-5-5".to_string(),
        None,
        None,
    );
    assert_eq!(to_default.model.as_deref(), Some(""));
    // The control: nothing on record, nobody's `/model`.
    let alias = model_restart(&named(), None, "claude-opus-5-5".to_string(), None, None);
    assert_eq!(alias.model.as_deref(), Some("fable"));
}

/// The record a restart saves, as the state file keeps it.
fn saved_restart(r: &super::super::relaunch::ModelRestart) -> St {
    let st = St {
        phase: Phase::Done,
        cause: r.cause.clone(),
        launch_model: r.launch_model.clone(),
        fallback_from: r.fallback_from.clone(),
        ..St::default()
    };
    St::from_json(&st.to_json()).expect("the record reads back")
}

/// A fallback from a launch that named no model still stands (recorded apart from the
/// model it replaced), so a second fallback's way back returns to Claude's default, not the
/// first fallback's model. Negative control: one fallback from no model drops `--model`.
#[test]
fn a_second_fallback_from_a_launch_with_no_model_goes_back_to_no_model() {
    let first = model_restart(
        &Restart::Model {
            to: "claude-opus-5".to_string(),
        },
        None,
        String::new(),
        None,
        None,
    );
    assert_eq!(
        fallback_origin(&saved_restart(&first)).as_deref(),
        Some(""),
        "a fallback stands, from no model"
    );
    let back_once = model_restart(
        &Restart::ModelBack { to: None },
        Some(&saved_restart(&first)),
        "claude-opus-5".to_string(),
        None,
        None,
    );
    assert_eq!(back_once.model.as_deref(), Some(""));
    let second = model_restart(
        &Restart::Model {
            to: "claude-sonnet-5".to_string(),
        },
        Some(&saved_restart(&first)),
        "claude-opus-5".to_string(),
        None,
        None,
    );
    let back = model_restart(
        &Restart::ModelBack { to: None },
        Some(&saved_restart(&second)),
        "claude-sonnet-5".to_string(),
        None,
        None,
    );
    assert_eq!(
        back.model.as_deref(),
        Some(""),
        "drop --model: Claude's default"
    );
    assert_eq!(
        with_model(
            vec!["--model".into(), "claude-sonnet-5".into()],
            back.model.as_deref()
        ),
        Vec::<String>::new()
    );
}

/// A `/model` since a fallback outranks the way back: an exit relaunched on the hand
/// choice makes it the origin, so the bucket's reset returns to it. Negative controls: no
/// `/model` returns to the origin; a memory restart reads the same; with no fallback
/// standing, a hand choice makes none.
#[test]
fn a_model_chosen_by_hand_after_a_fallback_is_not_undone_by_the_way_back() {
    let dir = scratch("hand-over-fallback");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let origin = "claude-fable-5-1";
    let fallback = "claude-opus-5";
    let hand = "claude-sonnet-5";
    let fell = model_restart(
        &Restart::Model {
            to: fallback.to_string(),
        },
        None,
        origin.to_string(),
        None,
        None,
    );
    save(&opts, SESSION, &saved_restart(&fell));
    let exit_record = |fallback_from: Option<String>, launch: &str| St {
        phase: Phase::Done,
        cause: CAUSE_EXIT.to_string(),
        launch_model: launch.to_string(),
        fallback_from,
        ..St::default()
    };
    let way_back = |st: &St, launch: &str| {
        model_restart(
            &Restart::ModelBack { to: None },
            Some(st),
            launch.to_string(),
            None,
            None,
        )
        .model
    };
    // The exit relaunched on the person's Sonnet.
    let carried = exit_record(exit_fallback(&opts, SESSION, None, Some(hand)), hand);
    assert_eq!(
        way_back(&carried, hand).as_deref(),
        Some(hand),
        "the hand choice stands"
    );
    // Negative control: nobody's `/model` — the origin rides the exit.
    let plain = exit_record(exit_fallback(&opts, SESSION, None, None), fallback);
    assert_eq!(way_back(&plain, fallback).as_deref(), Some(origin));
    // The memory restart, the same.
    let memory = model_restart(
        &Restart::Memory,
        Some(&saved_restart(&fell)),
        fallback.to_string(),
        None,
        Some(hand.to_string()),
    );
    assert_eq!(memory.model.as_deref(), Some(hand));
    assert_eq!(
        way_back(&saved_restart(&memory), hand).as_deref(),
        Some(hand)
    );
    // No fallback standing: a hand choice is no fallback's origin.
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Done,
            ..St::default()
        },
    );
    assert_eq!(exit_fallback(&opts, SESSION, None, Some(hand)), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The way back outlives an upgrade: the fallback's origin is carried onto
/// every state an upgrade mints over its record ([`St::for_target`]), so the
/// way back at the bucket's reset still has somewhere to return to. Negative
/// control: a record with no fallback standing mints none.
#[test]
fn an_upgrade_record_keeps_the_fallbacks_origin() {
    let from = Version::parse("2.1.280").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.283").expect("v"),
        source: Source::Managed,
    };
    let fell = saved_restart(&model_restart(
        &Restart::Model {
            to: "claude-opus-5".to_string(),
        },
        None,
        "claude-fable-5-1".to_string(),
        None,
        None,
    ));
    let pending = St {
        phase: Phase::Pending,
        to: "2.1.282".to_string(),
        ..fell.clone()
    };
    for (name, prior) in [("done", fell.clone()), ("retargeted", pending)] {
        let next = St::for_target(Some(prior), &from, &to, None, Proc::default(), 900);
        assert_eq!(
            fallback_origin(&next).as_deref(),
            Some("claude-fable-5-1"),
            "{name}"
        );
    }
    // Re-armed after a stop, an older build's fallback record keeps it too.
    let mut legacy = St {
        phase: Phase::Failed("unanswered".to_string()),
        cause: "model:claude-opus-5".to_string(),
        launch_model: "claude-fable-5-1".to_string(),
        ..St::default()
    };
    legacy.rearm(900);
    assert_eq!(
        fallback_origin(&legacy).as_deref(),
        Some("claude-fable-5-1")
    );
    let none = St::for_target(
        Some(St {
            phase: Phase::Done,
            ..St::default()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(fallback_origin(&none), None);
}

/// A `/model` before a bucket's fallback becomes the fallback's origin, not the launch's
/// model. Negative controls: no `/model`, the launch's; a fallback already standing keeps
/// its origin over a hand choice.
#[test]
fn a_fallback_after_a_model_chosen_by_hand_goes_back_to_that_model() {
    let launched = "claude-fable-5-1";
    let hand = "claude-sonnet-5";
    let fallback = Restart::Model {
        to: "claude-opus-5".to_string(),
    };
    let way_back = |fell: &super::super::relaunch::ModelRestart| {
        model_restart(
            &Restart::ModelBack { to: None },
            Some(&saved_restart(fell)),
            "claude-opus-5".to_string(),
            None,
            None,
        )
        .model
    };
    let fell = model_restart(
        &fallback,
        None,
        launched.to_string(),
        None,
        Some(hand.to_string()),
    );
    assert_eq!(fell.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(fell.fallback_from.as_deref(), Some(hand));
    assert_eq!(way_back(&fell).as_deref(), Some(hand));
    // Negative control: nobody's `/model`.
    let plain = model_restart(&fallback, None, launched.to_string(), None, None);
    assert_eq!(way_back(&plain).as_deref(), Some(launched));
    // Negative control: a second fallback keeps the first's origin.
    let second = model_restart(
        &fallback,
        Some(&saved_restart(&plain)),
        "claude-haiku-5".to_string(),
        None,
        Some(hand.to_string()),
    );
    assert_eq!(second.fallback_from.as_deref(), Some(launched));
}

/// A restart in place of a stand-in agent launched `--model <launch>`,
/// driven for real ([`restart_with`], its job and terminal scripted): the
/// conversation's last record `prior`, a `/model` of `hand` answered since
/// the agent started — and what it says, and the record it saved.
#[cfg(unix)]
fn restart_saved(
    name: &str,
    why: &Restart,
    launch: &str,
    prior: Option<&St>,
    hand: Option<&str>,
) -> (Report, Option<St>) {
    restart_saved_as(
        name,
        why,
        launch,
        prior,
        hand,
        None,
        &crate::harness::accounts::Roster::default(),
    )
}

/// [`restart_saved`] of an agent whose own exec environment named `login`
/// ([`Snapshot::login`]), by a host that keeps `roster`
/// ([`crate::harness::relaunch::restart_as`]).
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn restart_saved_as(
    name: &str,
    why: &Restart,
    launch: &str,
    prior: Option<&St>,
    hand: Option<&str>,
    login: Option<&str>,
    roster: &crate::harness::accounts::Roster,
) -> (Report, Option<St>) {
    let dir = scratch(name);
    let (sock, _asked) = instance_with(&dir, Answers::default());
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    let mut shell = stand_in_shell();
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    register(&opts.home, agent.id(), SESSION);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    if let Some(prior) = prior {
        save(&opts, SESSION, prior);
    }
    // Answered past this process's start (its clock is now).
    let answered = hand.unwrap_or(launch);
    write_transcript(&opts.home, &[answer_at(answered, "2099-01-01T00:00:00Z")]);
    if let Some(hand) = hand {
        chose_by_hand(&opts, hand);
    }
    let snap = Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: agent.id(),
        start: kernel_start(agent.id()).expect("lstart"),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: ["/opt/claude/bin/claude", "--model", launch]
            .iter()
            .map(|w| (*w).to_string())
            .collect(),
        session: Some(SESSION.to_string()),
        cwd: "/".to_string(),
        version: Some("1.0.0".to_string()),
        codex: None,
        dialect: None,
        login: login.map(str::to_string),
        home: None,
        claude_dir: None,
    };
    let r = crate::harness::relaunch::restart_as(
        &opts,
        why,
        roster,
        Ok(snap),
        &Script::new(shell.id(), usize::MAX, None),
    );
    let saved = load(&opts, SESSION);
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
    (r, saved)
}

/// The restart in place saves the fallback origin it decides ([`restart_from`] into
/// [`model_restart`]), driven past its signal: over a fallback, a `/model` made since
/// becomes the origin, as does a `/model` before a fallback. Negative control: a memory
/// restart with no `/model` carries the fallback's own origin.
#[cfg(unix)]
#[test]
fn a_restart_in_place_saves_the_fallback_origin_it_decides() {
    let origin = "claude-fable-5-1";
    let fallback = "claude-sonnet-5-5";
    let hand = "claude-opus-5-5";
    let fell = saved_restart(&model_restart(
        &Restart::Model {
            to: fallback.to_string(),
        },
        None,
        origin.to_string(),
        None,
        None,
    ));
    // (case, restart, the launch's model, the prior record, the `/model`,
    // the model the line asks for, the origin saved)
    for (name, why, launch, prior, chose, asks, want) in [
        (
            "saved-memory-hand",
            Restart::Memory,
            fallback,
            Some(&fell),
            Some(hand),
            hand,
            hand,
        ),
        (
            "saved-memory-nobody",
            Restart::Memory,
            fallback,
            Some(&fell),
            None,
            fallback,
            origin,
        ),
        (
            "saved-fallback-hand",
            Restart::Model {
                to: fallback.to_string(),
            },
            origin,
            None,
            Some(hand),
            fallback,
            hand,
        ),
    ] {
        let (r, saved) = restart_saved(name, &why, launch, prior, chose);
        let saved = saved.unwrap_or_else(|| panic!("{name}: no record: {r:?}"));
        assert!(
            !matches!(saved.phase, Phase::Pending | Phase::Done),
            "{name}: signalled: {r:?} {saved:?}"
        );
        assert_eq!(saved.fallback_from.as_deref(), Some(want), "{name}: {r:?}");
        assert!(
            saved.line.contains(&format!("'--model' '{asks}'")),
            "{name}: {}",
            saved.line
        );
    }
}

/// A login directory with a space and an apostrophe in it.
#[cfg(unix)]
const LOGIN_DIR: &str = "/tmp/aterm state/logins/owner's work-2";

/// [`LOGIN_DIR`] as a POSIX shell's relaunch line writes it.
#[cfg(unix)]
const LOGIN_WORD: &str =
    r"CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/aterm state/logins/owner'\''s work-2' ";

/// A two-row roster: `main` on this machine's login, `work-2` under [`LOGIN_DIR`].
#[cfg(unix)]
fn two_accounts() -> crate::harness::accounts::Roster {
    crate::harness::accounts::Roster::parse(&format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{LOGIN_DIR}\"\n"
    ))
    .expect("a roster")
}

/// Every restart in place re-applies the login its process had, quoted before the survey
/// switch (an empty value as the empty value), and keeps it on the record ([`St::login`]).
/// Negative control: an agent with no such variable gets the line it always got and no
/// `login` key.
#[cfg(unix)]
#[test]
fn a_restart_in_place_types_back_the_login_its_process_had() {
    let roster = crate::harness::accounts::Roster::default();
    let launch = "claude-fable-5-1";
    // The stand-in's line resumes in the conversation's directory (`/`),
    // which is not the test's own: the subshell form, the environment inside.
    let line = |env: &str| {
        format!(
            " ( cd -- '/' && {env}CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 exec \
             '/opt/claude/bin/claude' '--model' '{launch}' '--resume' '{SESSION}' )"
        )
    };
    for (name, login, want) in [
        ("login-memory-dir", Some(LOGIN_DIR), line(LOGIN_WORD)),
        (
            "login-memory-empty",
            Some(""),
            line("CLAUDE_SECURESTORAGE_CONFIG_DIR='' "),
        ),
        ("login-memory-none", None, line("")),
    ] {
        let (r, saved) =
            restart_saved_as(name, &Restart::Memory, launch, None, None, login, &roster);
        let saved = saved.unwrap_or_else(|| panic!("{name}: no record: {r:?}"));
        assert_eq!(saved.line, want, "{name}");
        assert_eq!(saved.login.as_deref(), login, "{name}");
        assert_eq!(saved.cause, "memory", "{name}");
        assert_eq!(
            saved.to_json().contains("\"login\""),
            login.is_some(),
            "{name}: the key is written only when set"
        );
        assert_eq!(
            St::from_json(&saved.to_json()).map(|st| st.login),
            Some(login.map(str::to_string)),
            "{name}: read back"
        );
    }
    // A record an older build wrote has no such key, and reads as none.
    let old = St::from_json(r#"{"phase":"done","line":" '/opt/claude/bin/claude'"}"#);
    assert_eq!(old.map(|st| st.login), Some(None));
}

/// An account move types the target's login from the roster (`work-2`'s directory, or the
/// empty value for `main`, the row with none) and records `account:<label>`, never the
/// directory; over a standing fallback the line asks for the origin and spends it.
/// Negative controls: a label the roster does not list, or no roster, is refused before
/// anything is signalled or saved.
#[cfg(unix)]
#[test]
fn an_account_move_types_the_rosters_login_and_a_label_nobody_lists_is_refused() {
    use crate::supervise::policy::turn_end::AccountAt;
    let roster = two_accounts();
    let launch = "claude-fable-5-1";
    let to = |label: &str| Restart::Account {
        to: label.to_string(),
        at: AccountAt::Wall,
    };
    let (r, saved) = restart_saved_as(
        "account-to-work",
        &to("work-2"),
        launch,
        None,
        None,
        None,
        &roster,
    );
    let saved = saved.unwrap_or_else(|| panic!("no record: {r:?}"));
    assert!(
        saved.line.contains(&format!(
            "&& {LOGIN_WORD}CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 exec "
        )),
        "{}",
        saved.line
    );
    assert_eq!(saved.cause, "account:work-2");
    assert_eq!(saved.login.as_deref(), Some(LOGIN_DIR));
    assert!(!saved.cause.contains("/tmp"), "the label only");
    // Back to the default login, over the directory the process had.
    let (r, saved) = restart_saved_as(
        "account-to-main",
        &to("main"),
        launch,
        None,
        None,
        Some(LOGIN_DIR),
        &roster,
    );
    let saved = saved.unwrap_or_else(|| panic!("no record: {r:?}"));
    assert!(
        saved.line.contains(
            "&& CLAUDE_SECURESTORAGE_CONFIG_DIR='' CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 exec "
        ),
        "{}",
        saved.line
    );
    assert!(!saved.line.contains(LOGIN_DIR), "{}", saved.line);
    assert_eq!(
        (saved.cause.as_str(), saved.login.as_deref()),
        ("account:main", Some(""))
    );
    // A fallback stands: the move comes back on the origin, and spends it.
    let origin = "claude-fable-5-1";
    let fallback = "claude-sonnet-5-5";
    let fell = saved_restart(&model_restart(
        &Restart::Model {
            to: fallback.to_string(),
        },
        None,
        origin.to_string(),
        None,
        None,
    ));
    let (r, saved) = restart_saved_as(
        "account-way-back",
        &to("work-2"),
        fallback,
        Some(&fell),
        None,
        None,
        &roster,
    );
    let saved = saved.unwrap_or_else(|| panic!("no record: {r:?}"));
    assert!(
        saved.line.contains(&format!("'--model' '{origin}'")),
        "{}",
        saved.line
    );
    assert_eq!(saved.fallback_from, None, "{r:?}");
    // Negative controls: nothing signalled, nothing saved.
    for (name, roster) in [
        ("account-not-listed", &roster),
        (
            "account-no-roster",
            &crate::harness::accounts::Roster::default(),
        ),
    ] {
        let (r, saved) = restart_saved_as(name, &to("pool-a"), launch, None, None, None, roster);
        assert_eq!(r.step, "refused:account-not-listed", "{name}");
        assert_eq!(saved, None, "{name}");
    }
}

/// An exit's relaunch types back the login the agent ran under (read off the process while
/// it ran), so a session moved to another account stays on it through a crash; the
/// relaunch after aterm ended ([`after_host_ended`]) types the reopened leaf's Claude home
/// and login, the home first. Negative control: no such variable, the line it always got.
#[cfg(unix)]
#[test]
fn an_exit_relaunch_types_back_the_login_the_agent_ran_under() {
    let case = |name: &str, home: bool, login: Option<&str>, host_ended: bool| {
        let _quick = QuickWaits::new();
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        let lab_home = a.dir.join("lab home/.claude");
        a.snap.login = login.map(str::to_string);
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        if home {
            a.snap.home = Some(lab_home.to_string_lossy().into_owned());
            a.write_record_in(&lab_home, SESSION);
            file_transcript_in(&lab_home, "p");
        } else {
            a.write_record();
        }
        let (r, cause) = if host_ended {
            (after_host_ended(&a.opts, &a.snap, true), CAUSE_HOST)
        } else {
            let left = a.look(|_| {});
            (after_exit(&a.opts, &a.snap, &left, true, None), CAUSE_EXIT)
        };
        let saved = load(&a.opts, SESSION).expect("state");
        assert_eq!(saved.cause, cause, "{name}: {r:?}");
        assert_eq!(saved.login.as_deref(), login, "{name}");
        assert_eq!(
            saved.line.contains(LOGIN_WORD),
            login.is_some(),
            "{name}: {}",
            saved.line
        );
        assert_eq!(
            saved.line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            login.is_some(),
            "{name}: {}",
            saved.line
        );
        assert_eq!(
            saved
                .line
                .find(&format!("CLAUDE_CONFIG_DIR='{}' ", lab_home.display())),
            home.then(|| saved.line.find("CLAUDE_")).flatten(),
            "{name}: the home, first, or nothing of one: {}",
            saved.line
        );
    };
    std::thread::scope(|scope| {
        let moved = scope.spawn(|| case("exit-login-dir", false, Some(LOGIN_DIR), false));
        let cold = scope.spawn(|| case("host-home-login", true, Some(LOGIN_DIR), true));
        case("exit-login-none", false, None, false);
        moved.join().expect("exit-login-dir");
        cold.join().expect("host-home-login");
    });
}

/// A transcript of [`SESSION`] with one prompt, filed under `claude_dir`'s project
/// `project` as Claude Code files one with its first message. A session with no transcript
/// is one nobody asked anything ([`transcript_exists_in`]) and is restarted afresh, so tests
/// that move or relaunch sessions with work file this.
#[cfg(unix)]
fn file_transcript_in(claude_dir: &Path, project: &str) {
    let dir = claude_dir.join("projects").join(project);
    std::fs::create_dir_all(&dir).expect("project");
    std::fs::write(
        dir.join(format!("{SESSION}.jsonl")),
        r#"{"type":"user","message":{"role":"user","content":"port the parser"}}"#.to_string()
            + "\n",
    )
    .expect("transcript");
}

/// [`Tasked::of_snapshot`]: the turn end's task read is the agent's own records, under its
/// Claude home (a relative one where it resolves from the directory it runs in), not the
/// user's `~/.claude`. Controls: with no home of its own, `~/.claude` is read; a relative
/// home whose directory nobody knows says nothing.
#[cfg(unix)]
#[test]
fn the_turn_ends_task_read_is_the_agents_own_records() {
    let dir = scratch("tasked-own-home");
    let user_home = dir.join("home");
    std::fs::create_dir_all(user_home.join(".claude")).expect("the user's home, no projects");
    let work = dir.join("work/.claude");
    file_transcript_in(&work, "p");
    let own = Snapshot {
        home: Some(work.to_string_lossy().into_owned()),
        claude_dir: Some(work.clone()),
        ..users_agent(SESSION)
    };
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &own, &[]),
        Some(true),
        "its own transcript: a person's prompt"
    );
    assert_eq!(
        Tasked::default().of(&user_home, SESSION, &[]),
        Some(false),
        "the user's ~/.claude says nothing of it"
    );
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &users_agent(SESSION), &[]),
        Some(false)
    );
    let relative = Snapshot {
        home: Some("work/.claude".to_string()),
        cwd: dir.to_string_lossy().into_owned(),
        ..users_agent(SESSION)
    };
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &relative, &[]),
        Some(true),
        "a relative home, where it resolves"
    );
    let unknown = Snapshot {
        cwd: String::new(),
        ..relative
    };
    assert_eq!(
        Tasked::default().of_snapshot(&user_home, &unknown, &[]),
        None
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent started under another Claude home is relaunched after aterm ended
/// ([`after_host_ended`]) off its own records in `<dir>`, never `~/.claude`'s: a record
/// left there names the conversation, a transcript there is resumed, and each line carries
/// the home. Controls: own projects with no transcript of it start afresh though
/// `~/.claude` holds one of the same id; an agent with no home relaunches off `~/.claude`.
#[cfg(unix)]
#[test]
fn an_agent_under_another_claude_home_is_relaunched_off_its_own_records() {
    /// Where the conversation's transcript is.
    #[derive(Clone, Copy)]
    enum Filed {
        /// Under the agent's own Claude home.
        Own,
        /// Under the user's `~/.claude` alone.
        UsersOnly,
    }
    // (name, under a home of its own, its crash left its record (else its
    // snapshot names the conversation), its transcript, resumed)
    let case = |name: &str, own_home: bool, record: bool, transcript: Filed, resumed: bool| {
        let _quick = QuickWaits::new();
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        let users = a.opts.home.join(".claude");
        // The user's own projects are readable whatever the agent ran under.
        std::fs::create_dir_all(users.join("projects/-other")).expect("the user's projects");
        let claude_dir = if own_home {
            let dir = a.dir.join("work home/.claude");
            a.snap.home = Some(dir.to_string_lossy().into_owned());
            dir
        } else {
            users.clone()
        };
        std::fs::create_dir_all(claude_dir.join("projects/-elsewhere")).expect("its projects");
        if record {
            a.snap.session = None;
            a.write_record_in(&claude_dir, SESSION);
        }
        match transcript {
            Filed::Own => file_transcript_in(&claude_dir, "-work"),
            Filed::UsersOnly => file_transcript_in(&users, "-work"),
        }
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        let r = after_host_ended(&a.opts, &a.snap, true);
        let saved =
            load(&a.opts, SESSION).unwrap_or_else(|| panic!("{name}: nothing relaunched: {r:?}"));
        let cause = if resumed {
            CAUSE_HOST
        } else {
            super::super::relaunch::CAUSE_FRESH
        };
        assert_eq!(saved.cause, cause, "{name}: {r:?}");
        assert_eq!(
            saved.line.contains(&format!("'--resume' '{SESSION}'")),
            resumed,
            "{name}: {}",
            saved.line
        );
        let home_word = format!("CLAUDE_CONFIG_DIR='{}' ", claude_dir.display());
        assert_eq!(
            saved.line.contains(&home_word),
            own_home,
            "{name}: {}",
            saved.line
        );
        // The record keeps the home its relaunched process files under
        // ([`St::home`]), for every step after the line: written only when
        // set, and read back.
        assert_eq!(saved.home, a.snap.home, "{name}");
        assert_eq!(
            saved.to_json().contains("\"home\""),
            own_home,
            "{name}: the key is written only when set"
        );
        assert_eq!(
            St::from_json(&saved.to_json()).map(|st| st.home),
            Some(a.snap.home.clone()),
            "{name}: read back"
        );
    };
    std::thread::scope(|scope| {
        let crashed = scope.spawn(|| case("home-record", true, true, Filed::Own, true));
        let known = scope.spawn(|| case("home-transcript", true, false, Filed::Own, true));
        let not_its = scope.spawn(|| case("home-not-its", true, false, Filed::UsersOnly, false));
        case("home-users", false, true, Filed::Own, true);
        crashed.join().expect("home-record");
        known.join().expect("home-transcript");
        not_its.join().expect("home-not-its");
    });
    // A record an older build wrote has no such key and reads as none: its relaunch looks
    // in `~/.claude`.
    let old = St::from_json(r#"{"phase":"relaunched","at":7,"cause":"host"}"#);
    assert_eq!(old.map(|st| st.home), Some(None));
}

/// The host's looks ([`look_at_exit`], [`follow`]) read the agent's record under the
/// Claude home its snapshot names: its crash reads as a crash, and a `/clear` it made is
/// followed.
#[cfg(unix)]
#[test]
fn the_hosts_looks_read_the_record_under_the_agents_own_claude_home() {
    const CLEARED: &str = "3d1c5a0e-7f42-4b8e-9c61-0a5b2e7d9f13";
    let mut a = DeadAgent::new("look-home");
    let claude_dir = a.dir.join("work home/.claude");
    a.snap.home = Some(claude_dir.to_string_lossy().into_owned());
    // The crash's record, where its Claude Code filed it.
    a.write_record_in(&claude_dir, SESSION);
    assert!(
        look_at_exit(&a.opts.home, &a.snap).record.is_some(),
        "its record is read where it is"
    );
    let left = a.look(|_| {});
    assert!(matches!(left, ExitRecord::Survived(_)), "a crash: {left:?}");
    // A `/clear` it made while it ran, followed.
    a.write_record_in(&claude_dir, CLEARED);
    let mut snap = a.snap.clone();
    assert!(follow(&a.opts.home, &mut snap), "followed");
    assert_eq!(snap.session.as_deref(), Some(CLEARED));
}

/// Claude's record of `pid` (started `start`, idle) holding [`SESSION`],
/// running in `cwd`, filed in the Claude Code directory `claude_dir`: where a
/// Claude Code whose home resolves there files it.
fn register_running_in(claude_dir: &Path, pid: u32, start: &str, cwd: &Path) {
    std::fs::create_dir_all(claude_dir.join("sessions")).expect("sessions");
    std::fs::write(
        claude_dir.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{SESSION}","cwd":"{}","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#,
            cwd.display()
        ),
    )
    .expect("the record");
}

/// A relative Claude home (`CLAUDE_CONFIG_DIR=work home/.claude`) is followed where the
/// line's `cd` puts it, its own record's directory: the host's look at the crash reads its
/// record, the relaunch after aterm ended records the resolved directory ([`St::home`]),
/// and the restart in place restarts it. Control: a record whose directory would move the
/// home is refused (`refused:home`).
#[cfg(unix)]
#[test]
fn a_relative_claude_home_is_followed_by_the_relaunch_and_the_restart_in_place() {
    const RELATIVE: &str = "work home/.claude";
    let record = register_running_in;
    // The look at a crash, and the relaunch after aterm itself ended.
    let _quick = QuickWaits::new();
    let mut a = DeadAgent::new("relative-home-relaunch");
    let (sock, _asked) = instance_with(&a.dir, Answers::default());
    a.opts = Opts {
        sock: Some(sock),
        dry_run: false,
        ..a.opts.clone()
    };
    let work = a.dir.to_path_buf();
    let claude_dir = work.join(RELATIVE);
    a.snap.cwd = work.to_string_lossy().into_owned();
    a.snap.home = Some(RELATIVE.to_string());
    record(&claude_dir, a.snap.pid, &a.snap.start, &work);
    file_transcript_in(&claude_dir, "-work");
    assert!(
        look_at_exit(&a.opts.home, &a.snap).record.is_some(),
        "its crash's record is read where its home resolves"
    );
    std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
    let r = after_host_ended(&a.opts, &a.snap, true);
    let saved = load(&a.opts, SESSION).unwrap_or_else(|| panic!("nothing relaunched: {r:?}"));
    assert!(
        saved.line.contains(&format!("'--resume' '{SESSION}'")),
        "resumed: {}",
        saved.line
    );
    assert!(
        saved
            .line
            .contains(&format!("CLAUDE_CONFIG_DIR='{RELATIVE}' ")),
        "the line carries the home as the agent had it: {}",
        saved.line
    );
    assert_eq!(
        saved.home.as_deref().map(Path::new),
        Some(claude_dir.as_path()),
        "the relaunched process is looked for where it files its record"
    );

    // The restart in place of a live agent registered under it.
    let dir = scratch("relative-home-restart");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
    let mut shell = stand_in_shell();
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    let work = dir.join("work");
    let claude_dir = work.join(RELATIVE);
    let snap = Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: agent.id(),
        start: start.clone(),
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(SESSION.to_string()),
        cwd: work.to_string_lossy().into_owned(),
        version: Some("1.0.0".to_string()),
        codex: None,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: Some(RELATIVE.to_string()),
        claude_dir: None,
    };
    record(&claude_dir, agent.id(), &start, &work);
    let restarted = restart_from(&opts, &Restart::Memory, Ok(snap.clone())).step;
    // Control: its record says it runs elsewhere, where the line would `cd`
    // and so move its home.
    record(&claude_dir, agent.id(), &start, &dir.join("elsewhere"));
    let moved = restart_from(&opts, &Restart::Memory, Ok(snap)).step;
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(restarted, "would-restart", "restarted on its record");
    assert_eq!(
        moved, "would-refuse:home",
        "a line that would move its home"
    );
}

/// The restart in place of a relative home's agent records the directory the relaunched
/// process files under ([`Records::home_word`]), not the raw value, which names no
/// directory: driven past its signal (`relaunch::restart_as`), the relaunched process is
/// found through the saved record ([`St::agent_records`]).
#[cfg(unix)]
#[test]
fn a_restart_in_place_records_the_directory_a_relative_homes_agent_files_under() {
    const RELATIVE: &str = "work home/.claude";
    let dir = scratch("restart-relative-home-record");
    let (sock, _asked) = instance_with(&dir, Answers::default());
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let work = dir.join("work");
    let claude_dir = work.join(RELATIVE);
    let mut shell = stand_in_shell();
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let start = kernel_start(agent.id()).expect("lstart");
    register_running_in(&claude_dir, agent.id(), &start, &work);
    file_transcript_in(&claude_dir, "-work");
    let snap = Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: agent.id(),
        start,
        shell: shell.id(),
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(SESSION.to_string()),
        cwd: work.to_string_lossy().into_owned(),
        version: Some("1.0.0".to_string()),
        codex: None,
        dialect: None,
        login: None,
        home: Some(RELATIVE.to_string()),
        claude_dir: None,
    };
    let r = crate::harness::relaunch::restart_as(
        &opts,
        &Restart::Memory,
        &crate::harness::accounts::Roster::default(),
        Ok(snap),
        &Script::new(shell.id(), usize::MAX, None),
    );
    let saved = load(&opts, SESSION);
    // The relaunched process files its record under the same home.
    let mut relaunched = parked_bare().spawn().expect("the relaunched agent");
    wait_exec(relaunched.id());
    let again = kernel_start(relaunched.id()).expect("lstart");
    register_running_in(&claude_dir, relaunched.id(), &again, &work);
    let found = saved
        .as_ref()
        .and_then(|st| st.agent_records(&opts.home).session_file(relaunched.id()));
    let _ = relaunched.kill();
    let _ = relaunched.wait();
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let saved = saved.unwrap_or_else(|| panic!("nothing restarted: {r:?}"));
    assert!(
        saved
            .line
            .contains(&format!("CLAUDE_CONFIG_DIR='{RELATIVE}' ")),
        "the line carries the home as the agent had it: {}",
        saved.line
    );
    assert_eq!(
        saved.home.as_deref().map(Path::new),
        Some(claude_dir.as_path()),
        "the record names the directory, not the raw value"
    );
    assert!(
        found.is_some(),
        "the relaunched process is found where it files its record"
    );
}

/// The restart in place types the usage tap under the switch and writes no settings file:
/// driven for real past its signal ([`super::super::relaunch::restart_as`], a stand-in agent on
/// its stored login, its job and terminal scripted), the line it types and records carries
/// `--settings <the tap>` before `--resume` when `[harness] usage_tap` names this aterm
/// ([`Opts::usage_tap`]), and the user's `settings.json` is left byte for byte. On the
/// machine's one account, renewed, the follow onto the renewed login types that login and the
/// tap with it (`tap::Armed::lone_login`). Negative control: the switch off types no
/// `--settings` (and a line armed for the default login alone carries none on the renewed one:
/// [`the_planned_relaunch_line_carries_the_usage_tap_only_where_its_gate_holds`]).
#[cfg(unix)]
#[test]
fn a_restart_in_place_types_the_usage_tap_under_the_switch_and_writes_no_settings() {
    use super::super::accounts::{self, Roster};
    use super::super::tap;
    let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let value = tap::settings_value(exe).expect("an admissible path");
    let armed = |lone_login: Option<&str>| {
        Some(tap::Armed {
            exe: exe.to_path_buf(),
            lone_login: lone_login.map(str::to_owned),
        })
    };
    let run = |label: &str, usage_tap: Option<tap::Armed>, why: &Restart, roster: &Roster| {
        let dir = scratch(label);
        let (sock, _asked) = instance_with(&dir, Answers::default());
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            usage_tap,
            ..drive(&dir)
        };
        seed_builds(&opts);
        let claude_dir = opts.home.join(".claude");
        std::fs::create_dir_all(claude_dir.join("sessions")).expect("the user's sessions");
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        let settings = claude_dir.join("settings.json");
        let text = "{\n  \"permissions\": {\"allow\": [\"Bash(ls)\"]}\n}\n";
        std::fs::write(&settings, text).expect("the user's settings");
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("work");
        let mut shell = stand_in_shell();
        let mut agent = parked_bare().spawn().expect("agent");
        wait_exec(agent.id());
        let start = kernel_start(agent.id()).expect("lstart");
        register_running_in(&claude_dir, agent.id(), &start, &work);
        file_transcript_in(&claude_dir, "-work");
        let snap = Snapshot {
            stored_login: true,
            tab: TAB.to_string(),
            pid: agent.id(),
            start,
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: work.to_string_lossy().into_owned(),
            version: Some("1.0.0".to_string()),
            codex: None,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        let r = crate::harness::relaunch::restart_as(
            &opts,
            why,
            roster,
            Ok(snap),
            &Script::new(shell.id(), usize::MAX, None),
        );
        let saved = load(&opts, SESSION);
        let after = std::fs::read_to_string(&settings).expect("settings");
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let saved = saved.unwrap_or_else(|| panic!("nothing restarted: {r:?}"));
        assert_eq!(after, text, "the user's settings are untouched");
        saved.line
    };
    let none = Roster::default();
    let tapped = run("restart-usage-tap-on", armed(None), &Restart::Memory, &none);
    assert!(
        tapped.contains(&format!("'--settings' '{value}' '--resume' '{SESSION}'")),
        "{tapped}"
    );
    let plain = run("restart-usage-tap-off", None, &Restart::Memory, &none);
    assert!(!plain.contains("--settings"), "{plain}");
    assert!(
        plain.contains(&format!("'--resume' '{SESSION}'")),
        "{plain}"
    );
    // The one account (`adopt-default`), renewed: bound to a login of its own.
    let renewed = "/tmp/aterm-state/logins/home-a.1";
    let one = Roster::parse(
        "[[account]]\nlabel = \"home-a\"\nclass = \"dedicated\"\nemail = \"owner@example.com\"\n",
    )
    .expect("a roster");
    let mut logins = accounts::Logins::default();
    let mut state = accounts::State::default();
    accounts::note_renewed(&mut state, &mut logins, "home-a", None, renewed, None, 1);
    let one = accounts::bound(&one, &logins);
    let follow = Restart::Account {
        to: "home-a".to_string(),
        at: crate::supervise::policy::turn_end::AccountAt::Ahead,
    };
    let followed = run(
        "restart-usage-tap-follow",
        armed(one.lone_login()),
        &follow,
        &one,
    );
    assert!(
        followed.contains(&format!(
            "CLAUDE_SECURESTORAGE_CONFIG_DIR={renewed} CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1"
        )),
        "{followed}"
    );
    assert!(
        followed.contains(&format!("'--settings' '{value}' '--resume' '{SESSION}'")),
        "{followed}"
    );
}

/// The relaunch on exit refuses a line whose `cd` would move a relative home (the crash's
/// record says it ran elsewhere, where the same home names another directory):
/// `refused:home` ([`home_carried`]), never typed. Control: a record naming the directory
/// the home resolves from is relaunched.
#[cfg(unix)]
#[test]
fn the_relaunch_on_exit_refuses_a_line_that_would_move_a_relative_home() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, ran_in: &str| {
        let mut a = DeadAgent::new(name);
        let work = a.dir.join("work");
        let claude_dir = work.join(RELATIVE);
        a.snap.cwd = work.to_string_lossy().into_owned();
        a.snap.home = Some(RELATIVE.to_string());
        a.snap.claude_dir = Some(claude_dir.clone());
        register_running_in(&claude_dir, a.snap.pid, &a.snap.start, &work.join(ran_in));
        file_transcript_in(&claude_dir, "-work");
        after_exit(&a.opts, &a.snap, &ExitRecord::Unread, false, None).step
    };
    assert_eq!(
        case("exit-relative-home-moved", "elsewhere"),
        "would-refuse:home",
        "a line that would move its home"
    );
    assert_eq!(
        case("exit-relative-home-kept", ""),
        "would-relaunch",
        "CONTROL: its record names where its home resolves"
    );
}

/// [`super::super::relaunch::snapshot`] itself (other tests hand-build the snapshot) reads
/// the tab leader's `CLAUDE_CONFIG_DIR` through [`Records::of_process`] (relative or empty,
/// against the directory it runs in) and keeps the resolved directory
/// ([`Snapshot::claude_dir`]) with the conversation it names. Control: an absolute home and
/// the user's own.
#[cfg(unix)]
#[test]
fn the_relaunch_snapshot_reads_a_relative_or_empty_home_where_its_agent_runs() {
    /// The agent's `CLAUDE_CONFIG_DIR`.
    #[derive(Clone, Copy)]
    enum Home {
        /// Unset: the user's own `~/.claude`.
        Users,
        /// `<scratch>/abs/.claude`.
        Absolute,
        /// `work home/.claude`, under the directory the agent runs in.
        Relative,
        /// Set and empty: the directory the agent runs in itself.
        Empty,
    }
    let case = |name: &str, home: Home| {
        let dir = scratch(name);
        let work = dir.join("work");
        std::fs::create_dir_all(&work).expect("its working directory");
        let absolute = dir.join("abs/.claude");
        // Where the kernel says it runs: a relative home resolves there.
        let runs_in = std::fs::canonicalize(&work).expect("its working directory");
        let (raw, claude_dir) = match home {
            Home::Users => (None, dir.join("home/.claude")),
            Home::Absolute => (
                Some(absolute.to_string_lossy().into_owned()),
                absolute.clone(),
            ),
            Home::Relative => (
                Some("work home/.claude".to_string()),
                runs_in.join("work home/.claude"),
            ),
            Home::Empty => (Some(String::new()), runs_in.clone()),
        };
        let env: Vec<(&str, &Path)> = raw
            .as_deref()
            .map(|h| (crate::harness::accounts::HOME_ENV, Path::new(h)))
            .into_iter()
            .collect();
        let mut agent = tty_agent(&env, Some(&work));
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, _asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        let start = kernel_start(pid).expect("lstart");
        register_running_in(&claude_dir, pid, &start, &work);
        let snap = crate::harness::relaunch::snapshot_with(
            &opts,
            &Script::new(std::process::id(), usize::MAX, None),
        );
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let snap = snap.unwrap_or_else(|why| panic!("{name}: no snapshot: {why}"));
        assert_eq!(
            (snap.session.as_deref(), snap.home.as_deref()),
            (Some(SESSION), raw.as_deref()),
            "{name}: its conversation, and its home as it had it"
        );
        assert_eq!(
            snap.claude_dir.as_deref(),
            Some(claude_dir.as_path()),
            "{name}: where its records are"
        );
    };
    case("snapshot-relative-home", Home::Relative);
    case("snapshot-empty-home", Home::Empty);
    case("snapshot-absolute-home", Home::Absolute);
    case("snapshot-users-home", Home::Users);
}

/// An unset `CLAUDE_CONFIG_DIR` is the agent's own `HOME`'s `.claude`, not the window's
/// (`homedir()` is the agent's `HOME`): the relaunch snapshot reads its record there and
/// the sweep lists that home. `footer::claude_dir_of`, the one resolver, reads an empty
/// value as the directory the process runs in and a relative one under it.
#[cfg(unix)]
#[test]
fn an_unset_claude_home_is_the_agents_own_homes_not_the_windows() {
    let dir = scratch("agent-home-unset");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("its working directory");
    // The agent's own HOME, which is not the window's (`dir/home`).
    let other = dir.join("other");
    let claude_dir = other.join(".claude");
    let mut agent = tty_agent(&[("HOME", other.as_path())], Some(&work));
    let pid = agent.id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    let start = kernel_start(pid).expect("lstart");
    register_running_in(&claude_dir, pid, &start, &work);
    let snap = crate::harness::relaunch::snapshot_with(
        &opts,
        &Script::new(std::process::id(), usize::MAX, None),
    );
    // The sweep's homes, read off the tab's leader.
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(pid)),
    }];
    let homes = Homes::read(
        &opts,
        Some(&tabs),
        atpkg::caller_shell::process_args,
        aterm_sysprobe::process_cwd,
    );
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let snap = snap.unwrap_or_else(|why| panic!("no snapshot: {why}"));
    assert_eq!(
        (snap.session.as_deref(), snap.claude_dir.as_deref()),
        (Some(SESSION), Some(claude_dir.as_path())),
        "the snapshot reads its record where its own HOME keeps it"
    );
    assert_eq!(
        homes.dirs(),
        vec![claude_dir.clone()],
        "the sweep lists its home"
    );
    // The footer's reader, the one resolver: an empty `CLAUDE_CONFIG_DIR` is
    // the directory the process runs in, a relative one under it.
    use crate::harness::footer::claude_dir_of;
    let home = "/Users//someone";
    let runs_in = || Some(PathBuf::from("/work"));
    assert_eq!(
        claude_dir_of(Some(""), Some(home), runs_in),
        Some(PathBuf::from("/work")),
        "an empty CLAUDE_CONFIG_DIR read as unset"
    );
    assert_eq!(
        claude_dir_of(Some("rel/.claude"), Some(home), runs_in),
        Some(PathBuf::from("/work/rel/.claude")),
        "a relative CLAUDE_CONFIG_DIR read as none"
    );
    assert_eq!(
        claude_dir_of(None, Some(home), runs_in),
        Some(PathBuf::from("/Users//someone/.claude"))
    );
}

/// [`Records::at`] for an agent's own `HOME`: Claude Code keeps the profile at
/// `join(CLAUDE_CONFIG_DIR || homedir(), ".claude.json")`, `homedir()` the agent's `HOME`,
/// never the window's ([`profile_apart`]); [`St::keep_home`] keeps both. Control: an
/// agent whose `HOME` is the user's is recorded as none.
#[test]
fn an_agents_own_home_keeps_its_records_and_its_profile() {
    let user = Path::new("/Users//someone");
    let other = "/Users//someone/other";
    let records = Records::at(
        user,
        HomeVars {
            config: None,
            home: Some(other),
        },
        || None,
    );
    assert_eq!(
        records.dir(),
        Some(Path::new("/Users//someone/other/.claude"))
    );
    assert_eq!(
        records.profile(user),
        Some(PathBuf::from("/Users//someone/other/.claude.json"))
    );
    let mut st = St::default();
    assert!(st.keep_home(&records, user));
    assert_eq!(st.home.as_deref(), Some("/Users//someone/other/.claude"));
    assert_eq!(
        st.home_profile.as_deref(),
        Some("/Users//someone/other/.claude.json")
    );
    assert_eq!(st.agent_records(user), records, "read back whole");
    // An empty one: its records where it runs, its profile in its own HOME.
    let empty = Records::at(
        user,
        HomeVars {
            config: Some(""),
            home: Some(other),
        },
        || Some(PathBuf::from("/work")),
    );
    assert_eq!(empty.dir(), Some(Path::new("/work")));
    assert_eq!(
        empty.profile(user),
        Some(PathBuf::from("/Users//someone/other/.claude.json"))
    );
    // Control: its HOME is the user's, or not known.
    for home in [Some("/Users//someone"), None] {
        let own = Records::at(user, HomeVars { config: None, home }, || None);
        assert_eq!(own.dir(), Some(Path::new("/Users//someone/.claude")));
        assert_eq!(own.home_word(user), None, "{home:?}");
        assert_eq!(
            own.profile(user),
            Some(PathBuf::from("/Users//someone/.claude.json"))
        );
    }
}

/// The user's own `~/.claude` named by `CLAUDE_CONFIG_DIR` keeps its profile inside it
/// ([`Records::at`], [`profile_apart`]): Claude Code keeps the profile at
/// `join(CLAUDE_CONFIG_DIR || homedir(), ".claude.json")`, so
/// `CLAUDE_CONFIG_DIR=<home>/.claude` (how the real-Claude lab launches it) reads
/// `<home>/.claude/.claude.json`, never the default home's `<home>/.claude.json`. The directory
/// alone ([`claude_profile`]) says beside it, so the profile is kept apart: a record of such an
/// agent keeps it with no home ([`St::keep_home`]) and reads it back ([`St::agent_records`]).
/// Control: with the variable unset, the same directory keeps it beside, and the record keeps
/// nothing apart.
#[test]
fn the_users_own_claude_home_named_explicitly_keeps_its_profile_inside_it() {
    let user = Path::new("/Users//someone");
    let named = Records::at(
        user,
        HomeVars::config(Some("/Users//someone/.claude")),
        || None,
    );
    assert_eq!(named.dir(), Some(Path::new("/Users//someone/.claude")));
    assert_eq!(
        named.profile(user),
        Some(PathBuf::from("/Users//someone/.claude/.claude.json"))
    );
    let mut st = St::default();
    assert!(st.keep_home(&named, user));
    assert_eq!(st.home, None, "the directory is the user's own");
    assert_eq!(
        st.home_profile.as_deref(),
        Some("/Users//someone/.claude/.claude.json")
    );
    assert_eq!(st.agent_records(user), named, "read back whole");
    // Control: unset, the same directory, the profile beside it.
    let unset = Records::at(user, HomeVars::config(None), || None);
    assert_eq!(unset.dir(), named.dir());
    assert_eq!(
        unset.profile(user),
        Some(PathBuf::from("/Users//someone/.claude.json"))
    );
    let mut st = St::default();
    assert!(!st.keep_home(&unset, user), "nothing kept apart");
    assert_eq!((st.home, st.home_profile), (None, None));
}

/// Every reader finds a relative home's records where its agent filed them at start, though it
/// changed directory since (Claude Code keeps `CLAUDE_CONFIG_DIR` unresolved and opens paths from
/// its current directory): the relaunch snapshot and the sweep read there. Control: a stand-in
/// that never moved.
#[cfg(unix)]
#[test]
fn every_reader_finds_a_relative_homes_records_where_its_agent_filed_them() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, moved: bool| {
        let dir = scratch(name);
        let work = dir.join("work");
        let wt = work.join("wt");
        std::fs::create_dir_all(&wt).expect("its directories");
        let started = std::fs::canonicalize(&work).expect("where it started");
        let claude_dir = started.join(RELATIVE);
        let runs_in = if moved { &wt } else { &work };
        let mut agent = tty_agent(
            &[
                (crate::harness::accounts::HOME_ENV, Path::new(RELATIVE)),
                ("PWD", started.as_path()),
            ],
            Some(runs_in),
        );
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, _asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
        // Filed where it started, a build nobody can compare.
        register_as(&claude_dir, pid, SESSION, "unknown");
        file_transcript_in(&claude_dir, "-work");
        // The window's relaunch snapshot and the sweep's look, each read on
        // its own.
        let snap = crate::harness::relaunch::snapshot_with(
            &opts,
            &Script::new(std::process::id(), usize::MAX, None),
        );
        let due_now = due(&opts);
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        let named = snap
            .as_ref()
            .map(|snap| (snap.session.clone(), snap.claude_dir.clone()));
        assert_eq!(
            (named, due_now),
            (Ok((Some(SESSION.to_string()), Some(claude_dir))), Due::Yes),
            "{name}: the snapshot names its conversation where it filed it, the sweep finds it"
        );
    };
    case("records-where-filed-unmoved", false);
    case("records-where-filed-moved", true);
}

/// The live upgrade finds an agent under another Claude home (`CLAUDE_CONFIG_DIR=<dir>`
/// files `sessions/<pid>.json` in `<dir>`) through the host's own reads (the `who`
/// roster, the agent's exec environment): due and visited, under an absolute or relative
/// home and with no `~/.claude/sessions` at all. Control: the same agent under the
/// user's home.
#[cfg(unix)]
#[test]
fn the_live_upgrade_finds_an_agent_under_another_claude_home() {
    /// The agent's Claude home.
    #[derive(Clone, Copy, PartialEq)]
    enum Home {
        /// The user's own (no `CLAUDE_CONFIG_DIR`).
        Users,
        /// `CLAUDE_CONFIG_DIR=<dir>`.
        Absolute,
        /// `CLAUDE_CONFIG_DIR=work home/.claude`, the agent running in the
        /// scratch directory: resolved against it, as Claude Code does.
        Relative,
    }
    // `users_records`: the user's own home keeps Claude records (its
    // `sessions/`); a machine whose agents all run under other homes has none.
    let case = |name: &str, home: Home, users_records: bool| {
        let dir = scratch(name);
        let claude_dir = dir.join("work home/.claude");
        let relative = Path::new("work home/.claude");
        let env: Vec<(&str, &Path)> = match home {
            Home::Users => Vec::new(),
            Home::Absolute => vec![(crate::harness::accounts::HOME_ENV, claude_dir.as_path())],
            Home::Relative => vec![(crate::harness::accounts::HOME_ENV, relative)],
        };
        let mut agent = tty_agent(&env, Some(&dir));
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, _asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        // The user's own home keeps records too, none of this agent's — or,
        // on a machine whose agents all run under other homes, none at all.
        let users = opts.home.join(".claude");
        if users_records {
            std::fs::create_dir_all(users.join("sessions")).expect("the user's sessions");
        }
        // Its record, where its Claude Code files it. A build nobody can
        // compare is looked at as a behind one is, whatever this machine
        // has installed.
        register_as(
            if home == Home::Users {
                &users
            } else {
                &claude_dir
            },
            pid,
            SESSION,
            "unknown",
        );
        let due_now = due(&opts);
        let r = step(&Opts {
            dry_run: true,
            ..opts.clone()
        });
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(due_now, Due::Yes, "{name}: the tab's agent is due");
        assert_eq!(
            (r.tab.as_str(), r.session.as_str(), r.pid),
            (TAB, SESSION, pid),
            "{name}: the step's pass visits it: {r:?}"
        );
    };
    case("sweep-users-home", Home::Users, true);
    case("sweep-own-home", Home::Absolute, true);
    case("sweep-relative-home", Home::Relative, true);
    case("sweep-own-home-only", Home::Absolute, false);
}

/// An agent under a set-and-empty `CLAUDE_CONFIG_DIR` is read where Claude Code files it,
/// in the directory it runs in (its `??` keeps the empty string): the live upgrade finds
/// it due and visits it, and the host's look at its crash finds its record.
#[cfg(unix)]
#[test]
fn an_agent_under_an_empty_claude_home_is_read_where_claude_code_files_it() {
    // The live upgrade: the agent leading the tab, under `CLAUDE_CONFIG_DIR=` in `work`,
    // its record where its Claude Code files it.
    let dir = scratch("sweep-empty-home");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("its working directory");
    let mut agent = tty_agent(
        &[(crate::harness::accounts::HOME_ENV, Path::new(""))],
        Some(&work),
    );
    let pid = agent.id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
    register_as(&work, pid, SESSION, "unknown");
    let due_now = due(&opts);
    let r = step(&Opts {
        dry_run: true,
        ..opts.clone()
    });
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    // The host's look at its crash.
    let mut a = DeadAgent::new("look-empty-home");
    a.snap.cwd = a.dir.to_string_lossy().into_owned();
    a.snap.home = Some(String::new());
    a.write_record_in(&a.dir, SESSION);
    let looked = look_at_exit(&a.opts.home, &a.snap).record.is_some();
    assert_eq!(due_now, Due::Yes, "the tab's agent is due");
    assert_eq!(
        (r.tab.as_str(), r.session.as_str(), r.pid),
        (TAB, SESSION, pid),
        "the step's pass visits it: {r:?}"
    );
    assert!(looked, "its crash's record is read where it filed it");
}

/// The upgrade view shows a pending upgrade only while a live process holds its
/// conversation (`Row::standing`); the holder is read under the home the record names
/// ([`St::home`]). Control: a record with no home, its agent under the user's.
#[cfg(unix)]
#[test]
fn the_upgrade_view_counts_an_agent_under_another_claude_home() {
    let case = |name: &str, own_home: bool| {
        let dir = scratch(name);
        let opts = drive(&dir);
        let users = opts.home.join(".claude");
        let claude_dir = if own_home {
            dir.join("work home/.claude")
        } else {
            users.clone()
        };
        std::fs::create_dir_all(users.join("sessions")).expect("the user's sessions");
        let mut agent = parked();
        if own_home {
            agent.env(crate::harness::accounts::HOME_ENV, &claude_dir);
        }
        let mut agent = agent.spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register_in(&claude_dir, agent.id(), SESSION);
        let now = now_s();
        let st = St {
            phase: Phase::Pending,
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            salt: now,
            pending_since: now,
            proc_start: squash(&sf.proc_start),
            home: own_home.then(|| claude_dir.to_string_lossy().into_owned()),
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        save(&opts, SESSION, &st);
        let (rows, whole) = status_rows(&opts, false);
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(whole, "{name}: the holders read whole");
        assert!(
            rows.iter()
                .any(|row| row.session == SESSION && row.tab == TAB),
            "{name}: shown and counted: {rows:?}"
        );
    };
    case("view-users-home", false);
    case("view-own-home", true);
}

/// [`Homes`]: the Claude homes one pass reads besides the user's, off each tab leader's exec
/// environment: `CLAUDE_CONFIG_DIR` as set, or resolved against the leader's working directory
/// ([`home_dir_at`]); only the scoped tab ([`Opts::only_sid`]); each directory once and never the
/// user's own, even by a link (read twice, every record would be a second owner of its
/// conversation). A home that cannot be named is marked `unnamed`, never read as the user's;
/// with no roster, the records' homes.
#[cfg(unix)]
#[test]
fn a_pass_reads_every_claude_home_its_tabs_agents_run_under() {
    let dir = scratch("homes");
    let opts = drive(&dir);
    let users = opts.home.join(".claude");
    std::fs::create_dir_all(&users).expect("the user's home");
    let link = dir.join("users-link");
    std::os::unix::fs::symlink(&users, &link).expect("a link to the user's home");
    let link = link.to_string_lossy().into_owned();
    let tab = |sid: &str, group: Option<i64>| LiveTab {
        sid: sid.to_string(),
        fgpgid: group,
    };
    // Two live leaders whose home cannot be named (this process and its
    // parent), and one gone.
    let me = std::process::id();
    // SAFETY: getppid takes nothing and cannot fail.
    let parent = u32::try_from(unsafe { libc::getppid() }).expect("a parent");
    let gone = dead_pid();
    let tabs = [
        tab("s-a", Some(1)),
        tab("s-b", Some(2)),
        tab("s-c", Some(3)),
        tab("s-d", Some(4)),
        tab("s-e", Some(i64::from(me))),
        tab("s-f", Some(i64::from(parent))),
        tab("s-g", Some(7)),
        tab("s-h", None),
        tab("s-i", Some(1)),
        tab("s-j", Some(i64::from(gone))),
    ];
    let env = |home: Option<&str>| {
        Some(atpkg::caller_shell::ProcArgs {
            env: home
                .map(|h| vec![format!("CLAUDE_CONFIG_DIR={h}")])
                .unwrap_or_default(),
            ..atpkg::caller_shell::ProcArgs::default()
        })
    };
    let read_args = |pid: u32| match pid {
        1 => env(Some("/work/a/.claude")),
        // Against its working directory, `/work/b`.
        2 => env(Some("rel/../rel/.claude")),
        // The user's own.
        3 => env(None),
        // Set and empty: its working directory itself, `/work/d`.
        4 => env(Some("")),
        // Relative, its working directory unread.
        p if p == parent => env(Some("rel/.claude")),
        // The user's own, by a link.
        7 => env(Some(&link)),
        // Its exec record unread (`me`; `gone`, which leads nothing now).
        _ => None,
    };
    let read_cwd = |pid: u32| match pid {
        2 => Some(PathBuf::from("/work/b")),
        4 => Some(PathBuf::from("/work/d")),
        _ => None,
    };
    let homes = Homes::read(&opts, Some(&tabs), read_args, read_cwd);
    assert_eq!(
        homes.dirs(),
        [
            PathBuf::from("/work/a/.claude"),
            PathBuf::from("/work/b/rel/.claude"),
            PathBuf::from("/work/d"),
        ]
    );
    assert_eq!(homes.unnamed, ["s-e", "s-f"]);
    assert!(homes.unnamed("s-f") && !homes.unnamed("s-a"));
    // The window's step: its own tab alone.
    let scoped = |sid: &str| Opts {
        only_sid: Some(sid.to_string()),
        ..opts.clone()
    };
    let homes = Homes::read(&scoped("s-b"), Some(&tabs), read_args, read_cwd);
    assert_eq!(homes.dirs(), [PathBuf::from("/work/b/rel/.claude")]);
    assert!(homes.unnamed.is_empty());
    let homes = Homes::read(&scoped("s-c"), Some(&tabs), read_args, read_cwd);
    assert_eq!(homes, Homes::default(), "the user's own is read anyway");
    // No roster: the records' homes, each once.
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    for (session, home) in [
        (
            "aaaaaaaa-0000-0000-0000-000000000001",
            Some("/work/a/.claude"),
        ),
        (
            "aaaaaaaa-0000-0000-0000-000000000002",
            Some("/work/a/.claude"),
        ),
        ("aaaaaaaa-0000-0000-0000-000000000003", None),
    ] {
        let st = St {
            home: home.map(str::to_string),
            ..St::default()
        };
        save(&opts, session, &st);
    }
    let homes = Homes::read(
        &opts,
        None,
        |_| panic!("no process is read without a roster"),
        |_| panic!("no process is read without a roster"),
    );
    assert_eq!(homes.dirs(), [PathBuf::from("/work/a/.claude")]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`Homes::roster`]: every home's records, each stamped with where it was read
/// ([`SessionFile::claude_dir`]). A home with no `sessions/` yet keeps none; one that cannot be
/// read whole (a file where `sessions/` should be) leaves the whole roster unread, as the user's
/// own does.
#[test]
fn a_roster_reads_every_home_whole_or_not_at_all() {
    let dir = scratch("roster");
    let me = std::process::id();
    let a = dir.join("a/.claude");
    register_as(&a, me, SESSION, "1.0.0");
    let fresh = dir.join("fresh/.claude");
    std::fs::create_dir_all(&fresh).expect("a home with no records yet");
    let broken = dir.join("broken/.claude");
    std::fs::create_dir_all(&broken).expect("a home");
    std::fs::write(broken.join("sessions"), "").expect("records that cannot be listed");
    let homes = |dirs: &[&PathBuf]| Homes {
        dirs: dirs
            .iter()
            .map(|d| Records::of(&dir.join("home"), d.to_str()))
            .collect(),
        unnamed: Vec::new(),
    };
    let got = homes(&[&a, &fresh])
        .roster(Some(Vec::new()))
        .expect("read whole");
    assert_eq!(got.len(), 1);
    assert_eq!(
        (got[0].pid, got[0].claude_dir.as_deref()),
        (me, Some(a.as_path()))
    );
    assert_eq!(homes(&[&a, &broken]).roster(Some(Vec::new())), None);
    assert_eq!(homes(&[&a]).roster(None), None, "the user's own unread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`home_carried`]: a relative Claude home is carried only where the relaunch line's `cd`
/// keeps it resolving to the agent's records; elsewhere it is refused (`refused:home`). An
/// absolute home, none, and a line starting it where it runs now are carried.
#[test]
fn a_relative_claude_home_the_line_would_move_is_refused() {
    let user = Path::new("/home");
    let records = Records::of(user, Some("/work/b/rel/.claude"));
    let refused = |r: Result<(), NoPlan>| matches!(r, Err(NoPlan::Refused { why, what, .. }) if why == "line:home" && what == "home");
    assert!(home_carried(user, None, "/work/c", &Records::of(user, None)).is_ok());
    assert!(home_carried(user, Some("/elsewhere/.claude"), "/work/c", &records).is_ok());
    assert!(home_carried(user, Some("rel/.claude"), "/work/b", &records).is_ok());
    assert!(home_carried(user, Some("rel/.claude"), "", &records).is_ok());
    assert!(refused(home_carried(
        user,
        Some("rel/.claude"),
        "/work/c",
        &records
    )));
    // An unset home under another `HOME`: its records are `<HOME>/.claude`, and the line,
    // carrying no `HOME`, would start it under its shell's.
    let other = Records::at(
        user,
        HomeVars {
            config: None,
            home: Some("/other"),
        },
        || None,
    );
    assert!(refused(home_carried(user, None, "/work/c", &other)));
}

/// The upgrade's record names the Claude home its agent runs under ([`St::home`]), written
/// by the visit and read back by [`St::agent_records`]. Control: an agent under the user's
/// own home writes none.
#[cfg(unix)]
#[test]
fn an_upgrades_record_names_the_claude_home_its_agent_runs_under() {
    for (name, own) in [("bind-own", true), ("bind-users", false)] {
        let dir = scratch(name);
        let opts = drive(&dir);
        let claude_dir = if own {
            dir.join("work home/.claude")
        } else {
            opts.home.join(".claude")
        };
        let mut agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register_in(&claude_dir, agent.id(), SESSION);
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        let r = visit(&opts, &sf, &table(), &newer(), &Live);
        let saved = load(&opts, SESSION).expect("the record");
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(r.session, SESSION, "{name}: visited: {r:?}");
        assert_eq!(
            saved.home,
            own.then(|| claude_dir.to_string_lossy().into_owned()),
            "{name}: {r:?}"
        );
        assert_eq!(saved.to_json().contains("\"home\""), own, "{name}");
        assert_eq!(
            saved.agent_records(&opts.home).dir(),
            Some(claude_dir.as_path()),
            "{name}: read back where its agent keeps its records"
        );
    }
}

/// An exit's relaunch saves the fallback origin it decides ([`after_exit`] into
/// [`exit_fallback`]): a hand `/model` choice since becomes the origin. Negative control:
/// no `/model`, the fallback's own origin is carried. The cases run side by side, each
/// waiting out the stand-in shell's prompt.
#[cfg(unix)]
#[test]
fn an_exit_relaunch_saves_the_fallback_origin_it_decides() {
    let origin = "claude-fable-5-1";
    let fallback = "claude-sonnet-5-5";
    let hand = "claude-opus-5-5";
    let case = |name: &str, chose: Option<&str>, want: &str| {
        let _quick = QuickWaits::new();
        let mut a = DeadAgent::new(name);
        let (sock, _asked) = instance_with(&a.dir, Answers::default());
        a.opts = Opts {
            sock: Some(sock),
            dry_run: false,
            ..a.opts.clone()
        };
        a.snap.argv = ["/opt/claude/bin/claude", "--model", fallback]
            .iter()
            .map(|w| (*w).to_string())
            .collect();
        // Answered past the agent's start (Sep 24 2026).
        write_transcript(
            &a.opts.home,
            &[answer_at(chose.unwrap_or(fallback), "2026-09-25T00:00:00Z")],
        );
        if let Some(hand) = chose {
            chose_by_hand(&a.opts, hand);
        }
        std::fs::create_dir_all(state_dir(&a.opts)).expect("state");
        save(
            &a.opts,
            SESSION,
            &saved_restart(&model_restart(
                &Restart::Model {
                    to: fallback.to_string(),
                },
                None,
                origin.to_string(),
                None,
                None,
            )),
        );
        a.write_record();
        let left = a.look(|_| {});
        let r = after_exit(&a.opts, &a.snap, &left, true, None);
        let saved = load(&a.opts, SESSION).expect("state");
        assert_eq!(saved.cause, CAUSE_EXIT, "{name}: {r:?}");
        assert_eq!(saved.fallback_from.as_deref(), Some(want), "{name}: {r:?}");
        assert!(
            saved
                .line
                .contains(&format!("'--model' '{}'", chose.unwrap_or(fallback))),
            "{name}: {}",
            saved.line
        );
    };
    std::thread::scope(|scope| {
        let hand_case = scope.spawn(|| case("exit-saved-hand", Some(hand), hand));
        case("exit-saved-nobody", None, origin);
        hand_case.join().expect("exit-saved-hand");
    });
}

/// A relaunch left in flight finds its process under the Claude home its line carried
/// ([`St::home`]), at adoption and at its loop's carry-on ([`resume`]). Negative control: a
/// record with no home finds its process in `~/.claude`.
#[cfg(unix)]
#[test]
fn a_relaunch_in_flight_finds_its_process_under_the_home_its_line_carried() {
    let dir = scratch("adopt-home");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("spawn");
    let (pid, me) = (agent.id(), std::process::id());
    wait_exec(pid);
    // One step over a record in flight whose line carried `home`, the
    // relaunched process registered where its Claude Code files it.
    let adopt = |home: Option<&Path>| {
        let claude_dir = home.map_or_else(|| opts.home.join(".claude"), Path::to_path_buf);
        let _ = std::fs::remove_dir_all(dir.join("work home"));
        let _ = std::fs::remove_dir_all(opts.home.join(".claude"));
        let sf = register_in(&claude_dir, pid, SESSION);
        let mut st = St {
            phase: Phase::Relaunched { at_s: now_s() },
            pid: dead_pid(),
            shell: me,
            tab: TAB.to_string(),
            to: "9.9.9".to_string(),
            source: "same".to_string(),
            cause: CAUSE_HOST.to_string(),
            home: home.map(|h| h.to_string_lossy().into_owned()),
            ..St::default()
        };
        save(&opts, SESSION, &st);
        // (The stand-in instance serves one connection at a time.)
        let mut c = connect(&opts, TAB).expect("control connection");
        await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live)
    };
    let r = adopt(Some(&dir.join("work home/.claude")));
    assert_eq!((r.step.as_str(), r.pid), ("adopted", pid), "{r:?}");
    // Its loop's idle point carries it on ([`resume`]): found again under the same home,
    // the carry goes on to the tab's own claim, which the stand-in (on no PTY of the tab's)
    // does not hold.
    let carried = resume(&opts);
    assert_eq!(
        (carried.step.as_str(), carried.pid),
        ("wait:tab-ownership-changed", pid),
        "{carried:?}"
    );
    let r = adopt(None);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", pid), "{r:?}");
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relaunch handed back (`hand_back`): the step ends at `adopted`, the record still in
/// flight and owed to its tab, and [`resume`] at the loop's idle point carries it on; a
/// typed continuation's model is owed until read. Negative controls: without `hand_back`
/// the find goes on to the carry-on; another tab owes nothing.
#[cfg(unix)]
#[test]
fn a_relaunch_handed_to_its_loop_ends_at_adoption_and_is_owed_to_its_tab() {
    let dir = scratch("hand-back");
    let (sock, _asked) = instance(&dir);
    let mut opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut agent = parked().spawn().expect("spawn");
    let (pid, me, dead) = (agent.id(), std::process::id(), dead_pid());
    wait_exec(pid);
    register(&opts.home, pid, SESSION);
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead,
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: super::super::relaunch::CAUSE_EXIT.to_string(),
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let mut c = connect(&opts, TAB).expect("control connection");
    let sf = register(&opts.home, pid, SESSION);
    let r = await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", pid));
    assert!(
        matches!(st.phase, Phase::Relaunched { .. }),
        "still in flight"
    );
    assert!(owed(&opts), "owed to its tab");
    let other = Opts {
        only_sid: Some("s-0ther".to_string()),
        ..opts.clone()
    };
    assert!(!owed(&other), "another tab owes nothing");
    // Negative control: no loop takes it — the same find goes on to the
    // carry-on in the same step (here stopped at the tab's claim).
    opts.hand_back = false;
    let sf = register(&opts.home, pid, SESSION);
    let carried = await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live);
    assert_ne!(carried.step, "adopted", "{carried:?}");
    // (The stand-in instance serves one connection at a time.)
    drop(c);
    // Where the loop parks, `resume` carries the record on the same way.
    st.phase = Phase::Relaunched { at_s: now_s() };
    save(&opts, SESSION, &st);
    assert_eq!(resume(&opts).step, carried.step);
    let _ = agent.kill();
    let _ = agent.wait();
    // A typed continuation's model, owed until read (no mark: at once).
    let confirming = St {
        phase: Phase::Done,
        confirm_by: now_s() + 60,
        resumed_on: "9.9.9".to_string(),
        resumed_pid: pid,
        ..st.clone()
    };
    save(&opts, SESSION, &confirming);
    assert!(owed(&opts), "its model is owed");
    let r = resume(&opts);
    assert_eq!(r.step, "done:model-unconfirmed", "{r:?}");
    assert!(!owed(&opts), "read: nothing more");
    let _ = std::fs::remove_dir_all(&dir);
}

/// One move, one `adopted` row: an adopted restart stays in flight until its loop's `resume`,
/// and a step that finds it meanwhile reads the record from disk and says nothing again.
/// Negative controls: a new move's relaunched process writes its own row; a move owing a
/// continuation says its supervisor types it.
#[cfg(unix)]
#[test]
fn an_adoption_is_said_once_in_the_words_of_its_move() {
    let dir = scratch("adopted-once");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut first = parked().spawn().expect("spawn");
    let mut second = parked().spawn().expect("spawn");
    let (a, b, me) = (first.id(), second.id(), std::process::id());
    wait_exec(a);
    wait_exec(b);
    let mut c = connect(&opts, TAB).expect("control connection");
    let sessions = opts.home.join(".claude/sessions");
    // One step over the record on disk, finding `pid` as the relaunch.
    let mut adopt = |pid: u32| {
        let _ = std::fs::remove_dir_all(&sessions);
        let sf = register(&opts.home, pid, SESSION);
        let mut st = load(&opts, SESSION).expect("state");
        await_new(&opts, blank(&sf), &mut st, &mut c, SESSION, &Live)
    };
    let quiet_move = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead_pid(),
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        quiet: true,
        ..St::default()
    };
    save(&opts, SESSION, &quiet_move);
    let r = adopt(a);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", a), "{r:?}");
    // Kept apart from the typed continuation's pid, which says it was typed
    // (host_live's proof that the continuation reached the parked loop).
    let adopted = load(&opts, SESSION).expect("state");
    assert_eq!((adopted.relaunched_pid, adopted.resumed_pid), (a, 0));
    // The host's carry of the same move finds the same process.
    let again = adopt(a);
    assert_eq!(
        (again.step.as_str(), again.pid),
        ("adopted", a),
        "{again:?}"
    );
    let rows = ledger_details(&opts, "adopted");
    assert_eq!(rows.len(), 1, "one move, one row: {rows:?}");
    assert!(
        rows[0].contains("resumed while idle, nothing typed")
            && !rows[0].contains("types the continuation"),
        "a quiet move promises nothing: {rows:?}"
    );
    // Negative control: the next move — an ask-flow round, relaunched
    // process `b` — says its own row, and owes the continuation.
    save(
        &opts,
        SESSION,
        &St {
            phase: Phase::Relaunched { at_s: now_s() },
            pid: a,
            quiet: false,
            ..load(&opts, SESSION).expect("state")
        },
    );
    let r = adopt(b);
    assert_eq!((r.step.as_str(), r.pid), ("adopted", b), "{r:?}");
    let rows = ledger_details(&opts, "adopted");
    assert_eq!(rows.len(), 2, "a new move, its own row: {rows:?}");
    assert!(
        rows[1].contains("its supervisor types the continuation"),
        "{rows:?}"
    );
    drop(c);
    for p in [&mut first, &mut second] {
        let _ = p.kill();
        let _ = p.wait();
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The orphan pass leaves a restart whose relaunched process registered after the pass
/// read its list: it is `visit`'s or its loop's `resume`, so nothing is dialled, said or
/// written. Negative control: once the relaunch holds nothing, the orphan pass carries it.
#[cfg(unix)]
#[test]
fn the_orphan_pass_leaves_a_restart_relaunched_since_its_list_was_read() {
    let dir = scratch("orphan-fresh-read");
    let opts = Opts {
        hand_back: true,
        ..drive(&dir)
    };
    let (old, me) = (dead_pid(), std::process::id());
    // The pass's list, read while the old agent held the conversation.
    let listed = upgrade::parse_session_file(&format!(
        r#"{{"pid":{old},"sessionId":"{SESSION}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"gone","kind":"interactive","entrypoint":"cli"}}"#
    ))
    .expect("a session file");
    let record = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: old,
        shell: me,
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        quiet: true,
        ..St::default()
    };
    save(&opts, SESSION, &record);
    let saved = load(&opts, SESSION).expect("the record");
    let mut new = parked().spawn().expect("spawn");
    wait_exec(new.id());
    register(&opts.home, new.id(), SESSION);
    let steps = || {
        orphans(&opts, std::slice::from_ref(&listed), None)
            .into_iter()
            .map(|r| r.step)
            .collect::<Vec<_>>()
    };
    assert_eq!(steps(), Vec::<String>::new(), "the relaunch holds it");
    assert_eq!(ledger_lines(&opts), 0, "nothing said");
    assert_eq!(load(&opts, SESSION), Some(saved), "nothing written");
    // Negative control: the relaunched process gone, and its file with it.
    let _ = new.kill();
    let _ = new.wait();
    std::fs::remove_file(
        opts.home
            .join(format!(".claude/sessions/{}.json", new.id())),
    )
    .expect("remove the relaunch's session file");
    assert_eq!(steps(), ["wait:no-socket"], "an orphan is carried on");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh start carries every launch flag and no `--resume` — the one pair
/// the resume adds — and a resume carries the pair. Negative control: a
/// launch that cannot be carried is refused either way.
#[test]
fn a_fresh_start_is_the_same_launch_without_the_resume() {
    let argv: Vec<String> = ["claude", "--model", "opus", "fix the bug"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let exe = Path::new("/opt/claude");
    let resumed = line_for(
        Dialect::Bash,
        None,
        None,
        exe,
        &argv,
        Some(SESSION),
        None,
        None,
        &[],
    )
    .unwrap_or_else(|_| panic!("a line"));
    assert_eq!(
        resumed,
        format!(
            " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' '--model' 'opus' \
             '--resume' '{SESSION}'"
        )
    );
    let fresh = line_for(Dialect::Bash, None, None, exe, &argv, None, None, None, &[])
        .unwrap_or_else(|_| panic!("a line"));
    assert_eq!(
        fresh,
        " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' '--model' 'opus'"
    );
    let print: Vec<String> = ["claude", "-p", "hi"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert!(
        line_for(
            Dialect::Bash,
            None,
            None,
            exe,
            &print,
            None,
            None,
            None,
            &[]
        )
        .is_err()
    );
}

/// The survey export never refuses a launch: a relaunch line that fits the kernel's bound
/// only without `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 ` is relaunched without it.
/// Negative control: a line over the bound either way is refused; one that fits carries it.
#[test]
fn the_survey_export_never_refuses_a_launch() {
    let exe = Path::new("/opt/claude");
    let line = |pad: usize| {
        let argv: Vec<String> = ["claude", "--add-dir"]
            .iter()
            .map(|s| (*s).to_string())
            .chain([format!("/{}", "d".repeat(pad))])
            .collect();
        line_for(
            Dialect::Bash,
            None,
            None,
            exe,
            &argv,
            Some(SESSION),
            None,
            None,
            &[],
        )
    };
    // The bare line's length grows one byte per pad byte.
    let bare = |pad: usize| {
        let flags = upgrade::rewrite_argv(
            &[
                "claude".to_string(),
                "--add-dir".to_string(),
                format!("/{}", "d".repeat(pad)),
            ],
            SESSION,
        )
        .expect("flags");
        upgrade::relaunch_line(Dialect::Bash, None, None, exe, &flags)
            .expect("a line")
            .len()
    };
    let small = bare(1);
    let fits = upgrade::MAX_LINE - 5 - small + 1;
    assert_eq!(bare(fits), upgrade::MAX_LINE - 5);
    let carried = line(fits).unwrap_or_else(|_| panic!("relaunched without the switch"));
    assert!(!carried.contains("FEEDBACK_SURVEY"), "{}", carried.len());
    let over = upgrade::MAX_LINE - small + 2;
    assert!(line(over).is_err(), "over the bound either way");
    let roomy = line(1).unwrap_or_else(|_| panic!("a short line"));
    assert!(roomy.contains("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1"));
}

// ---------------------------------------------------------------- the model

/// An assistant row naming `model`, as Claude Code writes one, saying `text`.
#[cfg(unix)]
fn turn_by(model: &str, text: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"{model}","role":"assistant","content":[{{"type":"text","text":"{text}"}}]}},"version":"2.1.282"}}"#
    )
}

/// A user row saying `text`.
#[cfg(unix)]
fn user_row(text: &str) -> String {
    format!(r#"{{"type":"user","message":{{"role":"user","content":"{text}"}}}}"#)
}

/// [`user_row`], recorded `at` (Claude Code's `timestamp`).
#[cfg(unix)]
fn user_row_at(text: &str, at: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"{text}"}}}}"#
    )
}

/// A restart relaunched from 2.1.281 onto 2.1.282 over a stand-in agent in [`TAB`], at the
/// moment its new process holds the conversation: the state as [`relaunch`] leaves it, with the
/// transcript's model and mark as the sweep read them ([`model_and_mark`]) when it was `before`.
/// Dropped, it stops the agent and removes its directory.
#[cfg(unix)]
struct Rig {
    dir: ScratchDir,
    opts: Opts,
    asked: crate::harness::FakeInstanceLog,
    agent: aterm_uds::spawnfd::Child,
    sf: SessionFile,
    path: PathBuf,
    st: St,
}

#[cfg(unix)]
impl Rig {
    fn new(name: &str, before: &[String]) -> Rig {
        Rig::with(name, before, Answers::default())
    }

    /// [`Rig::new`], the tab's instance answering as `answers` says.
    fn with(name: &str, before: &[String], answers: Answers) -> Rig {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let agent = parked().spawn().expect("agent");
        wait_exec(agent.id());
        let file = opts
            .home
            .join(format!(".claude/sessions/{}.json", agent.id()));
        register(&opts.home, agent.id(), SESSION);
        let relaunched = std::fs::read_to_string(&file)
            .expect("session file")
            .replace(r#""version":"1.0.0""#, r#""version":"2.1.282""#);
        std::fs::write(&file, relaunched).expect("relaunched version");
        let sf = session_file_of(&opts.home, agent.id()).expect("parses");
        let project = opts.home.join(".claude/projects/p");
        std::fs::create_dir_all(&project).expect("project");
        let path = project.join(format!("{SESSION}.jsonl"));
        std::fs::write(&path, before.join("\n") + "\n").expect("transcript");
        let (model_before, mark) = model_and_mark(Some(&tail_to_end(&path, TAIL_BYTES)));
        let st = St {
            phase: Phase::Relaunched { at_s: now_s() },
            from: "2.1.281".to_string(),
            to: "2.1.282".to_string(),
            source: "managed".to_string(),
            tab: TAB.to_string(),
            model_before,
            mark,
            ..St::default()
        };
        save(&opts, SESSION, &st);
        Rig {
            dir,
            opts,
            asked,
            agent,
            sf,
            path,
            st,
        }
    }

    /// The transcript gains `rows`.
    fn append(&self, rows: &[String]) {
        let mut more = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .expect("append");
        for row in rows {
            writeln!(more, "{row}").expect("row");
        }
    }

    /// Carry `st` on, the resumed answer waited for at most `wait`.
    fn carry_on_from(&self, st: &mut St, wait: Duration) -> Report {
        let mut c = connect(&self.opts, TAB).expect("control connection");
        carry_on_with_tab_probe(
            &self.opts,
            blank(&self.sf),
            st,
            &mut c,
            SESSION,
            &self.sf,
            wait,
            |_, _, _| true,
            None,
        )
    }

    /// Every turn typed into the tab.
    fn typed(&self) -> Vec<String> {
        self.asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
            .unwrap_or_default()
    }

    /// The details of the ledger's rows whose step is `step`.
    fn details(&self, step: &str) -> Vec<String> {
        std::fs::read_to_string(state_dir(&self.opts).join("ledger.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
            .filter(|v| v.get("step").and_then(Value::as_str) == Some(step))
            .filter_map(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
            .collect()
    }
}

#[cfg(unix)]
impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.agent.kill();
        let _ = self.agent.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What one carried-on restart left: the report, every line typed into the
/// tab, the ledger's detail for the report's step and the state.
#[cfg(unix)]
struct Carried {
    report: Report,
    typed: Vec<String>,
    detail: String,
    st: St,
}

/// A restart carried on end to end ([`Rig`]): its transcript is `before` when
/// the restart records what the agent ran, and gains `after` since, and the
/// resumed answer is waited for at most `wait`.
#[cfg(unix)]
fn carried_on(name: &str, before: &[String], after: &[String], wait: Duration) -> Carried {
    let rig = Rig::new(name, before);
    rig.append(after);
    let mut st = rig.st.clone();
    // The step that types says only that it typed; the model its answer
    // names is the next step's to read ([`confirmations`]).
    let typed = rig.carry_on_from(&mut st, wait);
    assert_eq!(typed.step, "continued", "{typed:?}");
    let mut said = confirmations(&rig.opts);
    assert_eq!(said.len(), 1, "{said:?}");
    let report = said.remove(0);
    let detail = rig.details(&report.step);
    assert_eq!(detail.len(), 1, "the outcome is recorded once: {detail:?}");
    Carried {
        typed: rig.typed(),
        report,
        detail: detail[0].clone(),
        st: load(&rig.opts, SESSION).expect("state"),
    }
}

/// The continuation names the model the READY answer ran; the step after reads the model
/// after from the resumed session's first answer past the mark (Claude's `<synthetic>`
/// row skipped). A change is words only: the step is `done`.
#[cfg(unix)]
#[test]
fn a_restart_says_the_model_before_and_confirms_the_one_after() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5", "Earlier work."),
        user_row("[aterm harness] Claude Code 2.1.282 (managed) is installed"),
        turn_by("claude-opus-5", &format!("Saved.\\n{marker}")),
    ];
    let after = [
        user_row("[aterm harness] Upgraded: carry on"),
        r#"{"type":"assistant","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"No response requested."}]}}"#.to_string(),
        turn_by("claude-opus-5-5", "Resumed."),
        turn_by("claude-fable-5-1", "A later turn."),
    ];
    let changed = carried_on("model-changed", &before, &after, MODEL_WAIT);
    assert_eq!(changed.report.step, "done", "{:?}", changed.report);
    assert_eq!(changed.st.phase, Phase::Done);
    assert_eq!(
        changed.typed.len(),
        1,
        "one continuation: {:?}",
        changed.typed
    );
    assert!(
        changed.typed[0].contains(
            "restarted on Claude Code 2.1.282 (from 2.1.281); it ran claude-opus-5 before the \
             restart and was resumed."
        ),
        "{:?}",
        changed.typed
    );
    assert_eq!(
        changed.detail,
        "claude restarted on 2.1.282 · model claude-opus-5 -> claude-opus-5-5 (a session \
         launched without --model takes the current default; /model changes it)"
    );
    // The same model on both sides: the model, and nothing about a change.
    let before_same = [before[0].clone(), turn_by("claude-opus-5-5", marker)];
    let same = carried_on(
        "model-same",
        &before_same,
        &[turn_by("claude-opus-5-5", "Resumed.")],
        MODEL_WAIT,
    );
    assert_eq!(same.report.step, "done");
    assert!(same.typed[0].contains("it ran claude-opus-5-5 before the restart"));
    assert_eq!(
        same.detail,
        "claude restarted on 2.1.282 · model claude-opus-5-5"
    );
}

/// A relaunch after aterm itself ended (`CAUSE_HOST`) types the relaunch's own words, not
/// the upgrade's `Upgraded: …`, which would be false for an agent nobody upgraded.
/// Negative control: the upgrade's own record still types the upgrade's words.
#[cfg(unix)]
#[test]
fn a_relaunch_after_aterm_ended_types_why_it_was_relaunched() {
    let before = [user_row("work"), turn_by("claude-opus-5-5", "Working.")];
    let after = [turn_by("claude-opus-5-5", "Resumed.")];
    let typed = |cause: &str, name: &str| {
        let rig = Rig::new(name, &before);
        rig.append(&after);
        let mut st = St {
            cause: cause.to_string(),
            ..rig.st.clone()
        };
        let r = rig.carry_on_from(&mut st, MODEL_WAIT);
        assert_eq!(r.step, "continued", "{r:?}");
        rig.typed()
    };
    let host = typed(CAUSE_HOST, "host-ended-words");
    assert_eq!(host.len(), 1, "{host:?}");
    assert!(
        host[0].contains("Relaunched: aterm ended while this session ran"),
        "{host:?}"
    );
    assert!(!host[0].contains("Upgraded"), "{host:?}");
    let upgrade = typed("", "upgrade-words");
    assert_eq!(upgrade.len(), 1, "{upgrade:?}");
    assert!(upgrade[0].contains("Upgraded: "), "{upgrade:?}");
}

/// The relaunch's production waits (15 s for a shell's prompt, 5 s of a shell holding its
/// tab before the agent reads as ended, a look a second) cut to a fraction of a second for
/// this thread until dropped ([`WaitsInTest`]). No assertion bounds a step's time from
/// above.
#[cfg(unix)]
struct QuickWaits;

#[cfg(unix)]
impl QuickWaits {
    /// How long the stand-in shell must hold its tab before the agent reads as ended.
    const ENDED: Duration = Duration::from_millis(200);

    fn new() -> Self {
        WAITS_IN_TEST.with(|w| {
            w.set(Some(WaitsInTest {
                prompt_back: Duration::from_millis(300),
                ended_at_once: Self::ENDED,
                shell_look: Duration::from_millis(50),
            }));
        });
        Self
    }
}

#[cfg(unix)]
impl Drop for QuickWaits {
    fn drop(&mut self) {
        WAITS_IN_TEST.with(|w| w.set(None));
    }
}

/// A [`Kernel`] whose one scripted answer is whether the relaunch's shell has
/// its terminal back, look by look (the last answer repeating); nothing else
/// about any process can be read.
#[cfg(unix)]
struct ShellHolds {
    looks: &'static [Option<bool>],
    next: std::cell::Cell<usize>,
}

#[cfg(unix)]
impl ShellHolds {
    fn new(looks: &'static [Option<bool>]) -> Self {
        Self {
            looks,
            next: std::cell::Cell::new(0),
        }
    }
}

#[cfg(unix)]
impl Kernel for ShellHolds {
    fn job(&self, _: u32) -> Option<(Job, u32)> {
        None
    }

    fn parent(&self, _: u32) -> Option<u32> {
        None
    }

    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        None
    }

    fn shell_holds_tab(&self, _: u32) -> Option<bool> {
        let at = self.next.get();
        self.next.set(at + 1);
        self.looks
            .get(at)
            .or_else(|| self.looks.last())
            .copied()
            .flatten()
    }
}

/// The shell held its terminal at every look: what it does before the line's
/// command starts (its hooks), or a command that came and went between looks.
#[cfg(unix)]
const HELD: &[Option<bool>] = &[Some(true)];

/// The line's command held the terminal at the first look (the agent ran),
/// and the shell has it back at every look since.
#[cfg(unix)]
const RAN: &[Option<bool>] = &[Some(false), Some(true)];

/// What the looks of one [`held_step`] read: the shell's hold on its terminal
/// look by look ([`ShellHolds`]), and, where its integration marks command
/// blocks, the block the line was typed into ([`St::line_block`]) with the
/// tab's `blocks 1 --json` reply.
#[cfg(unix)]
#[derive(Clone, Copy)]
struct Looks {
    holds: &'static [Option<bool>],
    block: Option<(u64, &'static str)>,
}

#[cfg(unix)]
impl Looks {
    /// Only the kernel's witness: no block was read.
    const fn holds(holds: &'static [Option<bool>]) -> Self {
        Self { holds, block: None }
    }
}

/// The line was typed into block 3, which is still the prompt it was typed
/// at: the shell has not run the line's command yet (its hooks).
#[cfg(unix)]
const BLOCK_NOT_RUN: &str = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":5,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;

/// Block 3 is still running its command.
#[cfg(unix)]
const BLOCK_RUNS: &str = r#"OK {"blocks":[{"id":3,"state":"executing","exit":null,"prompt":10,"cmd":10,"cmdcol":5,"out":11,"end":null,"cwd":"","cmdline":"claude --resume x"}]}"#;

/// Block 3's command ended (its `133;D`).
#[cfg(unix)]
const BLOCK_ENDED: &str = r#"OK {"blocks":[{"id":3,"state":"complete","exit":1,"prompt":10,"cmd":10,"cmdcol":5,"out":11,"end":null,"cwd":"","cmdline":"claude --resume x"}]}"#;

/// The shell drew its next prompt: block 4 began after block 3.
#[cfg(unix)]
const BLOCK_NEXT: &str = r#"OK {"blocks":[{"id":4,"state":"entering","exit":null,"prompt":12,"cmd":12,"cmdcol":5,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;

/// A relaunched agent that ended as it started (Claude's `No conversation found`, a crash
/// at start: the agent took the terminal, its shell has it back at once) stops in
/// seconds: `failed:no-resume`, one ledger line. Negative controls:
/// `relaunch::tests::a_shell_that_has_the_tab_back_reads_as_ended_only_once_its_line_ran`.
#[cfg(unix)]
#[test]
fn a_relaunch_that_ended_as_it_started_is_said_at_once() {
    let _quick = QuickWaits::new();
    let dir = scratch("ended-at-once");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead_pid(),
        shell: std::process::id(),
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: CAUSE_HOST.to_string(),
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let mut c = connect(&opts, TAB).expect("control connection");
    let r = Report {
        pid: st.pid,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "9.9.9".to_string(),
        to: "9.9.9(same)".to_string(),
        step: String::new(),
    };
    let started = Instant::now();
    let r = await_new(&opts, r, &mut st, &mut c, SESSION, &ShellHolds::new(RAN));
    let took = started.elapsed();
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert!(
        took >= QuickWaits::ENDED,
        "not before the shell held it a while: {took:?}"
    );
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(ledger_lines(&opts), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One [`await_new`] step over a relaunch of `cause` in flight since just
/// now, its looks reading `looks` ([`Looks`]), and the wait for the
/// relaunched process cut to `wait` (a step that must not end early is
/// waited out): the step, the record after it, its ledger rows.
#[cfg(unix)]
fn held_step(name: &str, cause: &str, looks: Looks, wait: Duration) -> (Report, St, usize) {
    let (r, st, rows, _) = held_step_shown(name, cause, looks, wait);
    (r, st, rows)
}

/// [`held_step`], and the owner's rows of the records after it ([`rows`]),
/// the step's record saved as its callers save it.
#[cfg(unix)]
fn held_step_shown(
    name: &str,
    cause: &str,
    looks: Looks,
    wait: Duration,
) -> (Report, St, usize, Vec<Row>) {
    let dir = scratch(name);
    let (sock, _asked) = instance(&dir);
    if let Some((_, blocks)) = looks.block {
        std::fs::write(dir.join("t.blocks"), blocks).expect("blocks");
    }
    let opts = Opts {
        sock: Some(sock),
        only_sid: Some(TAB.to_string()),
        hand_back: true,
        background: false,
        ..drive(&dir)
    };
    let mut st = St {
        phase: Phase::Relaunched { at_s: now_s() },
        pid: dead_pid(),
        shell: std::process::id(),
        tab: TAB.to_string(),
        to: "9.9.9".to_string(),
        source: "same".to_string(),
        cause: cause.to_string(),
        // Its agent seen gone before the line, as `relaunch` stamps it.
        exited_at: now_s(),
        line_block: looks.block.map(|(block, _)| block),
        ..St::default()
    };
    save(&opts, SESSION, &st);
    let mut c = connect(&opts, TAB).expect("control connection");
    let r = Report {
        pid: st.pid,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "9.9.9".to_string(),
        to: "9.9.9(same)".to_string(),
        step: String::new(),
    };
    let _quick = QuickWaits::new();
    HELD_WAIT_IN_TEST.with(|w| w.set(Some(wait)));
    let r = await_new(
        &opts,
        r,
        &mut st,
        &mut c,
        SESSION,
        &ShellHolds::new(looks.holds),
    );
    HELD_WAIT_IN_TEST.with(|w| w.set(None));
    let rows = ledger_lines(&opts);
    // Saved as every caller of the step saves it, then read as the owner's.
    save(&opts, SESSION, &st);
    let shown = super::rows(&opts);
    let _ = std::fs::remove_dir_all(&dir);
    (r, st, rows, shown)
}

/// A wait long enough that a shell holding the tab throughout would have been read as ended
/// ([`QuickWaits::ENDED`] and several looks more): the negative controls wait it out. A
/// positive control, which ends at once, passes [`HANG_DETECTOR`] instead.
#[cfg(unix)]
const PAST_ENDED: Duration = Duration::from_millis(1_500);

/// The wait of a step that must end early: only a hang waits it out.
#[cfg(unix)]
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// Negative controls of the early verdict, through the step itself: a restored tab's
/// relaunched agent still starting (something other than its shell holds the tab), or
/// whose shell cannot be read, is waited on (`wait:resume`, nothing ledgered).
#[cfg(unix)]
#[test]
fn a_restored_relaunch_still_starting_or_unread_is_waited_on_not_failed() {
    // Each case waits PAST_ENDED out on a step of its own: side by side.
    side_by_side(
        [
            (&[Some(false)] as &'static [Option<bool>], "held-starting"),
            (&[None], "held-unread"),
        ]
        .into_iter()
        .map(|(holds, name)| -> Box<dyn FnOnce() + Send> {
            Box::new(move || {
                let (r, st, rows) = held_step(name, CAUSE_HOST, Looks::holds(holds), PAST_ENDED);
                assert_eq!(r.step, "wait:resume", "{holds:?}: {r:?}");
                assert!(
                    matches!(st.phase, Phase::Relaunched { .. }),
                    "{holds:?}: {st:?}"
                );
                assert_eq!(rows, 0, "{holds:?}");
            })
        })
        .collect(),
    );
}

/// The early verdict is never the live relaunch on exit's ([`CAUSE_EXIT`]): its loop retries
/// `failed:no-resume` by retyping the line, so an early verdict would retype a refusing
/// agent within seconds. Negative control: a restored tab's step is said ended.
#[cfg(unix)]
#[test]
fn a_live_relaunch_on_exit_keeps_its_wait_when_its_shell_has_the_tab() {
    let (r, st, rows) = held_step("held-live", CAUSE_EXIT, Looks::holds(RAN), PAST_ENDED);
    assert_eq!(r.step, "wait:resume", "{r:?}");
    assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
    assert_eq!(rows, 0);
    let (r, st, rows) = held_step(
        "held-restored",
        CAUSE_HOST,
        Looks::holds(RAN),
        HANG_DETECTOR,
    );
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(rows, 1);
}

/// A shell that holds its tab is no proof the agent ended: it can run its own work (a
/// `preexec` with `$(…)`, direnv on the line's `cd`) between reading the line and starting
/// it. With no witness that the line ran, the step waits `wait:resume`, the record in
/// flight, for the upgrade's move and an account move alike.
#[cfg(unix)]
#[test]
fn a_relaunch_whose_shell_has_not_run_its_line_yet_is_waited_on() {
    side_by_side(
        [
            ("hook-upgrade", "", Looks::holds(HELD)),
            ("hook-account", "account:work-2", Looks::holds(HELD)),
            (
                "hook-marked",
                "",
                Looks {
                    holds: HELD,
                    block: Some((3, BLOCK_NOT_RUN)),
                },
            ),
        ]
        .into_iter()
        .map(|(name, cause, looks)| -> Box<dyn FnOnce() + Send> {
            Box::new(move || {
                let (r, st, rows) = held_step(name, cause, looks, PAST_ENDED);
                assert_eq!(r.step, "wait:resume", "{name}: {r:?}");
                assert!(
                    matches!(st.phase, Phase::Relaunched { .. }),
                    "{name}: {st:?}"
                );
                assert_eq!(rows, 0, "{name}");
            })
        })
        .collect(),
    );
}

/// Where the shell's integration marks the line's block ([`St::line_block`]), that block
/// over (`133;D`, or the next prompt's begun) is the line's command ended, though no look
/// caught the agent: `failed:no-resume`. Negative control: the block still running waits,
/// though the kernel saw the agent come and go (the shell's word outranks it).
#[cfg(unix)]
#[test]
fn the_shells_own_block_says_whether_the_line_ran() {
    side_by_side(
        [
            ("marked-ended", BLOCK_ENDED, HELD, true),
            ("marked-next", BLOCK_NEXT, HELD, true),
            ("marked-runs", BLOCK_RUNS, RAN, false),
        ]
        .into_iter()
        .map(|(name, blocks, holds, ended)| -> Box<dyn FnOnce() + Send> {
            Box::new(move || {
                let looks = Looks {
                    holds,
                    block: Some((3, blocks)),
                };
                let wait = if ended { HANG_DETECTOR } else { PAST_ENDED };
                let (r, st, rows) = held_step(name, "", looks, wait);
                if ended {
                    assert_eq!(r.step, "failed:no-resume", "{name}: {r:?}");
                    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()), "{name}");
                    assert_eq!(rows, 1, "{name}");
                } else {
                    assert_eq!(r.step, "wait:resume", "{name}: {r:?}");
                    assert!(
                        matches!(st.phase, Phase::Relaunched { .. }),
                        "{name}: {st:?}"
                    );
                    assert_eq!(rows, 0, "{name}");
                }
            })
        })
        .collect(),
    );
}

/// The tab's newest block is the prompt the shell waits at (block 3), its
/// input start not marked yet (`133;B`): what a relaunch line is typed into.
#[cfg(unix)]
const BLOCK_AT_PROMPT: &str = r#"OK {"blocks":[{"id":3,"state":"prompt","exit":null,"prompt":10,"cmd":null,"cmdcol":null,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;

/// A restart stamps the block its line is typed into ([`St::line_block`]) and judges the
/// line by it: at a zsh that holds its terminal throughout, the next prompt's block begun
/// after the line is `failed:no-resume` at once. Negative control: block 3 still the
/// newest waits (`wait:resume`).
#[cfg(unix)]
#[test]
fn a_restarts_line_is_judged_by_the_block_it_was_typed_into() {
    let case = |name: &str, next_prompt: bool| {
        let _quick = QuickWaits::new();
        let dir = scratch(name);
        let mut shell = tty_zsh();
        let who = format!(
            "OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={}",
            shell.id()
        );
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                blocks: BLOCK_AT_PROMPT,
                who: Some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        let mut agent = parked_bare().spawn().expect("agent");
        let pid = agent.id();
        wait_exec(pid);
        register(&opts.home, pid, SESSION);
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        write_transcript(
            &opts.home,
            &[answer_at("claude-opus-5", "2099-01-01T00:00:00Z")],
        );
        let snap = Snapshot {
            stored_login: false,
            tab: TAB.to_string(),
            pid,
            start: kernel_start(pid).expect("lstart"),
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: Some("1.0.0".to_string()),
            codex: None,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        // Reaped as the stand-in's signal ends it.
        let (tx, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(agent.wait());
        });
        // The shell's next prompt, drawn once the line is typed.
        let next = next_prompt.then(|| {
            let (asked, blocks) = (asked.clone(), dir.join("t.blocks"));
            std::thread::spawn(move || {
                let typed = aterm_spec::wait::within(
                    aterm_spec::wait::MUST_HAPPEN,
                    Duration::from_millis(5),
                    || {
                        asked
                            .lock()
                            .ok()?
                            .iter()
                            .any(|l| l.contains(" turn "))
                            .then_some(())
                    },
                );
                if typed.is_some() {
                    std::fs::write(blocks, BLOCK_NEXT).expect("the next prompt's block");
                }
            })
        });
        let wait = if next_prompt {
            HANG_DETECTOR
        } else {
            PAST_ENDED
        };
        HELD_WAIT_IN_TEST.with(|w| w.set(Some(wait)));
        let restart = || {
            crate::harness::relaunch::restart_with(
                &opts,
                &Restart::Memory,
                Ok(snap.clone()),
                &Script::new(shell.id(), usize::MAX, None),
            )
        };
        let mut r = restart();
        // The stand-in is this test's child, alive to `kill(0)` until reaped: carry the
        // relaunch on, as the host's next idle point would, once it is.
        assert!(
            ended.recv_timeout(HANG_DETECTOR).is_ok(),
            "{name}: the agent ended"
        );
        if turns(&asked) == 0 {
            r = restart();
        }
        HELD_WAIT_IN_TEST.with(|w| w.set(None));
        let st = load(&opts, SESSION).expect("the restart's record");
        let typed = turns(&asked);
        if let Some(next) = next {
            let _ = next.join();
        }
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (r, st, typed)
    };
    let case = &case;
    side_by_side(vec![
        Box::new(move || {
            let (r, st, typed) = case("block-next-prompt", true);
            assert_eq!(typed, 1, "{r:?}");
            assert_eq!(st.line_block, Some(3), "{st:?}");
            assert_eq!(r.step, "failed:no-resume", "{r:?}");
            assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
        }),
        Box::new(move || {
            let (r, st, typed) = case("block-still-prompt", false);
            assert_eq!(typed, 1, "{r:?}");
            assert_eq!(st.line_block, Some(3), "{st:?}");
            assert_eq!(r.step, "wait:resume", "{r:?}");
            assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
        }),
    ]);
}

/// An upgrade's relaunch that ended as it started stops at once (`failed:no-resume`) and
/// reads as `agent-down`, its remedy the conversation's resume (a fresh `claude` for a
/// fresh restart): the harness's own restarts type their line once, so they take the early
/// read a restored tab's relaunch takes. Negative control:
/// [`a_live_relaunch_on_exit_keeps_its_wait_when_its_shell_has_the_tab`].
#[cfg(unix)]
#[test]
fn an_upgrade_relaunch_that_ended_as_it_started_is_an_agent_down_at_once() {
    use super::super::relaunch::CAUSE_UPGRADE_FRESH;
    side_by_side(
        [
            ("upgrade-ended", "", format!("claude --resume {SESSION}")),
            ("fresh-ended", CAUSE_UPGRADE_FRESH, "claude".to_string()),
        ]
        .into_iter()
        .map(|(name, cause, command)| -> Box<dyn FnOnce() + Send> {
            Box::new(move || {
                let (r, st, rows, shown) =
                    held_step_shown(name, cause, Looks::holds(RAN), HANG_DETECTOR);
                assert_eq!(r.step, "failed:no-resume", "{cause:?}: {r:?}");
                assert_eq!(
                    st.phase,
                    Phase::Failed("no-resume".to_string()),
                    "{cause:?}"
                );
                assert_eq!(rows, 1, "{cause:?}");
                let row = shown
                    .iter()
                    .find(|row| row.session == SESSION)
                    .expect("its row");
                match row.attention(now_s()) {
                    Attention::Interrupt(need) => {
                        assert_eq!(need.kind, "agent-down", "{cause:?}");
                        assert_eq!(need.command.as_deref(), Some(command.as_str()), "{cause:?}");
                    }
                    other => panic!("{cause:?}: {other:?}"),
                }
            })
        })
        .collect(),
    );
}

/// The kernel's own answer ([`Kernel::shell_holds_tab`]'s default, [`Live`]'s) on real
/// processes: a shell that leads its session on a terminal as its foreground holds the tab; a
/// background job or a process in another's group does not; no shell recorded (pid 0) or a gone
/// one cannot be read.
#[cfg(unix)]
#[test]
fn the_kernel_reads_whether_a_shell_has_its_tab_back() {
    let mut zsh = tty_zsh();
    let mut own = parked().process_group(0).spawn().expect("spawn");
    let mut plain = parked().spawn().expect("spawn");
    wait_exec(own.id());
    wait_exec(plain.id());
    let got = [
        Live.shell_holds_tab(zsh.id()),
        Live.shell_holds_tab(own.id()),
        Live.shell_holds_tab(plain.id()),
    ];
    let _ = zsh.kill();
    let _ = zsh.wait();
    for c in [&mut own, &mut plain] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(got, [Some(true), Some(false), Some(false)]);
    assert_eq!(Live.shell_holds_tab(0), None);
    assert_eq!(Live.shell_holds_tab(dead_pid()), None);
}

/// A resumed session that has not answered by the first sweep past the bound is said
/// `done:model-unconfirmed`, once; the mark keeps the old process's turns from confirming
/// the new one's model.
#[cfg(unix)]
#[test]
fn a_resumed_session_that_has_not_answered_is_unconfirmed() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "ATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("model-unconfirmed", &before);
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued");
    assert!(rig.typed()[0].contains("it ran claude-opus-5-5 before the restart"));
    // Within the bound, a sweep reads again and says nothing.
    assert_eq!(confirmations(&rig.opts), []);
    assert!(load(&rig.opts, SESSION).is_some_and(|st| st.confirming()));
    // Past it, still no answer: unconfirmed, once.
    let late = St {
        confirm_by: now_s() - 1,
        ..load(&rig.opts, SESSION).expect("state")
    };
    save(&rig.opts, SESSION, &late);
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done:model-unconfirmed"]
    );
    assert!(said[0].is_act());
    assert_eq!(
        (said[0].pid, said[0].to.as_str()),
        (rig.sf.pid, "2.1.282(managed)")
    );
    assert_eq!(
        rig.details("done:model-unconfirmed"),
        [
            "claude restarted on 2.1.282 · model unconfirmed (it ran claude-opus-5-5 before the \
          restart)"
        ]
    );
    let saved = load(&rig.opts, SESSION).expect("state");
    assert_eq!(saved.phase, Phase::Done, "said once, never re-asked");
    assert!(!saved.confirming());
    assert_eq!(confirmations(&rig.opts), []);
    assert_eq!(rig.typed().len(), 1);
}

/// A restart an older aterm began took no mark: nothing past it can be told to
/// be the resumed session's, so the model is unconfirmed at the first read
/// after the continuation — never read from the start of the conversation,
/// and never left pending past it.
#[cfg(unix)]
#[test]
fn a_restart_without_a_mark_is_unconfirmed_at_once() {
    let before = [
        ASKED.to_string(),
        turn_by("claude-opus-5-5", "ATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("model-no-mark", &before);
    rig.append(&[turn_by("claude-opus-5-5", "Resumed.")]);
    let mut st = St {
        mark: 0,
        ..rig.st.clone()
    };
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued");
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done:model-unconfirmed"]
    );
    assert!(!load(&rig.opts, SESSION).expect("state").confirming());
    assert_eq!(rig.typed().len(), 1);
}

/// A restart records the model its READY answer ran and where that read ended, from the
/// tail that proved the answer, even when its last look then waits; Claude's own
/// `<synthetic>` row after the READY names no model.
#[cfg(unix)]
#[test]
fn a_restart_records_the_model_its_ready_answer_ran_and_where_that_read_ended() {
    let dir = scratch("model-before");
    let (sock, _asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    // An argv a resume carries (the filter alone: a positional, dropped).
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let path = project.join(format!("{SESSION}.jsonl"));
    let body = [
        ASKED.to_string(),
        turn_by("claude-fable-5-1", "Earlier."),
        turn_by("claude-opus-5-5", &format!("Saved.\\n{marker}")),
        r#"{"type":"assistant","message":{"model":"<synthetic>","content":[{"type":"text","text":"No response requested."}]}}"#.to_string(),
    ]
    .join("\n")
        + "\n";
    std::fs::write(&path, &body).expect("transcript");
    let shell = dead_pid();
    let t = vec![(shell, 1, "zsh".to_string())];
    let now = now_s();
    let st = St {
        phase: Phase::Announced {
            at_s: now - 60,
            asks: 1,
        },
        marker: marker.to_string(),
        tab: TAB.to_string(),
        notice_pid: sf.pid,
        notice_start: squash(&sf.proc_start),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 77,
        seq_since_s: now - 3600,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    // The launch named a model: the relaunch keeps it, and the restart says so.
    let mut args = atpkg::caller_shell::process_args(pid).expect("agent argv");
    args.argv
        .extend(["--model".to_string(), "claude-sonnet-5".to_string()]);
    // Foreground for the first three job reads — the visit's two and the
    // restart's own — then suspended: the restart's last look waits.
    let files = session_files(&opts.home);
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &t,
        &newer(),
        &Script::new(shell, 3, None),
        Some(&args),
        None,
    );
    let saved = load(&opts, SESSION).expect("state");
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(r.step, "wait:changed", "the restart was reached: {r:?}");
    // The restart's own last look said it, past the reducer's go: the host's
    // watch, whose dry read stops at the go, keeps it (`St::held_past_go`).
    assert!(saved.restart_look, "the restart's own word: {saved:?}");
    assert!(saved.held_past_go("would-restart"));
    assert!(!saved.held_past_go("would-announce"), "another act's read");
    assert_eq!(saved.model_before, "claude-opus-5-5");
    assert_eq!(saved.mark, body.len() as u64, "the read ended at the end");
    assert_eq!(saved.launch_model, "claude-sonnet-5", "the kept --model");
    assert!(
        matches!(saved.phase, Phase::Announced { .. }),
        "never signalled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The visit that types the continuation does not wait for the answer: a sweep's visits
/// run one after another under one lock, the orphan pass last, so every second blocked
/// ages a restart left exiting toward [`STALE_S`], past which it is never relaunched.
#[cfg(unix)]
#[test]
fn the_sweep_that_types_the_continuation_does_not_wait_for_the_answer() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("continued-no-wait", &before);
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let started = Instant::now();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    let blocked = started.elapsed();
    // A hang detector, not a latency budget (AGENTS.md): no answer is written,
    // so a visit that waited for one sits out the whole MODEL_WAIT, two minutes.
    assert!(
        blocked < HANG_DETECTOR,
        "the visit blocked {blocked:?} for an answer that had not come (STALE_S {STALE_S}s)"
    );
    assert_eq!(rig.typed().len(), 1);
    assert_eq!(r.step, "continued", "{r:?}");
    assert!(r.is_act(), "the continuation typed is said");
    assert_eq!(rig.details("continued").len(), 1);
    let saved = load(&rig.opts, SESSION).expect("state");
    assert_eq!(saved.phase, Phase::Done, "never in flight again");
    assert!(saved.confirming());
    // The next sweep, before the answer: nothing to say, still owed.
    assert_eq!(confirmations(&rig.opts), []);
    // The answer is written — past the old build's one more row, which is not
    // it — and the sweep after says the model.
    let old_build = turn_by("claude-opus-5", "One more.").replace("2.1.282", "2.1.281");
    rig.append(&[old_build, turn_by("claude-opus-5-5", "Resumed.")]);
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done"]
    );
    assert_eq!(
        rig.details("done"),
        ["claude restarted on 2.1.282 · model claude-opus-5-5"]
    );
    assert!(!load(&rig.opts, SESSION).expect("state").confirming());
    assert_eq!(confirmations(&rig.opts), [], "said once");
    // A sweep for another tab leaves it alone.
    let mut other = load(&rig.opts, SESSION).expect("state");
    other.confirm_by = now_s() + 60;
    save(&rig.opts, SESSION, &other);
    let scoped = Opts {
        only_sid: Some("s-0000000000000000beef".to_string()),
        ..rig.opts.clone()
    };
    assert_eq!(confirmations(&scoped), []);
    assert_eq!(rig.typed().len(), 1);
}

/// The reason, end to end: a session launched with `--model claude-sonnet-5`
/// and moved to opus with `/model` comes back on sonnet; the kept flag did it,
/// and the outcome says so rather than blaming the default.
#[cfg(unix)]
#[test]
fn a_kept_model_flag_is_the_reason_a_launch_with_one_changed_model() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("model-flag", &before);
    rig.append(&[turn_by("claude-sonnet-5", "Resumed.")]);
    let mut st = St {
        launch_model: "claude-sonnet-5".to_string(),
        ..rig.st.clone()
    };
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    let said = confirmations(&rig.opts);
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].step, "done", "{said:?}");
    assert_eq!(
        rig.details("done"),
        [
            "claude restarted on 2.1.282 · model claude-opus-5-5 -> claude-sonnet-5 (the relaunch \
          kept the launch's --model claude-sonnet-5; /model changes it)"
        ]
    );
}

/// One outcome per restart: a newer build landing while the last restart's
/// model is still owed waits for that `done` row, since a new upgrade starts
/// from a fresh state and would lose it.
#[cfg(unix)]
#[test]
fn a_new_upgrade_waits_for_the_last_restarts_model() {
    let dir = scratch("confirm-before-next");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let pending = St {
        phase: Phase::Done,
        from: "0.9.0".to_string(),
        to: "1.0.0".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        mark: 1,
        confirm_by: now_s() + 60,
        resumed_on: "1.0.0".to_string(),
        ..St::default()
    };
    save(&opts, SESSION, &pending);
    // As written: the one writer stamps its progress (`St::progress_at`).
    let pending = load(&opts, SESSION).expect("saved");
    let r = visit(&opts, &sf, &[], &newer(), &Script::new(1, usize::MAX, None));
    assert_eq!(r.step, "wait:confirming", "{r:?}");
    assert_eq!(
        load(&opts, SESSION),
        Some(pending),
        "the owed outcome is kept"
    );
    // Once it is said, the next build is upgraded to.
    let said = St {
        confirm_by: 0,
        ..load(&opts, SESSION).expect("state")
    };
    save(&opts, SESSION, &said);
    let r = visit(&opts, &sf, &[], &newer(), &Script::new(1, usize::MAX, None));
    assert_ne!(r.step, "wait:confirming", "{r:?}");
    agent.kill().expect("stop agent");
    agent.wait().expect("reap agent");
    let _ = std::fs::remove_dir_all(dir);
}

/// A refusal with a model due (the ladder's `build-restart` keeps `model_to` set every
/// pass) is still said and ledgered once: the reuse guard matches the Failed state. The
/// first pass must still refuse.
#[cfg(unix)]
#[test]
fn a_refusal_with_a_model_due_is_said_once_not_every_pass() {
    let dir = scratch("job-model");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    // A warm transcript: the conversation answered on claude-opus-5 just now.
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(now)
        ),
    )
    .expect("transcript");
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    // Precondition, so the pass below is about this path: the move is due, the cache is
    // warm, and the ladder takes it because a build restarts.
    let mread = model_read(&opts, &sf, None, &mctx);
    assert!(
        matches!(&mread.verdict, ModelVerdict::Due { to, .. } if to == "claude-opus-5-5"),
        "the move must be due for this test to mean anything"
    );
    assert!(!mread.cold, "the transcript must read WARM");
    assert_eq!(
        models::model_moves_now(mread.cold, true, mread.due_for_s),
        Some("build-restart")
    );
    let files = session_files(&opts.home);
    let pass = || {
        visit_models(
            &opts,
            &sf,
            files.as_deref(),
            &table(),
            &newer(),
            &Live,
            None,
            None,
            &mctx,
        )
    };
    let first = pass();
    let again = pass();
    let third = pass();
    let _ = agent.kill();
    let _ = agent.wait();
    assert_eq!(first.step, "refused:not-a-shell-job", "{first:?}");
    assert_eq!(
        load(&opts, SESSION).map(|s| s.phase),
        Some(Phase::Failed("not-a-shell-job".to_string()))
    );
    for later in [&again, &third] {
        assert!(
            !later.is_act(),
            "a final refusal was said again on a later pass: {later:?}"
        );
    }
    assert_eq!(ledger_lines(&opts), 1, "one refusal, one ledger row");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ride is re-judged at every visit: a model the record says failed is dropped from an
/// announced round once (`model-ride-dropped`), the round kept; with the build current the
/// agent is released; where the rule now moves to another model the round starts over.
/// Negative control: a record that never failed keeps its model, nothing said.
#[cfg(unix)]
#[test]
fn a_barred_ride_is_dropped_once_and_the_round_goes_on_without_it() {
    let dir = scratch("ride-barred");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let now = now_s();
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(i64::try_from(now).expect("now"))
        ),
    )
    .expect("transcript");
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    let salt = now - 500;
    let announced = |to: &str| St {
        phase: Phase::Announced { at_s: now, asks: 1 },
        from: "1.0.0".to_string(),
        to: to.to_string(),
        source: "managed".to_string(),
        model_list: "claude-opus-5-5".to_string(),
        marker: "ATERM-UPGRADE-READY-0badf00d".to_string(),
        tab: TAB.to_string(),
        notice_pid: sf.pid,
        notice_start: squash(&sf.proc_start),
        salt,
        ..St::default()
    };
    let current = Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("1.0.0").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let files = session_files(&opts.home);
    // The agent is its shell's foreground job, in a tab aterm owns: the visit
    // goes past the process proofs and stops only at the missing socket.
    let kernel = Script::new(1, 1_000, None);
    let pass = |targets: &Targets| {
        visit_models(
            &opts,
            &sf,
            files.as_deref(),
            &table(),
            targets,
            &kernel,
            None,
            None,
            &mctx,
        )
    };
    let dropped = || -> Vec<String> {
        std::fs::read_to_string(ledger_path(&opts))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("\"step\":\"model-ride-dropped\""))
            .map(str::to_string)
            .collect()
    };

    // Negative control: nothing recorded against the model — it rides.
    save(&opts, SESSION, &announced("9.9.9"));
    let _ = pass(&newer());
    assert!(dropped().is_empty(), "{:?}", dropped());
    let kept = load(&opts, SESSION).expect("state");
    assert_eq!(kept.model_list, "claude-opus-5-5");

    // (a) Recorded failed: the ride is dropped, once, and the round goes on
    // to the build in place.
    let failed = ModelRecord {
        failed: vec!["claude-opus-5-5".to_string()],
        ..ModelRecord::default()
    };
    save_model_record(&opts, SESSION, &failed);
    save(&opts, SESSION, &announced("9.9.9"));
    let _ = pass(&newer());
    let rows = dropped();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("model:claude-opus-5-5")
            && rows[0].contains("model-failed-before")
            && rows[0]
                .contains("the round goes on without it, its notice and any READY still count"),
        "{rows:?}"
    );
    let kept = load(&opts, SESSION).expect("state");
    assert!(kept.model_list.is_empty(), "the round forgot the model");
    assert!(
        matches!(kept.phase, Phase::Announced { .. }),
        "{:?}",
        kept.phase
    );
    assert_eq!(
        (kept.to.as_str(), kept.salt),
        ("9.9.9", salt),
        "the round kept"
    );
    assert_eq!(kept.marker, "ATERM-UPGRADE-READY-0badf00d");
    let _ = pass(&newer());
    assert_eq!(dropped().len(), 1, "said once");

    // With the build current, the round had only the model to move: released.
    save(&opts, SESSION, &announced("1.0.0"));
    let _ = pass(&current);
    let rows = dropped();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[1].contains("is released"), "{rows:?}");
    let released = load(&opts, SESSION).expect("state");
    assert_eq!(released.phase, Phase::Pending);
    assert!(released.model_list.is_empty() && released.marker.is_empty());
    assert_eq!(
        released.release, "model-dropped",
        "the agent is owed its release"
    );

    // The rule moves it now to another model: the round is begun again.
    save_model_record(&opts, SESSION, &ModelRecord::default());
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(i64::try_from(now).expect("now"))
        ),
    )
    .expect("transcript");
    let fable = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec![
            "claude-opus-5-5".to_string(),
            "claude-fable-5-1".to_string(),
        ],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    save(&opts, SESSION, &announced("9.9.9"));
    let _ = visit_models(
        &opts,
        &sf,
        files.as_deref(),
        &table(),
        &newer(),
        &kernel,
        None,
        None,
        &fable,
    );
    let rows = dropped();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert!(
        rows[2].contains("model-off-list")
            && rows[2].contains("started over")
            && !rows[2].contains("still count"),
        "{rows:?}"
    );

    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `relaunch::plan`, the one place every relaunch line is made, types the
/// `CLAUDE_CONFIG_DIR` its process had in front of the program (the login after it);
/// without it, a Claude Code under another home comes back in the first-run theme picker.
/// Negative control: no home, the plain line byte for byte.
#[test]
fn the_planned_relaunch_line_carries_the_claude_home() {
    let dir = scratch("line-home");
    let opts = drive(&dir);
    let argv = vec!["claude".to_string(), "--verbose".to_string()];
    let planned = |home: Option<&str>, login: Option<&str>| {
        let launch = crate::harness::relaunch::Launch {
            stored_login: false,
            session: SESSION,
            resume: true,
            cwd: "/tmp",
            agent: 0,
            argv: &argv,
            exe: Path::new("/opt/claude"),
            model: None,
            mode: None,
            login,
            home,
        };
        crate::harness::relaunch::plan(&opts, &launch, 999_999, &[(999_999, 1, "zsh".to_string())])
            .unwrap_or_else(|_| panic!("a plan"))
            .line
    };
    let home = "CLAUDE_CONFIG_DIR=/lab/home/.claude ";
    let login = "CLAUDE_SECURESTORAGE_CONFIG_DIR=/logins/w ";
    let plain = planned(None, None);
    let program = |line: &str| line.find("'/opt/claude'").expect("the program");
    // The home, in front of the program; otherwise the plain line.
    let with_home = planned(Some("/lab/home/.claude"), None);
    assert!(
        with_home
            .find(home)
            .is_some_and(|at| at < program(&with_home)),
        "{with_home}"
    );
    assert_eq!(with_home.replacen(home, "", 1), plain);
    // The home first, then the login: both.
    let both = planned(Some("/lab/home/.claude"), Some("/logins/w"));
    assert!(both.contains(&format!("{home}{login}")), "{both}");
    assert_eq!(both.replacen(home, "", 1).replacen(login, "", 1), plain);
    // Negative control: the process had no home of its own.
    assert!(!plain.contains("CLAUDE_CONFIG_DIR"), "{plain}");
    assert!(
        plain.contains(&format!("'/opt/claude' '--verbose' '--resume' '{SESSION}'")),
        "{plain}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The planned line carries the usage tap where its gate holds (`relaunch::plan`, `tap::gate`;
/// `docs/DESIGN-claude-accounts-v2-2026-10-15.md` §3.1): with `[harness] usage_tap` naming this
/// aterm ([`Opts::usage_tap`]), a process on its stored login in the default home is relaunched
/// with `--settings <the tap>` before `--resume`, judged against the settings chain Claude Code
/// reads in the directory it starts in. Each guard alone takes the tap off: a statusLine in the
/// user's, cowork, project shared or local, or managed settings; `disableAllHooks`; an
/// `apiKeyHelper`; an `ANTHROPIC_BASE_URL` in `env`; a file that does not parse; a managed
/// preference domain; a profile store config; a person's own `--settings`, carried verbatim.
/// aterm's own tap at a stale path is replaced by the current one. A line on the login of the
/// machine's one account (`tap::Armed::lone_login`, the login a renewal bound its row to)
/// carries it too. Negative controls: with the switch off, off its stored login, or on a named
/// login that is not the one account's, the line is byte for byte the untapped one.
#[test]
fn the_planned_relaunch_line_carries_the_usage_tap_only_where_its_gate_holds() {
    use super::super::tap;
    let dir = scratch("line-tap");
    let mut opts = drive(&dir);
    let project = dir.join("proj");
    std::fs::create_dir_all(project.join(".claude")).expect("project");
    std::fs::create_dir_all(opts.home.join(".claude")).expect("user dir");
    let cwd = project.to_str().expect("utf-8").to_owned();
    let exe = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let value = tap::settings_value(exe).expect("an admissible path");
    let planned = |opts: &Opts, argv: &[String], stored_login: bool| {
        let launch = crate::harness::relaunch::Launch {
            stored_login,
            session: SESSION,
            resume: true,
            cwd: &cwd,
            agent: 0,
            argv,
            exe: Path::new("/opt/claude"),
            model: None,
            mode: None,
            login: None,
            home: None,
        };
        crate::harness::relaunch::plan(opts, &launch, 999_999, &[(999_999, 1, "zsh".to_string())])
            .unwrap_or_else(|_| panic!("a plan"))
            .line
    };
    let argv = vec!["claude".to_string(), "--verbose".to_string()];
    let plain = planned(&opts, &argv, true);
    assert!(
        plain.contains(&format!(
            "'/opt/claude' '--verbose' '--resume' '{SESSION}' )"
        )),
        "{plain}"
    );
    // Negative control: with the switch off, no `--settings`.
    assert!(!plain.contains("--settings"), "{plain}");
    opts.usage_tap = Some(tap::Armed {
        exe: exe.to_path_buf(),
        lone_login: None,
    });
    let pair = format!("'--settings' '{value}'");
    let tapped = planned(&opts, &argv, true);
    assert!(
        tapped.contains(&format!(
            "'/opt/claude' '--verbose' {pair} '--resume' '{SESSION}' )"
        )),
        "{tapped}"
    );
    assert_eq!(tapped.replacen(&format!(" {pair}"), "", 1), plain);
    // Negative control: not on its stored login, no tap.
    assert_eq!(planned(&opts, &argv, false), plain);
    // The one account's own login: tapped as the default login is.
    let renewed = "/tmp/aterm-state/logins/home-a.1";
    let on_login = |lone: Option<&str>| {
        let opts = Opts {
            usage_tap: Some(tap::Armed {
                exe: exe.to_path_buf(),
                lone_login: lone.map(str::to_owned),
            }),
            ..opts.clone()
        };
        let launch = crate::harness::relaunch::Launch {
            login: Some(renewed),
            stored_login: true,
            session: SESSION,
            resume: true,
            cwd: &cwd,
            agent: 0,
            argv: &argv,
            exe: Path::new("/opt/claude"),
            model: None,
            mode: None,
            home: None,
        };
        crate::harness::relaunch::plan(&opts, &launch, 999_999, &[(999_999, 1, "zsh".to_string())])
            .unwrap_or_else(|_| panic!("a plan"))
            .line
    };
    let renewed_line = on_login(Some(renewed));
    assert!(renewed_line.contains(renewed), "{renewed_line}");
    assert!(renewed_line.contains(&pair), "{renewed_line}");
    for lone in [None, Some("/tmp/aterm-state/logins/home-a.2")] {
        let line = on_login(lone);
        assert!(line.contains(renewed), "{line}");
        assert!(!line.contains("--settings"), "{lone:?}: {line}");
    }
    // aterm's own tap at a stale path is shed and the current one carried.
    let stale = tap::settings_value(Path::new("/Users//example/old/aterm")).expect("a path");
    let rerun: Vec<String> = argv
        .iter()
        .cloned()
        .chain(["--settings".to_string(), stale.clone()])
        .collect();
    let line = planned(&opts, &rerun, true);
    assert_eq!(line, tapped, "the stale tap is replaced");
    assert!(!line.contains("/Users//example/old"), "{line}");
    // A person's own `--settings` is carried verbatim and blocks the tap.
    let theirs: Vec<String> = argv
        .iter()
        .cloned()
        .chain(["--settings".to_string(), r#"{"model":"x"}"#.to_string()])
        .collect();
    let line = planned(&opts, &theirs, true);
    assert!(line.contains(r#"'--settings' '{"model":"x"}'"#), "{line}");
    assert!(!line.contains("harness tap"), "{line}");
    // Each document of the chain, alone, takes the tap off.
    let user = opts.home.join(".claude/settings.json");
    let cowork = opts.home.join(".claude/cowork_settings.json");
    let shared = project.join(".claude/settings.json");
    let local = project.join(".claude/settings.local.json");
    let status = r#"{"statusLine":{"type":"command","command":"echo mine"}}"#;
    for (file, text) in [
        (&user, status),
        (&cowork, status),
        (&shared, status),
        (&local, status),
        (&local, r#"{"disableAllHooks":true}"#),
        (&user, r#"{"apiKeyHelper":"/usr/local/bin/key"}"#),
        (
            &user,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://gw.example"}}"#,
        ),
        (&shared, "{not json"),
    ] {
        std::fs::write(file, text).expect("write");
        assert_eq!(
            planned(&opts, &argv, true),
            plain,
            "{} {text}",
            file.display()
        );
        std::fs::remove_file(file).expect("remove");
    }
    assert_eq!(
        planned(&opts, &argv, true),
        tapped,
        "every document quiet again"
    );
    // The managed file and a managed preference domain.
    let managed = dir.join("managed");
    std::fs::create_dir_all(&managed).expect("managed");
    std::fs::write(managed.join("managed-settings.json"), status).expect("write");
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = Some(managed.clone()));
    assert_eq!(planned(&opts, &argv, true), plain, "a managed statusLine");
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = None);
    let prefs = dir.join("prefs");
    std::fs::create_dir_all(&prefs).expect("prefs");
    std::fs::write(prefs.join("com.anthropic.claudecode.plist"), "<plist/>").expect("write");
    MANAGED_PREFERENCES.with(|m| *m.borrow_mut() = Some(prefs.clone()));
    assert_eq!(
        planned(&opts, &argv, true),
        plain,
        "a managed preference domain"
    );
    MANAGED_PREFERENCES.with(|m| *m.borrow_mut() = None);
    // A profile store config.
    let configs = opts.home.join(".config/anthropic/configs");
    std::fs::create_dir_all(&configs).expect("configs");
    std::fs::write(configs.join("default.json"), "{}").expect("write");
    assert_eq!(planned(&opts, &argv, true), plain, "a profile store config");
    std::fs::remove_file(configs.join("default.json")).expect("remove");
    assert_eq!(planned(&opts, &argv, true), tapped, "and back");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `--model` this harness asked and the record marks failed never rides the line: a
/// relaunch asking no model drops it and keeps none. A person's own id or alias, and a
/// site's own model, stand. Negative control: with no failure the `--model` is kept.
#[test]
fn a_failed_model_on_the_launch_line_is_dropped_by_a_relaunch_asking_none() {
    let dir = scratch("line-failed");
    let opts = drive(&dir);
    let argv = |model: &str| -> Vec<String> {
        ["claude", "--model", model, "--verbose"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    };
    let line = |argv: &[String], model: Option<&str>| {
        line_for(
            Dialect::Bash,
            None,
            None,
            Path::new("/opt/claude"),
            argv,
            Some(SESSION),
            line_model_of(&opts, SESSION, argv, model),
            None,
            &[],
        )
        .unwrap_or_else(|_| panic!("a line"))
    };
    let ours = argv("claude-opus-5-5[1m]");
    let kept = format!(
        " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' '--model' 'claude-opus-5-5[1m]' \
         '--verbose' '--resume' '{SESSION}'"
    );
    // The relaunch as `relaunch::plan` makes it (asking the rule), on a zsh the table names.
    let planned = || {
        let launch = crate::harness::relaunch::Launch {
            stored_login: false,
            session: SESSION,
            resume: true,
            cwd: "/tmp",
            agent: 0,
            argv: &ours,
            exe: Path::new("/opt/claude"),
            model: None,
            mode: None,
            login: None,
            home: None,
        };
        crate::harness::relaunch::plan(&opts, &launch, 999_999, &[(999_999, 1, "zsh".to_string())])
            .unwrap_or_else(|_| panic!("a plan"))
            .line
    };
    // The line a person is told to run (`resume_command_kept`).
    let told = || resume_command_kept(Some(&opts.state), &ours, SESSION).expect("a resume line");
    // ... and the footer's, as the window's resolver reads the live process
    // (`footer::read_pid`): the frozen program's remedy and the menu-bar row
    // tell a person the same line.
    register(&opts.home, std::process::id(), SESSION);
    let footer = || {
        let offset = |_: Option<&str>, _: i64| None;
        crate::harness::footer::read_pid(
            &opts.home.join(".claude"),
            std::process::id(),
            None,
            |_| (crate::harness::footer::LaunchFacts::default(), ours.clone()),
            &mut crate::harness::footer::TailCache::default(),
            &mut crate::harness::footer::FooterCache::default(),
            &crate::harness::footer::ReadClock {
                read_at: std::time::SystemTime::now(),
                now: 0,
                offset_at: &offset,
            },
            Some(&opts.state),
        )
        .and_then(|facts| facts.resume)
        .expect("the footer's line")
    };
    // Negative control: nothing failed — the flag is kept.
    assert_eq!(line(&ours, None), kept);
    assert!(planned().contains("'--model'"), "{}", planned());
    assert!(told().contains("--model"), "{}", told());
    assert_eq!(footer(), told());
    assert_eq!(
        launch_kept(&opts, SESSION, &ours).as_deref(),
        Some("claude-opus-5-5[1m]")
    );
    save_model_record(
        &opts,
        SESSION,
        &ModelRecord {
            failed: vec!["claude-opus-5-5".to_string()],
            ..ModelRecord::default()
        },
    );
    assert_eq!(
        line(&ours, None),
        format!(
            " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 '/opt/claude' '--verbose' '--resume' \
             '{SESSION}'"
        )
    );
    assert_eq!(launch_kept(&opts, SESSION, &ours), None);
    assert!(!planned().contains("--model"), "{}", planned());
    assert!(
        !told().contains("--model") && told().contains(&format!("--resume {SESSION}")),
        "{}",
        told()
    );
    assert_eq!(footer(), told());
    // A site's own model replaces it.
    assert!(line(&ours, Some("claude-fable-5-1")).contains("'--model' 'claude-fable-5-1'"));
    // A person's own id or alias, which no ask on the record names, stands.
    for theirs in ["claude-fable-5-1", "opus"] {
        let argv = argv(theirs);
        assert!(line(&argv, None).contains(&format!("'--model' '{theirs}'")));
        assert_eq!(launch_kept(&opts, SESSION, &argv).as_deref(), Some(theirs));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A due model rides every restart (a relaunch on exit, the restart in place), even on a
/// warm conversation, and is recorded as asked for. Negative control: nothing due, nothing
/// rides.
#[test]
fn a_due_model_rides_a_restart_made_for_another_reason() {
    let dir = scratch("riding-model");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
            crate::harness::usage::rfc3339_utc(now)
        ),
    )
    .expect("transcript");
    let argv = vec!["/opt/claude/bin/claude".to_string()];
    let due = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    assert_eq!(
        riding_model_in(&opts, &sf, &argv, &due).as_deref(),
        Some("claude-opus-5-5"),
        "warm, and it rides all the same"
    );
    record_asked(&opts, SESSION, "claude-opus-5-5");
    assert_eq!(load_model_record(&opts, SESSION).set, "claude-opus-5-5");
    assert_eq!(riding_model_in(&opts, &sf, &argv, &ModelCtx::none()), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The due clock survives an unknown read (`model-unknown`, e.g. a tail holding no
/// answer), or an active session's one-hour bound would restart every visit; a definite
/// "nothing due" (no model can run, none pending) clears it, so a later move does not
/// inherit a stale start.
#[test]
fn the_due_clock_survives_an_unknown_read_and_clears_when_nothing_is_due() {
    let dir = scratch("due-clock");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let path = proj.join(format!("{SESSION}.jsonl"));
    let answered = format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5\"}}}}\n",
        crate::harness::usage::rfc3339_utc(now)
    );
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    std::fs::write(&path, &answered).expect("transcript");
    assert!(matches!(
        model_read(&opts, &sf, None, &mctx).verdict,
        ModelVerdict::Due { .. }
    ));
    // Due for 50 minutes already.
    let mut rec = load_model_record(&opts, SESSION);
    assert_eq!(
        rec.due_to, "claude-opus-5-5",
        "the clock started on the due visit"
    );
    rec.due_since = now_s() - 3000;
    save_model_record(&opts, SESSION, &rec);

    // A visit that cannot read the live model: the clock stands.
    std::fs::write(&path, "{\"type\":\"user\"}\n").expect("tail with no answer");
    let unknown = model_read(&opts, &sf, None, &mctx);
    assert_eq!(unknown.verdict, ModelVerdict::Keep("model-unknown"));
    let kept = load_model_record(&opts, SESSION);
    assert_eq!(
        (kept.due_to.as_str(), kept.due_since),
        ("claude-opus-5-5", rec.due_since),
        "an unknown read reset the due clock"
    );

    // Readable again: the wait resumes where it was, not from zero.
    std::fs::write(&path, &answered).expect("transcript");
    let back = model_read(&opts, &sf, None, &mctx);
    assert!(
        back.due_for_s >= 3000,
        "the bound restarted: {}",
        back.due_for_s
    );

    // A start ahead of now (the wall clock stepped back): restarted at now,
    // never left to read as 0 s due for the whole skew.
    let mut ahead = load_model_record(&opts, SESSION);
    ahead.due_since = now_s() + 10_000;
    save_model_record(&opts, SESSION, &ahead);
    let _ = model_read(&opts, &sf, None, &mctx);
    assert!(
        load_model_record(&opts, SESSION).due_since <= now_s(),
        "a future-dated start survived and would postpone the bound"
    );

    // Nothing can run and nothing is pending: that clears it.
    let none = ModelCtx {
        offered: Vec::new(),
        ..mctx
    };
    let _ = model_read(&opts, &sf, None, &none);
    let cleared = load_model_record(&opts, SESSION);
    assert!(
        cleared.due_to.is_empty() && cleared.due_since == 0,
        "the early return left a stale clock: {cleared:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An ask that ran stands: once applied, a person moving the conversation off it is no
/// failure (a false `model-failed` would bar the model for good). Negative control: an ask
/// that never ran settles failed after `MODEL_SETTLE_S`.
#[test]
fn an_ask_that_ran_is_never_recorded_failed_when_a_person_moves_off_it() {
    let dir = scratch("ask-ran-stands");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    let now = i64::try_from(now_s()).expect("now");
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let answered = format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
        crate::harness::usage::rfc3339_utc(now)
    );
    std::fs::write(proj.join(format!("{SESSION}.jsonl")), &answered).expect("transcript");
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: None,
    };
    let asked = |applied: bool| {
        let mut rec = ModelRecord {
            set: "claude-opus-5-5".to_string(),
            set_at: now_s() - 2 * models::MODEL_SETTLE_S,
            ..ModelRecord::default()
        };
        if applied {
            rec.applied.push(rec.set.clone());
        }
        save_model_record(&opts, SESSION, &rec);
    };

    asked(true);
    let _ = model_read(&opts, &sf, None, &mctx);
    let rec = load_model_record(&opts, SESSION);
    assert!(
        rec.failed.is_empty(),
        "an ask that ran was recorded failed: {rec:?}"
    );
    assert_eq!(
        rec.set, "claude-opus-5-5",
        "the ask stays the harness's own"
    );

    // Negative control: the same ask, never run, settles as failed.
    asked(false);
    let _ = model_read(&opts, &sf, None, &mctx);
    let rec = load_model_record(&opts, SESSION);
    assert_eq!(rec.failed, ["claude-opus-5-5"], "{rec:?}");
    assert!(rec.set.is_empty(), "{rec:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The transcript of a conversation whose last answer, on `model`, came
/// `ago_s` seconds ago.
fn answered_on(opts: &Opts, model: &str, ago_s: i64) {
    let at = i64::try_from(now_s()).expect("now") - ago_s;
    let proj = opts.home.join(".claude/projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"{model}\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
}

/// A session on the native build is judged by the native build's own catalog and offer
/// (here Fable 5.1, where the managed view offers Opus 5.5); with no native build
/// installed it falls back to the managed view.
#[test]
fn a_native_session_is_judged_by_the_native_builds_own_offer() {
    let dir = scratch("native-view");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    answered_on(&opts, "claude-opus-5", 7200);
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: Some((None, vec!["claude-fable-5-1".to_string()])),
    };
    let due_to =
        |native: bool, ctx: &ModelCtx| match model_read_for(&opts, &sf, None, ctx, native, None)
            .verdict
        {
            ModelVerdict::Due { to, .. } => Some(to),
            ModelVerdict::Keep(_) => None,
        };
    assert_eq!(due_to(false, &mctx).as_deref(), Some("claude-opus-5-5"));
    assert_eq!(due_to(true, &mctx).as_deref(), Some("claude-fable-5-1"));
    let no_native = ModelCtx {
        native: None,
        ..mctx
    };
    assert_eq!(due_to(true, &no_native).as_deref(), Some("claude-opus-5-5"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host's pre-filter never restarts a native session's warm wait: writing what it read
/// under both views would clear the due clock (managed `model-current`) and restart it
/// (native `Due`), so `warm-wait-over` would never fire. Negative control: where the views
/// agree, the pre-filter's settle step is kept.
#[test]
fn the_pre_filter_never_restarts_a_native_sessions_warm_wait() {
    let dir = scratch("prefilter-views");
    let opts = drive(&dir);
    let sf = register(&opts.home, std::process::id(), SESSION);
    answered_on(&opts, "claude-opus-5", 0);
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: Some((None, vec!["claude-opus-5-5".to_string()])),
    };
    let first = model_read_for(&opts, &sf, None, &mctx, true, None);
    assert!(
        matches!(&first.verdict, ModelVerdict::Due { to, .. } if to == "claude-opus-5-5"),
        "the native view must see the move due: {:?}",
        first.verdict
    );
    let mut rec = load_model_record(&opts, SESSION);
    rec.due_since = now_s() - 3000;
    save_model_record(&opts, SESSION, &rec);
    for sweep in 0..3 {
        assert!(model_wants_a_look(&opts, &sf, &mctx), "sweep {sweep}");
        let read = model_read_for(&opts, &sf, None, &mctx, true, None);
        assert!(
            read.due_for_s >= 3000,
            "sweep {sweep}: the pre-filter restarted the warm wait ({} s due)",
            read.due_for_s
        );
    }
    let mut over = load_model_record(&opts, SESSION);
    over.due_since = now_s() - models::MODEL_WARM_MAX_S;
    save_model_record(&opts, SESSION, &over);
    assert!(model_wants_a_look(&opts, &sf, &mctx));
    let read = model_read_for(&opts, &sf, None, &mctx, true, None);
    assert_eq!(
        models::model_moves_now(read.cold, false, read.due_for_s),
        Some("warm-wait-over"),
        "the warm wait must end for an active native session"
    );

    // Negative control: the views agree, and the pre-filter keeps its settle.
    let agree = ModelCtx {
        offered: vec!["claude-opus-5-5".to_string()],
        ..mctx
    };
    record_asked(&opts, SESSION, "claude-opus-5-5");
    answered_on(&opts, "claude-opus-5-5", 0);
    assert!(!model_wants_a_look(&opts, &sf, &agree), "nothing due");
    assert_eq!(
        load_model_record(&opts, SESSION).applied,
        ["claude-opus-5-5"],
        "the pre-filter's settle step was not kept where the views agree"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A crashed native session's riding model is the native build's: its pid has exited, so
/// the view is the program the crash ran (`argv[0]`), which the relaunch runs again.
/// Negative control: a managed program on the same dead pid gets the managed view.
#[test]
fn a_crashed_native_sessions_riding_model_is_the_native_builds() {
    let dir = scratch("riding-native");
    let opts = drive(&dir);
    // The crash's own record, read while it lived; its pid since exited.
    let mut sf = register(&opts.home, std::process::id(), SESSION);
    sf.pid = dead_pid();
    assert_eq!(exe_of(sf.pid), None, "the pid must be gone for this test");
    answered_on(&opts, "claude-opus-5", 0);
    let mctx = ModelCtx {
        list: Priority::seed(0),
        baked: None,
        offered: vec!["claude-opus-5".to_string()],
        default_model: None,
        versions: Default::default(),
        native: Some((None, vec!["claude-opus-5-5".to_string()])),
    };
    let native = native_root(&opts.home).join("2.1.283");
    let argv = vec![native.to_string_lossy().into_owned()];
    assert_eq!(
        riding_model_in(&opts, &sf, &argv, &mctx).as_deref(),
        Some("claude-opus-5-5"),
        "a crashed native session was judged by the managed view"
    );
    let managed = vec!["/opt/aterm/pkg/agents/claude".to_string()];
    assert_eq!(riding_model_in(&opts, &sf, &managed, &mctx), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent under another Claude home is judged by that home's model files
/// ([`ModelCtx::at`]): its `settings.json`, `cache/model-catalog` and `.claude.json`.
/// Controls: nothing chosen or denied there is still due; an agent in the user's own home
/// is judged by the user's files; under several logins the other home's caches are unread.
#[test]
fn an_agent_under_another_claude_home_is_judged_by_its_homes_model_files() {
    let dir = scratch("model-home");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    // Both models served, neither chosen nor denied, in the user's home.
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    let served_in = |claude_dir: &Path| {
        let catalog = claude_dir.join("cache/model-catalog");
        std::fs::create_dir_all(&catalog).expect("the catalog cache");
        std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    };
    served_in(&opts.home.join(".claude"));
    // No build: the offer is the list as the served catalog admits it.
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    // The agent under the other home answered on Fable 5.1 two hours ago.
    let other = dir.join("work/.claude");
    served_in(&other);
    let answered = |claude_dir: &Path| {
        let proj = claude_dir.join("projects/-work");
        std::fs::create_dir_all(&proj).expect("projects");
        let at = i64::try_from(now_s()).expect("now") - 7_200;
        std::fs::write(
            proj.join(format!("{SESSION}.jsonl")),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
                crate::harness::usage::rfc3339_utc(at)
            ),
        )
        .expect("transcript");
    };
    answered(&other);
    let sf = SessionFile {
        claude_dir: Some(other.clone()),
        ..record("2.1.296")
    };
    let verdict = |opts: &Opts, sf: &SessionFile| {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(opts, sf, None, &mctx, false, None).verdict
    };
    let due = ModelVerdict::Due {
        from: "claude-fable-5-1".to_string(),
        to: "claude-opus-5-5".to_string(),
    };
    // Control: nothing chosen or denied in its home either: due.
    assert_eq!(verdict(&opts, &sf), due, "the judge is not switched off");
    assert!(model_wants_a_look(&opts, &sf, &mctx));
    // Each of its home's files, alone, keeps it.
    let settings = other.join("settings.json");
    let profile = other.join(".claude.json");
    for (name, file, text, kept) in [
        (
            "its saved default",
            &settings,
            r#"{"model":"claude-fable-5-1"}"#,
            "model-chosen-by-hand",
        ),
        (
            "its allowlist",
            &settings,
            r#"{"availableModels":["claude-fable-5-1"]}"#,
            "model-current",
        ),
        (
            "its login's denial",
            &profile,
            r#"{"modelAccessCache":[{"apiName":"claude-opus-5-5","entitled":false}]}"#,
            "model-current",
        ),
    ] {
        std::fs::write(file, text).expect("the home's file");
        assert_eq!(
            verdict(&opts, &sf),
            ModelVerdict::Keep(kept),
            "{name}: judged by the user's home"
        );
        assert!(!model_wants_a_look(&opts, &sf, &mctx), "{name}");
        let argv = vec!["claude".to_string()];
        assert_eq!(riding_model_in(&opts, &sf, &argv, &mctx), None, "{name}");
        // Control: the same conversation in the user's own home is judged by
        // the user's files, which choose and deny nothing.
        answered(&opts.home.join(".claude"));
        let own = SessionFile {
            claude_dir: Some(opts.home.join(".claude")),
            ..sf.clone()
        };
        assert_eq!(verdict(&opts, &own), due, "{name}: the user's own home");
        let _ = std::fs::remove_file(file);
    }
    // Under several logins the other home's caches are the unknown too, as
    // the user's are ([`Opts::several_logins`]): its served catalog is not
    // read, and with no build's own catalog nothing is offered.
    let several = Opts {
        several_logins: true,
        instances: Instances::Listed(Vec::new()),
        ..opts.clone()
    };
    assert_eq!(
        verdict(&several, &sf),
        ModelVerdict::Keep("no-model-available")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The model rule reads an agent's settings through the one chain
/// (`claude_settings::sources_under`), not its home's `settings.json` alone: a project's
/// `availableModels` or saved `model` keeps the agent where it is.
#[test]
fn the_model_rule_reads_the_agents_settings_chain_not_its_homes_file_alone() {
    let dir = scratch("model-settings-chain");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    let user = opts.home.join(".claude");
    let catalog = user.join("cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    // The agent, in the user's own home, answered on Fable 5.1 two hours ago
    // in the project `work`.
    let work = dir.join("work");
    let proj = user.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    let sf = SessionFile {
        claude_dir: Some(user.clone()),
        cwd: work.to_string_lossy().into_owned(),
        ..record("2.1.296")
    };
    let verdict = || {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(&opts, &sf, None, &mctx, false, None).verdict
    };
    // Control: nothing chosen or allowed anywhere: due.
    let due = ModelVerdict::Due {
        from: "claude-fable-5-1".to_string(),
        to: "claude-opus-5-5".to_string(),
    };
    assert_eq!(verdict(), due, "the judge is not switched off");
    let shared = work.join(".claude/settings.json");
    std::fs::create_dir_all(shared.parent().expect("dir")).expect("the project's .claude");
    for (name, text, kept) in [
        (
            "the project's allowlist",
            r#"{"availableModels":["claude-fable-5-1"]}"#,
            "model-current",
        ),
        (
            "the project's saved model",
            r#"{"model":"claude-fable-5-1"}"#,
            "model-chosen-by-hand",
        ),
    ] {
        std::fs::write(&shared, text).expect("the project's settings");
        assert_eq!(verdict(), ModelVerdict::Keep(kept), "{name}");
        let argv = vec!["claude".to_string()];
        assert_eq!(riding_model_in(&opts, &sf, &argv, &mctx), None, "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`agent_settings`]: each of `model` and `availableModels` comes from the highest
/// document that sets it (managed drop-ins over the managed file over the project's local
/// and shared files over the user's), a higher list replacing a lower one whole.
/// Fail-closed: an unparsable document leaves the chain unknown; a non-list
/// `availableModels` allows nothing.
#[test]
fn the_model_rule_takes_each_key_from_the_highest_document_that_sets_it() {
    let dir = scratch("model-settings-order");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    let user = opts.home.join(".claude");
    let catalog = user.join("cache/model-catalog");
    std::fs::create_dir_all(&catalog).expect("the catalog cache");
    std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    let work = dir.join("work");
    let proj = user.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    let sf = SessionFile {
        claude_dir: Some(user.clone()),
        cwd: work.to_string_lossy().into_owned(),
        ..record("2.1.296")
    };
    let managed = dir.join("managed");
    std::fs::create_dir_all(managed.join("managed-settings.d")).expect("the managed tier");
    std::fs::create_dir_all(work.join(".claude")).expect("the project's .claude");
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = Some(managed.clone()));
    let due = ModelVerdict::Due {
        from: "claude-fable-5-1".to_string(),
        to: "claude-opus-5-5".to_string(),
    };
    let only_fable = r#"{"availableModels":["claude-fable-5-1"]}"#;
    let both_allowed = r#"{"availableModels":["claude-opus-5-5","claude-fable-5-1"]}"#;
    let files = [
        ("managed", managed.join("managed-settings.json")),
        ("drop-in", managed.join("managed-settings.d/10-models.json")),
        ("project", work.join(".claude/settings.json")),
        ("local", work.join(".claude/settings.local.json")),
        ("user", user.join("settings.json")),
    ];
    let case = |name: &str, docs: &[(&str, &str)]| {
        for (_, path) in &files {
            let _ = std::fs::remove_file(path);
        }
        for (which, text) in docs {
            let path = &files
                .iter()
                .find(|(w, _)| w == which)
                .expect("a document")
                .1;
            std::fs::write(path, text).expect("the document");
        }
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        let verdict = model_read_for(&opts, &sf, None, &mctx, false, None).verdict;
        let _ = name;
        verdict
    };
    assert_eq!(
        case("nothing set", &[]),
        due,
        "the judge is not switched off"
    );
    assert_eq!(
        case(
            "managed over project",
            &[("managed", only_fable), ("project", both_allowed)]
        ),
        ModelVerdict::Keep("model-current")
    );
    assert_eq!(
        case(
            "a drop-in over the managed file",
            &[("managed", both_allowed), ("drop-in", only_fable)]
        ),
        ModelVerdict::Keep("model-current")
    );
    assert_eq!(
        case(
            "the local file over the user's",
            &[("local", only_fable), ("user", both_allowed)]
        ),
        ModelVerdict::Keep("model-current")
    );
    assert_eq!(
        case(
            "a higher list replaces a lower one whole",
            &[("project", both_allowed), ("user", only_fable)]
        ),
        due
    );
    assert_eq!(
        case(
            "the managed model",
            &[("managed", r#"{"model":"claude-fable-5-1"}"#)]
        ),
        ModelVerdict::Keep("model-chosen-by-hand")
    );
    assert_ne!(
        case("a document that does not parse", &[("project", "{")]),
        due,
        "unknown: nothing is moved"
    );
    assert_ne!(
        case(
            "an allowlist that is no list",
            &[("project", r#"{"availableModels":"opus"}"#)]
        ),
        due,
        "an empty allowlist"
    );
    MANAGED_SETTINGS.with(|m| *m.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent under an empty `CLAUDE_CONFIG_DIR` keeps its records and settings in the
/// directory it runs in, but its profile (the login's model caches) in `$HOME/.claude.json`
/// (Claude Code reads the empty value as unset there): the model rule reads its login's
/// denials there, through the sweep's own reader ([`Homes::read`]). Control: a relative
/// `.` keeps its profile inside the directory.
#[test]
fn an_agent_under_an_empty_claude_home_is_judged_by_the_profile_claude_code_keeps_for_it() {
    let dir = scratch("model-empty-home-profile");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    let work = dir.join("work");
    // Both models served, in the user's home and in the working directory,
    // which an empty home's Claude Code reads as its own.
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    for claude_dir in [opts.home.join(".claude"), work.clone()] {
        let catalog = claude_dir.join("cache/model-catalog");
        std::fs::create_dir_all(&catalog).expect("the catalog cache");
        std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    }
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&opts, &targets);
    // The agent leading the tab, filed where its Claude Code files it, its
    // last answer on Fable 5.1 two hours ago.
    const LEADER: u32 = 4_242;
    std::fs::create_dir_all(work.join("sessions")).expect("sessions");
    std::fs::write(
        work.join(format!("sessions/{LEADER}.json")),
        format!(
            r#"{{"pid":{LEADER},"sessionId":"{SESSION}","cwd":"{}","version":"2.1.296","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#,
            work.display()
        ),
    )
    .expect("the record");
    let proj = work.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(LEADER)),
    }];
    // The record as the sweep reads it, its leader's exec home `home`.
    let read = |home: &str| {
        let args = |_: u32| {
            Some(atpkg::caller_shell::ProcArgs {
                env: vec![format!("CLAUDE_CONFIG_DIR={home}")],
                ..atpkg::caller_shell::ProcArgs::default()
            })
        };
        Homes::read(&opts, Some(&tabs), args, |_| Some(work.clone()))
            .roster(Some(Vec::new()))
            .and_then(|files| files.into_iter().find(|sf| sf.pid == LEADER))
            .expect("the record is read where its Claude Code files it")
    };
    let verdict = |sf: &SessionFile| {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(&opts, sf, None, &mctx, false, None).verdict
    };
    let denial = r#"{"modelAccessCache":[{"apiName":"claude-opus-5-5","entitled":false}]}"#;
    let due = ModelVerdict::Due {
        from: "claude-fable-5-1".to_string(),
        to: "claude-opus-5-5".to_string(),
    };
    let empty = read("");
    // Control: nothing denied anywhere: due.
    assert_eq!(verdict(&empty), due, "the judge is not switched off");
    // Its login's denial, in `$HOME/.claude.json`: kept.
    let home_profile = opts.home.join(".claude.json");
    std::fs::write(&home_profile, denial).expect("the profile");
    assert_eq!(
        verdict(&empty),
        ModelVerdict::Keep("model-current"),
        "an empty home's login is read in $HOME/.claude.json"
    );
    std::fs::remove_file(&home_profile).expect("rm");
    // A `.claude.json` in its working directory is nobody's profile here.
    let cwd_profile = work.join(".claude.json");
    std::fs::write(&cwd_profile, denial).expect("a stray file");
    assert_eq!(
        verdict(&empty),
        due,
        "an empty home's working directory holds no profile of its"
    );
    // Control: `CLAUDE_CONFIG_DIR=.` names the same directory, and keeps its
    // profile inside it (`"." || homedir()` is `.`).
    let dot = read(".");
    assert_eq!(dot.claude_dir.as_deref(), Some(work.as_path()));
    assert_eq!(
        verdict(&dot),
        ModelVerdict::Keep("model-current"),
        "a relative home's profile is inside it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An agent under another Claude home is moved to a model announced to that home's login
/// (`tengu_startup_announcements`' `requiresModel`, admitted by [`models::recommendations`]
/// before the build names it its family's latest). Control: the same announcement in the
/// user's home moves the user's agent.
#[test]
fn an_agent_under_another_claude_home_moves_to_the_model_its_home_announces() {
    let dir = scratch("model-home-announces");
    let opts = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    // The managed build: it knows Opus 5.6 and still names Opus 5.5 its
    // family's latest.
    let bin = dir.join("build/claude");
    std::fs::create_dir_all(bin.parent().expect("dir")).expect("the build");
    std::fs::write(
        &bin,
        concat!(
            r#"VERSION:"2.1.300",models:[{id:"claude-opus-5",family:"opus",display_name:"Opus 5"},"#,
            r#"{id:"claude-opus-5-5",family:"opus",display_name:"Opus 5.5"},"#,
            r#"{id:"claude-opus-5-6",family:"opus",display_name:"Opus 5.6"},"#,
            r#"{id:"claude-fable-5-1",family:"fable",display_name:"Fable 5.1"}],"#,
            r#"latest_per_family:{fable:"claude-fable-5-1",opus:"claude-opus-5-5"},alias_migration:{}};"#,
        ),
    )
    .expect("the build's catalog");
    let twin = dir.join("agents/claude");
    std::fs::create_dir_all(twin.parent().expect("dir")).expect("the twin");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", bin.display()),
    )
    .expect("the twin");
    let targets = Targets {
        managed: Some(Candidate {
            exe: twin,
            version: Version::parse("2.1.300").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    // Its conversation answered on Opus 5.5 two hours ago, in `claude_dir`.
    let answered = |claude_dir: &Path| {
        let proj = claude_dir.join("projects/-work");
        std::fs::create_dir_all(&proj).expect("projects");
        let at = i64::try_from(now_s()).expect("now") - 7_200;
        std::fs::write(
            proj.join(format!("{SESSION}.jsonl")),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5-5\"}}}}\n",
                crate::harness::usage::rfc3339_utc(at)
            ),
        )
        .expect("transcript");
    };
    let announced = r#"{"cachedGrowthBookFeatures":{"tengu_startup_announcements":[{"id":"opus-5-6-launch","text":"t","requiresModel":"claude-opus-5-6"}]}}"#;
    let users = opts.home.join(".claude");
    std::fs::create_dir_all(&users).expect("the user's home");
    let due = ModelVerdict::Due {
        from: "claude-opus-5-5".to_string(),
        to: "claude-opus-5-6".to_string(),
    };
    let verdict = |sf: &SessionFile| {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        let mctx = ModelCtx::read(&opts, &targets);
        model_read_for(&opts, sf, None, &mctx, false, None).verdict
    };
    // An agent under another home, announced to there.
    let other = dir.join("work/.claude");
    std::fs::create_dir_all(&other).expect("its home");
    std::fs::write(other.join(".claude.json"), announced).expect("its profile");
    answered(&other);
    let theirs = SessionFile {
        claude_dir: Some(other.clone()),
        ..record("2.1.300")
    };
    assert_eq!(
        verdict(&theirs),
        due,
        "moved to the model its own home's login was announced"
    );
    // Control: the user's agent, the same announcement in the user's home.
    std::fs::write(opts.home.join(".claude.json"), announced).expect("the user's profile");
    answered(&users);
    let own = SessionFile {
        claude_dir: Some(users.clone()),
        ..record("2.1.300")
    };
    assert_eq!(verdict(&own), due, "the user's own agent");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The CLI's sweep (no roster, [`sweep`]) finds an agent under another Claude home that no
/// visit has recorded, reading the instances' tabs' leaders as the window's pass does
/// ([`Homes::read`]).
#[cfg(unix)]
#[test]
fn the_clis_sweep_finds_an_agent_under_another_claude_home_no_visit_recorded() {
    let dir = scratch("cli-sweep-other-home");
    let claude_dir = dir.join("work home/.claude");
    let mut agent = tty_agent(
        &[(crate::harness::accounts::HOME_ENV, claude_dir.as_path())],
        Some(&dir),
    );
    let pid = agent.id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    // The CLI's unscoped pass: no tab of its own, a dry run.
    let opts = Opts {
        sock: Some(sock),
        dry_run: true,
        ..drive(&dir)
    };
    seed_builds(&opts);
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("the user's sessions");
    register_as(&claude_dir, pid, SESSION, "unknown");
    let reports = sweep(&opts);
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        reports.iter().any(|r| r.session == SESSION && r.pid == pid),
        "the sweep visits it: {reports:?}"
    );
}

// ---------------------------------------------------------------- a parked agent

/// Claude's session file for `pid` as [`register`] writes it, its status
/// changed `ago_s` seconds before now: a turn that just ended is `0`.
fn register_idle_since(home: &Path, pid: u32, session: &str, ago_s: u64) -> SessionFile {
    let sf = register(home, pid, session);
    let at_ms = (now_s() - ago_s) * 1000;
    let path = home.join(format!(".claude/sessions/{pid}.json"));
    let text = std::fs::read_to_string(&path).expect("session file");
    std::fs::write(
        &path,
        text.replace(
            r#""statusUpdatedAt":1,"#,
            &format!(r#""statusUpdatedAt":{at_ms},"#),
        ),
    )
    .expect("rewrite");
    let again = session_file_of(home, pid).expect("the file parses");
    assert_eq!(again.status_updated_at_ms, at_ms);
    assert_eq!(again.session_id, sf.session_id);
    again
}

/// A stand-in agent parked in [`TAB`], registered with its status changed
/// `ago_s` seconds ago, and a pending upgrade for it whose screen has just
/// moved (`seq_since` now: not settled).
#[cfg(unix)]
struct Parked {
    dir: ScratchDir,
    opts: Opts,
    /// The stand-in agent, until a test takes it to reap it itself
    /// ([`Parked::reaped`]).
    agent: Option<aterm_uds::spawnfd::Child>,
    sf: SessionFile,
    asked: crate::harness::FakeInstanceLog,
}

#[cfg(unix)]
impl Parked {
    fn new(name: &str, answers: Answers, st: St, ago_s: u64) -> Parked {
        let dir = scratch(name);
        let (sock, asked) = instance_with(&dir, answers);
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        // One argument, as a launch the rewrite can carry: the parked test
        // binary's `--exact` is a flag Claude does not have.
        let agent = parked_bare().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register_idle_since(&opts.home, agent.id(), SESSION, ago_s);
        file_transcript_in(&opts.home.join(".claude"), "p");
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
        Parked {
            dir,
            opts,
            agent: Some(agent),
            sf,
            asked,
        }
    }

    fn visit(&self) -> Report {
        self.visit_under(dead_pid())
    }

    /// [`Self::visit`] with the agent the foreground job of `shell`, a zsh.
    fn visit_under(&self, shell: u32) -> Report {
        self.visit_with(shell, &[])
    }

    /// [`Self::visit_under`], the process table holding `more` too.
    fn visit_with(&self, shell: u32, more: &[(u32, u32, String)]) -> Report {
        let mut table = vec![(shell, 1, "zsh".to_string())];
        table.extend(more.iter().cloned());
        visit(
            &self.opts,
            &self.sf,
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
        )
    }

    /// The stand-in agent handed to a thread that reaps it the moment it ends
    /// and removes its session file, as Claude's own exit does, so a restart's
    /// wait for it to be gone ends on the event; says how it ended.
    fn reaped(&mut self) -> std::sync::mpsc::Receiver<std::process::ExitStatus> {
        let mut agent = self.agent.take().expect("the agent");
        let file = self
            .opts
            .home
            .join(format!(".claude/sessions/{}.json", agent.id()));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok(status) = agent.wait() {
                let _ = std::fs::remove_file(&file);
                let _ = tx.send(status);
            }
        });
        rx
    }

    /// The details of the ledger's rows whose step is `step`.
    fn details(&self, step: &str) -> Vec<String> {
        ledger_details(&self.opts, step)
    }

    fn typed(&self) -> Vec<String> {
        self.asked.lock().map_or_else(
            |_| Vec::new(),
            |a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect(),
        )
    }
}

#[cfg(unix)]
impl Drop for Parked {
    fn drop(&mut self) {
        if let Some(agent) = &mut self.agent {
            let _ = agent.kill();
            let _ = agent.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The details of `opts`' ledger rows whose step is `step`.
fn ledger_details(opts: &Opts, step: &str) -> Vec<String> {
    std::fs::read_to_string(state_dir(opts).join("ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| aterm_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("step").and_then(Value::as_str) == Some(step))
        .filter_map(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// A zsh that is the session leader of a pseudo-terminal of its own and its
/// foreground process group (`pgid == tpgid`): the shell a relaunch waits to
/// hold its terminal again. Killed and reaped by the caller.
#[cfg(unix)]
fn tty_zsh() -> std::process::Child {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    // Both ends close-on-exec from birth: the zsh below holds its terminal,
    // never a copy of its master, and no child another test spawns holds either.
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned here
    // alone; the master lives as long as the test process (leaked below) so
    // the terminal never hangs up under the shell.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    // `std`'s, for the hook below (the one stand-in that must fork), with the
    // strip installed first: like every other child here it holds its stdio
    // and nothing else (this file's `Command` import says why).
    let mut zsh = std::process::Command::new("/bin/zsh");
    zsh.inherit_nothing()
        .args(["-f", "-c", "sleep 60; :"])
        .stdin(tty(&slave))
        .stdout(tty(&slave))
        .stderr(tty(&slave));
    // SAFETY: async-signal-safe calls only, between fork and exec: a new
    // session, and the terminal on stdin made its controlling terminal.
    unsafe {
        zsh.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = zsh.spawn().expect("zsh");
    std::mem::forget(master);
    drop(slave);
    // Until zsh runs as the session's leader, its prompt is not "back".
    for _ in 0..6_000 {
        if ids(child.id())
            .is_some_and(|(_, pgid, tpgid)| i64::from(child.id()) == pgid && pgid == tpgid)
            && atpkg::caller_shell::process_args(child.id())
                .is_some_and(|a| a.exec_path.ends_with("zsh"))
        {
            return child;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("zsh never held its terminal");
}

/// A stand-in agent ([`parked`]) that leads a terminal of its own, as Claude Code leads its
/// tab: a new session on a pty, its group the terminal's foreground (`pgid == tpgid ==
/// pid`), so a roster naming that group (`who … fgpgid=<pid>`) claims it
/// ([`host_candidates`]). It names [`TAB`] and no login or Claude home but `env`'s, and
/// runs in `cwd` (`None`: the test's own). Killed and reaped by the caller.
#[cfg(unix)]
fn tty_agent(env: &[(&str, &Path)], cwd: Option<&Path>) -> std::process::Child {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned here
    // alone; the master lives as long as the test process (leaked below) so
    // the terminal never hangs up under the agent.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    // `std`'s, for the hook below, with the strip installed first ([`tty_zsh`]).
    let mut agent = std::process::Command::new(std::env::current_exe().expect("exe"));
    agent
        .inherit_nothing()
        .args(PARK)
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .env_remove(crate::harness::accounts::LOGIN_ENV)
        .env_remove(crate::harness::accounts::HOME_ENV)
        // An unset Claude home is the process's own `HOME`'s: a stand-in's is
        // the sweep's user home unless a test names one.
        .env_remove("HOME")
        .stdin(tty(&slave))
        .stdout(tty(&slave))
        .stderr(tty(&slave));
    for (key, value) in env {
        agent.env(key, value);
    }
    if let Some(cwd) = cwd {
        agent.current_dir(cwd);
    }
    // SAFETY: async-signal-safe calls only, between fork and exec: a new
    // session, and the terminal on stdin made its controlling terminal.
    unsafe {
        agent.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = agent.spawn().expect("agent");
    std::mem::forget(master);
    drop(slave);
    let me = std::env::current_exe().expect("exe");
    for _ in 0..6_000 {
        if ids(child.id())
            .is_some_and(|(_, pgid, tpgid)| i64::from(child.id()) == pgid && pgid == tpgid)
            && atpkg::caller_shell::process_args(child.id())
                .is_some_and(|a| Path::new(&a.exec_path).file_name() == me.file_name())
        {
            return child;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("the stand-in agent never held its terminal");
}

/// A pending state for [`SESSION`] whose screen ([`idle_screen`], seq 77) has
/// just been seen to move.
fn unsettled() -> St {
    St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 76,
        seq_since_s: now_s(),
        salt: 1,
        // Found behind just now: the ladder's first rung, where the natural
        // idle point is asked for (a person within `human_grace_s`, the full
        // settle). With no start recorded the salt (1, 1970) would stand for
        // it, and every rig would stand at the last rung.
        pending_since: now_s(),
        ..St::default()
    }
}

/// [`unsettled`], its screen (seq 77) unchanged for an hour, in [`TAB`]: a
/// settled idle point the notice may be typed at.
fn settled() -> St {
    St {
        last_seq: 77,
        seq_since_s: now_s() - 3600,
        tab: TAB.to_string(),
        ..unsettled()
    }
}

/// The screen [`idle_screen`] shows, with the server's person stamp `human`
/// (`"human_ms"`'s value; `None`: a host that sends none).
#[cfg(unix)]
fn stamped_screen(human: Option<&str>) -> String {
    let rule = "─".repeat(20);
    let stamp = human.map_or_else(String::new, |h| format!(r#","human_ms":{h}"#));
    format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77{stamp},"first":0}}"#
    )
}

/// The presence guard: a tab is attended when the server's per-session stamp (`human_ms`)
/// on the read the sweep judges shows input within `human_grace_s`, whatever the tab's
/// placement. Said once (`held-back:attended`), then `wait:attended`; no stamp fails
/// closed; `--now` types all the same. Negative controls: a person long gone, or `null`.
#[cfg(unix)]
#[test]
fn an_attended_tab_has_a_person_at_it_and_is_said_once() {
    let settled = settled();
    let front = "meta=0 window=1 active=1 wfocus=1";
    let rig = |name: &str, human: Option<&str>| {
        Parked::new(
            name,
            Answers {
                row: front,
                screen: stamped_screen(human),
                ..Answers::default()
            },
            settled.clone(),
            3600,
        )
    };
    // A person here thirty seconds ago: said once, then waited on quietly.
    let h = rig("attended", Some("30000"));
    assert_eq!(h.visit().step, "held-back:attended");
    assert_eq!(h.visit().step, "wait:attended");
    assert_eq!(h.visit().step, "wait:attended");
    assert!(h.typed().is_empty());
    assert_eq!(h.details("held-back:attended").len(), 1, "said once");
    drop(h);
    let h = rig("attended-unstamped", None);
    assert_eq!(h.visit().step, "held-back:attended", "fails closed");
    drop(h);
    // The owner's word: typed all the same.
    let h = rig("attended-now", Some("30000"));
    ask(&h.opts, TAB, Ask::Now, false).expect("--now");
    assert_eq!(h.visit().step, "announced:1");
    drop(h);
    // Negative controls: in front of the focused window, but nobody keyed it.
    for (case, human) in [("person-away", "660000"), ("never", "null")] {
        let h = rig(&format!("unattended-{case}"), Some(human));
        assert_eq!(h.visit().step, "announced:1", "{case}");
    }
}

/// The notice is pressed key by key (`typed=1 cadence=2 cap=4096`) where the host's `turn`
/// takes `cap=`: Claude Code wraps a long paste in `<pasted_content>`, which the model may
/// decline to act on. Negative control: a host without `cap=` gets the plain paste; either
/// way the fence and the yield stay.
#[cfg(unix)]
#[test]
fn the_notice_is_pressed_where_the_host_takes_cap_and_pasted_where_it_does_not() {
    let rule = "─".repeat(20);
    let screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let with_cap = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    let without = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    for (name, help, pressed) in [
        ("notice-pressed", with_cap, true),
        ("notice-pasted", without, false),
    ] {
        let h = Parked::new(
            name,
            Answers {
                screen: screen.clone(),
                help,
                turn: vec!["OK 0 turn skipped reason=changed submitted=0 seq=78 id=1"],
                ..Answers::default()
            },
            settled(),
            3600,
        );
        let _ = h.visit();
        let typed = h.typed();
        assert!(!typed.is_empty(), "{name}: the notice was tried");
        for t in &typed {
            assert_eq!(
                t.contains(" typed=1 cadence=2 cap=4096 "),
                pressed,
                "{name}: {t}"
            );
            assert!(
                t.contains(" turn if-gen=4.77 ") && t.contains(" yield=0.2 "),
                "{name}: the fence and the yield stay: {t}"
            );
            assert!(t.contains("[aterm harness]"), "{name}: {t}");
        }
    }
}

/// A pressed notice leaves Claude Code's open session survey to the person (its first key
/// would close the card unanswered): nothing is typed, refused `survey` until a quiet
/// moment. Negative control: the same screen with no card presses it.
#[cfg(unix)]
#[test]
fn a_pressed_notice_leaves_an_open_survey_to_the_person() {
    let rule = "─".repeat(80);
    let survey = [
        "● How is Claude doing this session? (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
        "",
    ];
    let with_cap = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    for (name, card) in [("survey-open", true), ("survey-none", false)] {
        let mut rows: Vec<String> = if card {
            survey.iter().map(|r| (*r).to_string()).collect()
        } else {
            vec![String::new(); 3]
        };
        rows.extend([rule.clone(), "❯ ".to_string(), rule.clone()]);
        let screen = format!(
            r#"{{"rows":{},"cursor":{{"row":4,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#,
            aterm_json::to_string(&rows).expect("rows")
        );
        let h = Parked::new(
            name,
            Answers {
                screen,
                help: with_cap,
                turn: vec!["OK 0 turn skipped reason=changed submitted=0 seq=78 id=1"],
                ..Answers::default()
            },
            settled(),
            3600,
        );
        let r = h.visit();
        let typed = h.typed();
        if card {
            assert!(typed.is_empty(), "{name}: nothing typed: {typed:?}");
            assert!(r.step.contains("survey"), "{name}: {r:?}");
        } else {
            assert!(
                typed.iter().any(|t| t.contains(" typed=1 cadence=2 ")),
                "{name}: the notice is pressed: {typed:?}"
            );
        }
    }
}

/// Only the notice is pressed key by key: the relaunch line (a person's Enter mid-line
/// would run a bare `claude`) and the continuation go as one fenced paste even where the
/// host takes `cap=`. Negative control: the notice's own turn on the same host is pressed.
#[cfg(unix)]
#[test]
fn only_the_agents_own_words_are_pressed_where_the_host_takes_cap() {
    const SETTLED: &str = "OK 0 id=2 submitted=1 status=settled";
    let answers = || Answers {
        help: CAP_HELP,
        ..fenced_answers(vec![SETTLED])
    };
    // The relaunch line at a shell that is not bash, then the notice.
    let dir = scratch("pressed-only-notice");
    let (sock, asked) = instance_with(&dir, answers());
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let line = " '/pkg/claude' '--resume' 'x'";
    let got = type_relaunch_line_with(
        &opts,
        &mut c,
        4242,
        TAB,
        line,
        None,
        PromptLook {
            mark: &mut None,
            ended_at_s: 0,
        },
        || true,
        |_, _, _| true,
    );
    assert_eq!(got, Ok(()));
    let generation = typing_fence(&mut c, TAB).expect("empty composer");
    notice_fenced(&mut c, TAB, "[aterm harness] notice", generation.as_deref()).expect("typed");
    drop(c);
    let turns: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert!(
        turns[0].contains(" turn if-gen=4.77 yield=0.2 ") && turns[0].ends_with(line),
        "the relaunch line is one fenced paste: {turns:#?}"
    );
    assert!(
        turns[1].contains(" turn if-gen=4.77 typed=1 cadence=2 cap=4096 yield=0.2 "),
        "the notice is pressed: {turns:#?}"
    );
    // The continuation.
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::with("pressed-not-continue", &before, answers());
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(
        typed[0].contains(" turn if-gen=4.77 yield=0.2 "),
        "the continuation is one fenced paste: {typed:#?}"
    );
}

/// A pressed notice's turn budget covers its keys and its digits: the host holds each
/// digit for the previous key's echo (about one render), so a notice can outrun
/// [`TURN_WAIT`]'s 10 s and be cut part-way. The real notice asks its keys at the cadence
/// plus its digits' renders on top of `TURN_WAIT`. Negative control: a paste waits
/// `TURN_WAIT` exactly.
#[cfg(unix)]
#[test]
fn a_pressed_notices_turn_budget_covers_its_keys_and_its_digits() {
    const SETTLED: &str = "OK 0 id=2 submitted=1 status=settled";
    let v = |s: &str| Version::parse(s).expect("version");
    let held: Vec<upgrade::Held> = (0..5u32)
        .map(|i| upgrade::Held {
            pid: 70_100 + i,
            name: "zsh".to_string(),
            age_s: 90_061 + u64::from(i),
            command: format!("/bin/zsh -c sleep 3{i}00; make test-{i}"),
        })
        .collect();
    let notice = upgrade::with_running(
        &upgrade::prepare_prompt_with_model(
            &v("2.1.290"),
            &v("2.1.291"),
            Source::Managed,
            &upgrade::ready_marker(SESSION, &v("2.1.291"), 1),
            None,
            None,
        ),
        &upgrade::running_clause(&held),
    );
    let dir = scratch("pressed-budget");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            help: CAP_HELP,
            ..fenced_answers(vec![SETTLED])
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    turn_fenced(&mut c, TAB, "'/pkg/claude' '--resume' 'x'", None).expect("pasted");
    notice_fenced(&mut c, TAB, &notice, None).expect("pressed");
    drop(c);
    let turns: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(turns.len(), 2, "{turns:#?}");
    assert!(
        turns[0].contains(&format!(" yield=0.2 {TURN_WAIT} ")),
        "the paste waits TURN_WAIT: {turns:#?}"
    );
    let count = |n: usize| u64::try_from(n).expect("count");
    let keys = count(notice.chars().count());
    let digits = count(notice.chars().filter(char::is_ascii_digit).count());
    assert!(digits >= 40, "a notice carries dozens of digits: {digits}");
    assert!(
        turns[1].contains(" typed=1 cadence=2 cap=4096 yield=0.2 idle=1 timeout="),
        "{}",
        turns[1]
    );
    let timeout: u64 = turns[1]
        .split_whitespace()
        .find_map(|w| w.strip_prefix("timeout="))
        .and_then(|t| t.parse().ok())
        .expect("a timeout");
    assert!(
        timeout >= keys * 2 + digits * 150 + 10_000,
        "timeout={timeout} keys={keys} digits={digits}"
    );
}

/// A notice the host cut short (`skipped reason=<why> typed=<k>`, [`Typed::Cut`]) is never
/// announced (the agent has half a notice, no READY sentence) nor typed again in the
/// visit. A skip that typed nothing keeps its word: `changed` is retried, a spent budget is
/// a wait (`yield`). Negative controls: `changed` is tried [`NOTICE_FENCE_TRIES`] times; a
/// whole notice is announced.
#[cfg(unix)]
#[test]
fn a_notice_the_host_cut_short_is_never_announced_or_typed_again() {
    let cut = "OK 0 turn skipped reason=person typed=40 pasted=0 submitted=0 seq=78 id=1";
    for (head, want) in [
        (cut, Err(Typed::Cut("person".to_string()))),
        ("OKAY", Err(Typed::Refused("OKAY".to_string()))),
        (
            "OK 0 turn skipped reason=deadline typed=12 pasted=0 submitted=0 seq=9 id=3",
            Err(Typed::Cut("deadline".to_string())),
        ),
        (
            "OK 0 turn skipped reason=changed typed=5 pasted=0 submitted=0 seq=9 id=3",
            Err(Typed::Cut("changed".to_string())),
        ),
        (
            "OK 0 turn skipped reason=deadline typed=0 pasted=0 submitted=0 seq=9 id=3",
            Err(Typed::Yielded),
        ),
        (
            "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
            Err(Typed::Changed),
        ),
        (
            "OK 0 turn submitted=1 status=settled seq=9 id=3 dur_ms=1 hash=0 typed=40 pasted=0",
            Ok(()),
        ),
    ] {
        assert_eq!(turn_verdict(head), want, "{head}");
    }
    assert_eq!(Typed::Cut("person".to_string()).word(), "cut-person");
    let rule = "─".repeat(20);
    let screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let with_cap = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                    [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    let visit = |name: &str, reply: &'static str| {
        let h = Parked::new(
            name,
            Answers {
                screen: screen.clone(),
                help: with_cap,
                turn: vec![reply],
                ..Answers::default()
            },
            settled(),
            3600,
        );
        let step = h.visit().step;
        let phase = load(&h.opts, SESSION).expect("the record").phase;
        (step, phase, h.typed().len())
    };
    let (step, phase, typed) = visit("notice-cut", cut);
    assert_eq!(step, "wait:announce-refused:cut-person");
    assert_eq!(phase, Phase::Pending, "not announced");
    assert_eq!(typed, 1, "not typed again over its own half");
    // Negative controls.
    let (step, _, typed) = visit(
        "notice-cut-changed",
        "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
    );
    assert_eq!(step, "wait:announce-refused:changed");
    assert_eq!(typed, NOTICE_FENCE_TRIES, "a moved screen is tried again");
    let (step, _, _) = visit(
        "notice-cut-whole",
        "OK 0 turn submitted=1 status=settled seq=78 id=1 dur_ms=2 hash=0 typed=900 pasted=0",
    );
    assert_eq!(step, "announced:1");
}

/// A labelled top rule (an effort tag, `sandboxed`) is the input box too: `aterm_phase`'s
/// frame reader fences the recorded screens (`wait_bg.out`, the footer fixture) on their
/// three rows, and the notice's `turn` carries `if-rows=`, no `if-gen=`. Negative control:
/// an all-`─` test refuses both recorded top rules.
#[cfg(unix)]
#[test]
fn a_labelled_top_rule_is_the_input_box_the_notice_fences_on() {
    use aterm_types::control_verbs::RowsFence;
    let old_rule = |row: &str| {
        let t = row.trim();
        t.chars().count() >= 8 && t.chars().all(|ch| ch == '─')
    };
    for (name, text) in [
        (
            "wait_bg",
            include_str!("../../../aterm-phase/src/fixtures/wait_bg.out"),
        ),
        (
            "footer-auto-idle",
            include_str!("../../../aterm-phase/src/fixtures/claude-2.1.283-footer-auto-idle.txt"),
        ),
    ] {
        let rows: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('❯'))
            .unwrap_or_else(|| panic!("{name}: a caret row"));
        let scr = Screen {
            rows: rows.clone(),
            cursor: Some((caret, 2)),
            seq: 1,
            first: 0,
            generation: Some("4.77".to_string()),
            human: HumanInput::Unknown,
        };
        let fence = composer_rows(&scr).unwrap_or_else(|| panic!("{name}: the box"));
        assert_eq!((fence.first, fence.last), (caret - 1, caret + 1), "{name}");
        let three = rows[caret - 1..=caret + 1].iter().map(String::as_str);
        assert_eq!(fence.hash, RowsFence::hash_rows(three), "{name}");
        assert!(
            rows[caret - 1].trim_end().ends_with(" ─") && !old_rule(&rows[caret - 1]),
            "{name}: labelled, and refused by the old test: {}",
            rows[caret - 1]
        );
        // The cursor off the caret row is no box of ours.
        let off = Screen {
            cursor: Some((caret - 1, 0)),
            ..scr
        };
        assert!(composer_rows(&off).is_none(), "{name}");
    }
    // Through the fence's own read, on a host that takes `if-rows=`.
    let top = format!("{} sandboxed ─", "─".repeat(20));
    let rule = "─".repeat(33);
    let dir = scratch("fence-rows-labelled");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            screen: format!(
                r#"{{"rows":["✻ Waiting for 1 dynamic workflow to finish","{top}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
            ),
            help: ROWS_HELP,
            turn: vec!["OK 0 id=2 submitted=1 status=settled"],
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let (fence, _) = claude_fence(&mut c, TAB).expect("empty composer");
    notice_fenced(&mut c, TAB, "[aterm harness] hi", fence.as_deref()).expect("typed");
    drop(c);
    let line = asked
        .lock()
        .expect("log")
        .iter()
        .find(|l| l.split_whitespace().find(|w| !w.starts_with('@')) == Some("turn"))
        .cloned()
        .expect("a turn was sent");
    let _ = std::fs::remove_dir_all(&dir);
    let want = RowsFence {
        first: 1,
        last: 3,
        hash: RowsFence::hash_rows([top.as_str(), "❯ ", rule.as_str()]),
    };
    assert!(line.contains(&format!("if-rows={want} ")), "{line}");
    assert!(!line.contains("if-gen="), "{line}");
}

/// A host's `help` that names every option these rigs send: the guarded
/// submit, the key-by-key `cap=`, the generation fence — and a `key` entry
/// naming its fences, as the take-back asks ([`key_takes_gen`]).
#[cfg(unix)]
const PRESS_HELP: &str = "OK 3\nturn [option=value ...] <text>: ... [submit=<key|none|guarded:<re>>] \
                          [cadence=<ms>] [cap=<n>] [yield=<floor>] [if-gen=<epoch>.<seq>] ...\n\
                          key <name>: one key\n    FENCES (key [if-gen=<epoch>.<seq>] \
                          [if-human=<n>] <name>) ...";

/// The row a composer's text ends on, word-wrapped at `width` columns as
/// Claude Code wraps it: between words, a word longer than a row broken
/// inside it, continuation rows indented two. It stands in for Codex's
/// composer too (the Codex lane's tests), assumed to wrap the same way.
#[cfg(unix)]
pub(super) fn last_wrapped_row(text: &str, width: usize) -> String {
    const INDENT: &str = "  ";
    let len = |s: &str| s.chars().count();
    let mut row = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        loop {
            let fresh = row.is_empty() || row == INDENT;
            let need = len(&word) + usize::from(!fresh);
            if len(&row) + need <= width {
                if !fresh {
                    row.push(' ');
                }
                row.push_str(&word);
                break;
            }
            if fresh {
                let room = width - len(&row);
                row.extend(word.chars().take(room));
                word = word.chars().skip(room).collect();
            }
            row = INDENT.to_string();
        }
    }
    row
}

/// A pressed notice's Enter is guarded on its own last row (`submit=guarded:`, the
/// supervisor's composer guard): a box Claude Code draws over its input moves neither hold,
/// and a plain Enter would confirm its `1. Yes`. On the host's engine
/// (`aterm_observe::row_matcher`) the guard matches the notice's and release's last row at
/// every width and no row of the box; a miss is a cut. Negative control: a host whose help
/// does not name the option gets the plain Enter.
#[cfg(unix)]
#[test]
fn a_pressed_notices_enter_is_guarded_on_its_own_last_row() {
    let v = |s: &str| Version::parse(s).expect("version");
    let marker = upgrade::ready_marker(SESSION, &v("2.1.291"), 1);
    let notice = upgrade::prepare_prompt_with_model(
        &v("2.1.290"),
        &v("2.1.291"),
        Source::Managed,
        &marker,
        None,
        None,
    );
    let release = upgrade::release_prompt(upgrade::Agent::Claude, false);
    let the_box = [
        " Do you want to proceed?",
        "❯ 1. Yes",
        "  2. Yes, and don't ask again for this command",
        "  3. No, and tell Claude what to do differently (esc)",
    ];
    for text in [&notice, &release] {
        let guard = aterm_observe::row_matcher(&composer_guard('❯', text))
            .expect("the host compiles the guard");
        for width in 30..=160 {
            let last = last_wrapped_row(&format!("❯ {text}"), width);
            assert!(guard.matches(&last), "width {width}: {last:?}");
        }
        for row in the_box {
            assert!(!guard.matches(row), "{row}");
        }
    }
    let rule = "─".repeat(20);
    let screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let rig = |name: &str, help: &'static str, reply: &'static str| {
        let h = Parked::new(
            name,
            Answers {
                screen: screen.clone(),
                help,
                turn: vec![reply],
                ..Answers::default()
            },
            settled(),
            3600,
        );
        let step = h.visit().step;
        let phase = load(&h.opts, SESSION).expect("the record").phase;
        (step, phase, h.typed())
    };
    const WHOLE: &str =
        "OK 0 turn submitted=1 status=settled seq=78 id=1 dur_ms=2 hash=0 typed=900 pasted=0";
    let (step, _, typed) = rig("guarded-notice", PRESS_HELP, WHOLE);
    assert_eq!(step, "announced:1");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    let text = typed[0]
        .find(" [aterm harness]")
        .map(|at| &typed[0][at + 1..])
        .expect("the notice");
    assert!(
        typed[0].contains(&format!(
            " typed=1 cadence=2 cap=4096 submit=guarded:{} yield=0.2 ",
            composer_guard('❯', text)
        )),
        "{}",
        typed[0]
    );
    // A guard's miss: typed whole, never sent — a cut, not announced.
    let (step, phase, typed) = rig(
        "guarded-notice-missed",
        PRESS_HELP,
        "OK 0 turn skipped reason=guard submitted=0 seq=78 id=1",
    );
    assert_eq!(step, "wait:announce-refused:cut-guard");
    assert_eq!(phase, Phase::Pending, "not announced");
    assert_eq!(typed.len(), 1, "not typed again over its own words");
    // Negative control: a host without the option, the plain Enter.
    const CAP_ONLY: &str = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                            [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
    let (step, _, typed) = rig("unguarded-notice", CAP_ONLY, WHOLE);
    assert_eq!(step, "announced:1");
    assert!(
        typed[0].contains(" typed=1 cadence=2 cap=4096 yield=0.2 ")
            && !typed[0].contains("submit="),
        "{}",
        typed[0]
    );
}

/// What a stand-in prompt's `ctrl+u` takes back ([`prompt_tab`]).
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kill {
    /// The whole line.
    Line,
    /// The row the cursor is on, as a composer that kills a wrapped row at a
    /// time would.
    Row,
    /// Nothing.
    Nothing,
}

/// A Claude Code prompt for [`left_standing`] alone, 40 columns wide, holding `draft`
/// ([`Prompt::screen`]): `text --json` draws it, a `cell` reads plain, `help` answers `help`, and
/// `key … ctrl+u` (only while its `if-gen=` names the screen drawn) takes back what `kill` says.
/// Returns every request it read and the draft it was left holding.
#[cfg(unix)]
fn prompt_tab(
    draft: &str,
    kill: Kill,
    help: &'static str,
) -> (Client, std::thread::JoinHandle<(Vec<String>, String)>) {
    use std::io::BufRead as _;
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let mut prompt = Prompt {
        draft: draft.to_string(),
        person_key: false,
    };
    let server = std::thread::spawn(move || {
        let mut out = theirs.try_clone().expect("clone");
        let mut asked = Vec::new();
        let mut generation = 77;
        for line in std::io::BufReader::new(theirs).lines() {
            let Ok(line) = line else { break };
            let verb = line
                .split_whitespace()
                .find(|w| !w.starts_with('@'))
                .unwrap_or("");
            let reply = match verb {
                "text" => format!("OK {}", prompt.screen(generation)),
                "cell" => "OK %20 d0d0d0 111318 none".to_string(),
                "help" => help.to_string(),
                "key" if !line.contains(&format!(" if-gen=4.{generation} ")) => {
                    format!("OK skipped reason=changed seq={generation}")
                }
                "key" => {
                    let n = prompt.draft.chars().count();
                    let keep = match kill {
                        Kill::Line => 0,
                        Kill::Row => n.saturating_sub(1) / 38 * 38,
                        Kill::Nothing => n,
                    };
                    prompt.draft = prompt.draft.chars().take(keep).collect();
                    generation += 1;
                    format!("OK seq={generation}")
                }
                _ => "ERR unscripted".to_string(),
            };
            asked.push(line);
            if writeln!(out, "{reply}").is_err() {
                break;
            }
        }
        (asked, prompt.draft)
    });
    (RelayClient::new(ours), server)
}

/// Aterm never takes its own words back (a `ctrl+u` fenced on the read cannot see a key
/// the host counted but Claude Code has not drawn): [`left_standing`] only reads what a
/// stopped notice left, `unsent` alone, `mixed` with a person's key, else `Gone`; nothing
/// is pressed.
#[cfg(unix)]
#[test]
fn aterms_own_words_are_read_never_taken_back() {
    let release = upgrade::release_prompt(upgrade::Agent::Claude, false);
    let part: String = release.chars().take(150).collect();
    let presses = |asked: &[String]| asked.iter().filter(|l| l.contains(" key ")).count();
    for (draft, want) in [
        (part.clone(), Left::Kept("unsent")),
        (format!("{part}x"), Left::Kept("mixed")),
        ("push it as soon as the gates pass".to_string(), Left::Gone),
        (String::new(), Left::Gone),
    ] {
        for kill in [Kill::Line, Kill::Row, Kill::Nothing] {
            let (mut c, server) = prompt_tab(&draft, kill, PRESS_HELP);
            assert_eq!(left_standing(&mut c, TAB, &release), want, "{draft:?}");
            drop(c);
            let (asked, left) = server.join().expect("server");
            assert_eq!(
                (presses(&asked), left),
                (0, draft.clone()),
                "nothing pressed"
            );
        }
    }
}

/// A notice the host cut short is recorded (`St::left_typed`) and read first at every
/// later look as aterm's own (`wait:left-typed:unsent`), never `draft`, so the
/// supervisor's draft rule never sends it; nothing is pressed to take it back. A person's
/// key among it makes it theirs (`mixed`). Negative control: the record forgotten, `draft`.
#[cfg(unix)]
#[test]
fn a_notice_the_host_stopped_stands_unsent_and_is_never_pressed_back() {
    let rig = |name: &str, reply: &'static str, person_key: bool| {
        Parked::new(
            name,
            Answers {
                help: PRESS_HELP,
                turn: vec![reply],
                prompt: Some(Prompt {
                    draft: String::new(),
                    person_key,
                }),
                ..Answers::default()
            },
            settled(),
            3600,
        )
    };
    let keys = |h: &Parked| {
        h.asked
            .lock()
            .map_or(0, |a| a.iter().filter(|l| l.ends_with(" ctrl+u")).count())
    };
    let person = "OK 0 turn skipped reason=person typed=40 pasted=0 submitted=0 seq=78 id=1";
    for (name, reply, word) in [
        (
            "cut-deadline",
            "OK 0 turn skipped reason=deadline typed=300 pasted=0 submitted=0 seq=78 id=1",
            "wait:announce-refused:cut-deadline",
        ),
        (
            "cut-guard",
            "OK 0 turn skipped reason=guard submitted=0 seq=78 id=1",
            "wait:announce-refused:cut-guard",
        ),
        ("cut-person", person, "wait:announce-refused:cut-person"),
    ] {
        let h = rig(name, reply, false);
        assert_eq!(h.visit().step, word, "{name}");
        let st = load(&h.opts, SESSION).expect("the record");
        assert!(
            st.left_typed.starts_with(upgrade::ANNOUNCE_HEAD),
            "{name}: {st:?}"
        );
        // The next look reads aterm's own words first — never `draft`.
        assert_eq!(h.visit().step, "wait:left-typed:unsent", "{name}");
        assert_eq!(keys(&h), 0, "{name}: nothing pressed, ever");
        assert!(h.details("left-typed-cleared").is_empty(), "{name}");
        assert_eq!(h.typed().len(), 1, "{name}: typed once, never again");
    }
    // A person's key among aterm's words: theirs, pressed nothing.
    let h = rig("cut-mixed", person, true);
    let _ = h.visit();
    assert_eq!(h.visit().step, "wait:left-typed:mixed");
    assert_eq!(keys(&h), 0);
    drop(h);
    // Negative control: the record forgotten, aterm's words read as a person's draft.
    let h = rig("cut-forgotten", person, false);
    let _ = h.visit();
    let mut st = load(&h.opts, SESSION).expect("the record");
    st.left_typed.clear();
    save(&h.opts, SESSION, &st);
    assert_eq!(h.visit().step, "wait:draft");
    assert_eq!(keys(&h), 0);
}

/// A turn begun since the look takes no notice and no release under the rows fence: that
/// fence (`if-rows=`, [`claude_fence`]) covers only the input box, so its fresh read is
/// held to the look's own turn fact ([`busy_footer`]): `wait:busy` / `wait:release:busy`,
/// nothing typed. Negative controls: a screen that stayed idle types both.
#[cfg(unix)]
#[test]
fn a_turn_begun_since_the_look_takes_no_notice_and_no_release_under_the_rows_fence() {
    let rule = "─".repeat(20);
    let idle = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let busy = format!(
        r#"{{"rows":["✻ Pondering… (3s · esc to interrupt)","{rule}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":79,"first":0,"gen":"4.79","human_ms":null}}"#
    );
    let answers = |after: Option<(usize, String)>| Answers {
        screen: idle.clone(),
        after,
        help: ROWS_HELP,
        turn: vec!["OK 0 id=2 submitted=1 status=settled"],
        ..Answers::default()
    };
    let text_reads = |h: &Parked| {
        h.asked
            .lock()
            .expect("asked")
            .iter()
            .take_while(|l| !l.contains(" turn "))
            .filter(|l| l.contains(" text "))
            .count()
    };
    // The notice (the ask flow's): typed over a screen that stayed idle, on the box's rows.
    let h = Parked::new("rows-turn-idle", answers(None), settled(), 3600);
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(h.typed().len(), 1);
    assert!(h.typed()[0].contains(" if-rows="), "{:?}", h.typed());
    // The look's reads, then the fence's two (its `cell` asked between them).
    let look = text_reads(&h) - 2;
    drop(h);
    let h = Parked::new(
        "rows-turn-begun",
        answers(Some((look, busy.clone()))),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "wait:busy", "typed: {:?}", h.typed());
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    assert_eq!(load(&h.opts, SESSION).expect("state").phase, Phase::Pending);
    drop(h);
    // The release a give-up owes, under the same fence.
    let owing = |h: &Parked| {
        let st = St {
            phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
            release: "gave-up".to_string(),
            failed_at: now_s(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            ..settled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    };
    let h = Parked::new("rows-release-idle", answers(None), settled(), 3600);
    owing(&h);
    assert_eq!(h.visit().step, "released:gave-up");
    assert_eq!(h.typed().len(), 1);
    let look = text_reads(&h) - 2;
    drop(h);
    let h = Parked::new(
        "rows-release-begun",
        answers(Some((look, busy))),
        settled(),
        3600,
    );
    owing(&h);
    assert_eq!(
        h.visit().step,
        "wait:release:busy",
        "typed: {:?}",
        h.typed()
    );
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    assert_eq!(
        load(&h.opts, SESSION).expect("state").release,
        "gave-up",
        "still owed"
    );
}

/// The notice is fenced on the generation it was judged on (`if-gen=<gen> yield=0.2` where
/// `help turn` names it): a moment's repaint is typed at the next of
/// [`NOTICE_FENCE_TRIES`] fresh tries, one that outlasts them waits pending (counted in
/// `St::refused`), and a person typing through the park is not typed at again. Negative
/// control: a host without the fence gets the plain turn.
#[cfg(unix)]
#[test]
fn the_notice_is_fenced_on_the_generation_it_was_judged_on() {
    let rule = "─".repeat(20);
    let fenced_screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let help = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...";
    let answers = |turn| Answers {
        screen: fenced_screen.clone(),
        help,
        turn,
        ..Answers::default()
    };
    // The host moved: nothing typed, still pending.
    let h = Parked::new(
        "fence-changed",
        answers(vec![
            "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
        ]),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "wait:announce-refused:changed");
    let st = load(&h.opts, SESSION).expect("the record");
    assert_eq!(st.phase, Phase::Pending);
    assert_eq!(st.refused, 1, "one look refused");
    assert_ne!(st.refused_at, 0, "since when");
    let typed = h.typed();
    assert_eq!(typed.len(), NOTICE_FENCE_TRIES, "{typed:#?}");
    assert!(
        typed.iter().all(|t| t.contains(&format!(
            " turn if-gen=4.77 yield=0.2 {TURN_WAIT} [aterm harness]"
        ))),
        "{typed:?}"
    );
    // How many screen reads come before the first paste: the look's and the first
    // fence's. A later screen after them is what a retry reads.
    let reads = {
        let asked = h.asked.lock().expect("asked").clone();
        asked
            .iter()
            .take_while(|l| !l.contains(" turn "))
            .filter(|l| l.contains(" text "))
            .count()
    };
    assert!(reads >= 2, "the look's read and the fence's: {reads}");
    // The next look that cannot type it counts on, from the same first one.
    let first = st.refused_at;
    assert_eq!(h.visit().step, "wait:announce-refused:changed");
    let st = load(&h.opts, SESSION).expect("the record");
    assert_eq!((st.refused, st.refused_at), (2, first));
    drop(h);
    // A retry types only where the look's gates still hold: a person's input since the
    // look keeps the refusal, and a turn the agent woke into waits `busy`; either way
    // nothing is typed again.
    let busy_screen = format!(
        r#"{{"rows":["✻ Pondering… (3s · esc to interrupt)","{rule}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":79,"first":0,"gen":"4.79","human_ms":null}}"#
    );
    for (case, later, word, refused) in [
        (
            "hand",
            fenced_screen.replace(r#""human_ms":null"#, r#""human_ms":40"#),
            "wait:announce-refused:changed",
            1,
        ),
        ("turn", busy_screen, "wait:busy", 0),
    ] {
        let h = Parked::new(
            &format!("fence-retry-{case}"),
            Answers {
                after: Some((reads, later)),
                ..answers(vec![
                    "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
                    "OK 0 id=2 submitted=1 status=settled",
                ])
            },
            settled(),
            3600,
        );
        assert_eq!(h.visit().step, word, "{case}: never typed again");
        assert_eq!(h.typed().len(), 1, "{case}: typed once, never again");
        let st = load(&h.opts, SESSION).expect("the record");
        assert_eq!((st.phase, st.refused), (Phase::Pending, refused), "{case}");
        drop(h);
    }
    // Negative control: the same retry over a screen that only repainted (the
    // look's gates hold) is typed.
    let h = Parked::new(
        "fence-retry-repaint",
        Answers {
            after: Some((reads, fenced_screen.replace("4.77", "4.78"))),
            ..answers(vec![
                "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
                "OK 0 id=2 submitted=1 status=settled",
            ])
        },
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(h.typed().len(), 2);
    assert!(
        h.typed()[1].contains(" turn if-gen=4.78 "),
        "{:?}",
        h.typed()
    );
    drop(h);
    // A moment's repaint: typed at the next try, within the same visit.
    let h = Parked::new(
        "fence-changed-once",
        answers(vec![
            "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1",
            "OK 0 id=2 submitted=1 status=settled",
        ]),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(h.typed().len(), 2);
    let st = load(&h.opts, SESSION).expect("the record");
    assert_eq!(
        (st.refused, st.refused_at),
        (0, 0),
        "typed: nothing refused"
    );
    drop(h);
    // A person typing through the park: never typed at again.
    let h = Parked::new(
        "fence-yield",
        answers(vec!["ERR yield timeout momentum=0.84"]),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "wait:announce-refused:yield");
    assert_eq!(h.typed().len(), 1);
    drop(h);
    // Taken.
    let h = Parked::new(
        "fence-taken",
        answers(vec!["OK 0 id=2 submitted=1 status=settled"]),
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "announced:1");
    drop(h);
    // Negative control: a host whose `turn` has no fence.
    let h = Parked::new(
        "fence-absent",
        Answers {
            screen: fenced_screen.clone(),
            ..Answers::default()
        },
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "announced:1");
    let typed = h.typed();
    assert!(!typed[0].contains("if-gen="), "{typed:?}");
}

/// The notice is typed under the harness's own hand (`lease acquire … hard`, [`Hand`]), so the
/// window shows no driver row for it, and a refused fence (a ticking footer moves the generation
/// about one try in ninety) is retried on a fresh read up to [`NOTICE_FENCE_TRIES`]. Negative
/// controls: another driver's hold waits `held`; a server with no hard lease types without a
/// hand; a fence that keeps refusing waits.
#[cfg(unix)]
#[test]
fn the_notice_is_typed_under_the_harnesss_hand_and_a_refused_fence_is_retried() {
    let rule = "─".repeat(20);
    let screen = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
    );
    let help = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...";
    let held = "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1";
    let refused = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let taken = "OK 0 id=2 submitted=1 status=settled";
    let rig = |name: &str, lease: &'static str, turn: Vec<&'static str>| {
        Parked::new(
            name,
            Answers {
                screen: screen.clone(),
                help,
                turn,
                lease,
                ..Answers::default()
            },
            settled(),
            3600,
        )
    };
    let requests = |h: &Parked| h.asked.lock().map(|a| a.clone()).unwrap_or_default();
    let order = |asked: &[String]| -> Vec<&'static str> {
        asked
            .iter()
            .filter_map(|l| {
                if l.contains(" lease acquire ") {
                    Some("acquire")
                } else if l.contains(" lease release ") {
                    Some("release")
                } else if l.contains(" turn ") {
                    Some("turn")
                } else {
                    None
                }
            })
            .collect()
    };
    // The fence refuses once and the second try lands, in one visit.
    let h = rig("hand-retry", held, vec![refused, taken]);
    assert_eq!(h.visit().step, "announced:1");
    let asked = requests(&h);
    assert_eq!(order(&asked), ["acquire", "turn", "turn", "release"]);
    assert!(
        asked
            .iter()
            .find(|l| l.contains(" lease acquire "))
            .is_some_and(|l| l.ends_with(" hard")),
        "the hard hold: {asked:?}"
    );
    drop(h);
    // A fence that keeps refusing: a wait, the hand still given back.
    let h = rig("hand-refused", held, vec![refused]);
    assert_eq!(h.visit().step, "wait:announce-refused:changed");
    let mut want = vec!["acquire"];
    want.extend(std::iter::repeat_n("turn", NOTICE_FENCE_TRIES));
    want.push("release");
    assert_eq!(order(&requests(&h)), want);
    drop(h);
    // Another driver holds the tab: nothing typed.
    let h = rig("hand-busy", "ERR busy lease=orchestrator", vec![taken]);
    assert_eq!(h.visit().step, "wait:announce-refused:held");
    assert_eq!(order(&requests(&h)), ["acquire"]);
    assert_eq!(
        load(&h.opts, SESSION).map(|st| st.phase),
        Some(Phase::Pending)
    );
    drop(h);
    // Negative control: no hard lease to take (an older server).
    let h = rig(
        "hand-older",
        "ERR lease acquire: unknown arg 'hard' (ttl=<ms> holder=<name>)",
        vec![taken],
    );
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(order(&requests(&h)), ["acquire", "turn"]);
}

/// The wait log (the ledger holds only acts): each change of wait and each act is one row
/// with the facts the look decided on; a repeat adds nothing, the log is bounded to
/// [`WAITS_KEPT`], and it survives the state file.
#[test]
fn the_wait_log_keeps_each_change_of_wait_with_the_facts_that_decided_it() {
    let mut st = St::default();
    st.note_wait("draft", 10);
    st.annotate_wait(
        10,
        "status=idle composer_empty=0 render=ok cursor=56,2 caret=55",
    );
    st.note_wait("draft", 20);
    st.annotate_wait(20, "ignored: not a change");
    st.note_wait("settling", 30);
    st.note_step("announced:1", "idle", 40);
    st.note_step("wait:background", "busy", 50);
    st.note_step("skip:not-selected", "busy", 60);
    st.note_step("wait:background", "busy", 70);
    let words: Vec<(u64, &str)> = st.waits.iter().map(|w| (w.at, w.word.as_str())).collect();
    assert_eq!(
        words,
        [
            (10, "draft"),
            (30, "settling"),
            (40, "announced:1"),
            (50, "background")
        ],
        "a change of wait or an act; a repeat and a skip add nothing"
    );
    assert!(st.waits[0].note.contains("caret=55"), "{:?}", st.waits[0]);
    assert!(st.waits[1].note.is_empty(), "no facts were attached there");
    let back = St::from_json(&st.to_json()).expect("state");
    assert_eq!(back.waits, st.waits, "it survives the state file");
    for i in 0..(WAITS_KEPT as u64 + 5) {
        st.note_wait(&format!("w{i}"), 100 + i);
    }
    assert_eq!(st.waits.len(), WAITS_KEPT, "bounded");
    assert_eq!(st.waits.last().map(|w| w.word.as_str()), Some("w28"));
}

/// Through the real visit, a draft waits `draft` and its wait-log row records what the
/// look read (person stamp, composer verdict, cursor and caret, render, holders); a repeat
/// adds no row.
#[cfg(unix)]
#[test]
fn a_visit_that_waits_records_what_it_read() {
    let rule = "─".repeat(20);
    let typed = format!(
        r#"{{"rows":["{rule}","❯ half a thought","{rule}"],"cursor":{{"row":1,"col":16}},"seq":77,"human_ms":null,"first":0}}"#
    );
    let h = Parked::new(
        "wait-log",
        Answers {
            screen: typed,
            ..Answers::default()
        },
        settled(),
        3600,
    );
    assert_eq!(h.visit().step, "wait:draft");
    let st = load(&h.opts, SESSION).expect("state");
    let row = st.waits.last().expect("a wait row");
    assert_eq!(row.word, "draft");
    for fact in [
        "status=idle",
        "attended=0",
        "composer_empty=0",
        "box=0",
        "cursor=1,16",
        "caret=1",
        "holders=0",
    ] {
        assert!(row.note.contains(fact), "{fact}: {row:?}");
    }
    // Looked at again: the same word, no new row.
    assert_eq!(h.visit().step, "wait:draft");
    assert_eq!(
        load(&h.opts, SESSION).expect("state").waits.len(),
        st.waits.len()
    );
}

/// The owner's hold owns no turn end: `--skip`ped onto its target or `--defer`red past
/// now, an upgrade waits `skipped`/`deferred` (a skip is the build's last word) and the
/// supervisor continues the worker. Negative controls: no word, `--now`, a lapsed
/// deferral, a skip of another build, or a word on another tab waits on the turn alone.
#[test]
fn an_upgrade_the_owner_holds_owns_no_turn_end() {
    let now = now_s();
    let busy = upgrade::Facts {
        status: "busy".to_string(),
        composer_empty: true,
        ..upgrade::Facts::default()
    };
    let announced = Phase::Announced {
        at_s: now - upgrade::REASK_S - 1,
        asks: 1,
    };
    let step = |phase: &Phase, request: Request, request_tab: &str| {
        let st = St {
            phase: phase.clone(),
            to: "2.1.282".to_string(),
            tab: "s-1".to_string(),
            request,
            request_tab: request_tab.to_string(),
            ..St::default()
        };
        upgrade::requested_step(&st.request_for("s-1"), &st.phase, &busy, false, now, &st.to)
    };
    for phase in [Phase::Pending, announced] {
        let skipped = step(&phase, Request::Skip("2.1.282".into()), "s-1");
        assert_eq!(skipped, Step::Wait("skipped"), "{phase:?}");
        assert_eq!(
            after("wait:skipped", 0),
            After::Finished,
            "the build's last word"
        );
        let deferred = step(&phase, Request::DeferUntil(now + 3_600), "s-1");
        assert_eq!(deferred, Step::Wait("deferred"), "{phase:?}");
        assert!(
            matches!(after("wait:deferred", 0), After::Later(_)),
            "looked at again"
        );
        for (word, tab) in [
            (Request::None, ""),
            (Request::Now, "s-1"),
            (Request::DeferUntil(now.saturating_sub(1)), "s-1"),
            (Request::Skip("2.1.281".into()), "s-1"),
            (Request::Skip("2.1.282".into()), "s-elsewhere"),
        ] {
            assert_eq!(
                step(&phase, word.clone(), tab),
                Step::Wait("not-idle"),
                "{phase:?} {word:?}"
            );
        }
    }
}

/// `<exe> --version` is bounded whole while the sweep holds its lock (probe, exit and
/// output read): a candidate that never answers, or whose grandchild holds the pipe, is
/// `TimedOut`, its group killed. Negative controls: an answer is read; an exit without a
/// version is `NoVersion`.
#[cfg(unix)]
#[test]
fn a_version_probe_is_bounded_whole() {
    let dir = scratch("version-probe");
    // The sleeps carry this process's pid as their fraction, so the `ps` sweep below finds
    // only this test's processes, never another run's. A fraction, because macOS `sleep`
    // exits at once on a number past about 4e9 seconds.
    let pid = std::process::id();
    let (sleep_a, sleep_b, sleep_c) = (
        format!("987653.{pid}"),
        format!("987654.{pid}"),
        format!("120.98765{pid}"),
    );
    let script = |name: &str, body: &str| {
        let path = dir.join(name);
        sh_script(&path, body);
        path
    };
    // A grandchild holding the pipe as well as the child: the group is
    // killed, or the read of the pipe would never end.
    let hang = script("hang", &format!("sleep {sleep_a} &\nexec sleep {sleep_b}"));
    // The child answers and exits; its grandchild keeps the pipe open for two
    // minutes, so a read joined with no deadline fails this test (it reads the
    // version, late) rather than hanging it.
    let lingers = script(
        "lingers",
        &format!("sleep {sleep_c} &\necho '2.1.282 (Claude Code)'\nexit 0"),
    );
    let ours = |l: &str| {
        [&sleep_a, &sleep_b, &sleep_c]
            .iter()
            .any(|n| l.trim_end().ends_with(&format!("sleep {n}")))
    };
    let answer = script("answer", "echo '2.1.282 (Claude Code)'");
    let silent = script("silent", "exit 3");
    // The first exec of a script this process just wrote can take seconds on
    // macOS (the system's check of a new file), which is not the probe's time:
    // run the two scripts that end once, unbounded, so the bounded probes
    // below time the probe. (`hang` and `lingers` time out anyway.)
    for warm in [&answer, &silent] {
        let _ = Command::new(warm).output();
    }
    for (name, exe) in [("hang", &hang), ("lingers", &lingers)] {
        let started = Instant::now();
        assert_eq!(
            version_within(exe, Duration::from_millis(300)),
            Probed::TimedOut,
            "{name}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "{name}: bounded: {:?}",
            started.elapsed()
        );
    }
    // Every group was killed: gone once the kernel has reaped it (the pipe's
    // close, which ended the read, comes first in an exit).
    // A `ps` that failed (it can, racing a process that exits) is no evidence
    // that nothing is alive: it counts as not-yet-gone, never as gone.
    let gone = (0..100).any(|_| {
        let Some(left) = ps(&["-A", "-o", "stat=,command="]) else {
            std::thread::sleep(Duration::from_millis(20));
            return false;
        };
        let alive = left
            .lines()
            .any(|l| ours(l) && !l.trim_start().starts_with('Z'));
        if alive {
            std::thread::sleep(Duration::from_millis(20));
        }
        !alive
    });
    assert!(
        gone,
        "the probes' whole groups are gone: {:?}",
        ps(&["-A", "-o", "pid=,ppid=,pgid=,stat=,command="])
            .unwrap_or_default()
            .lines()
            .filter(|l| ours(l))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        version_within(&answer, Duration::from_secs(60)),
        Probed::Version(Version::parse("2.1.282").expect("v"))
    );
    assert_eq!(
        version_within(&silent, Duration::from_secs(60)),
        Probed::NoVersion
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The version cache remembers a clean answer while the file is unchanged, but a timeout
/// is no verdict on the build: it is asked again on a doubling back-off (unanswered until
/// then, [`UNREAD_VERSION`] to a look), and a changed file at once. Injected clock.
#[test]
fn a_timed_out_version_probe_is_retried_on_a_back_off() {
    let dir = scratch("version-cache");
    let slow = dir.join("slow");
    std::fs::write(&slow, "#!/bin/sh\n").expect("file");
    let t0 = Instant::now();
    let probes = std::cell::Cell::new(0);
    let timed_out = |_: &Path| {
        probes.set(probes.get() + 1);
        Probed::TimedOut
    };
    let first = t0 + VERSION_RETRY_FIRST;
    assert_eq!(cached_version_with(&slow, t0, timed_out), Asked::Unanswered);
    assert_eq!(probes.get(), 1);
    assert_eq!(
        cached_version_with(&slow, first - Duration::from_secs(1), timed_out),
        Asked::Unanswered,
        "unanswered still"
    );
    assert_eq!(probes.get(), 1, "not before the back-off");
    let second = first + 2 * VERSION_RETRY_FIRST;
    assert_eq!(
        cached_version_with(&slow, first, timed_out),
        Asked::Unanswered
    );
    assert_eq!(probes.get(), 2, "asked again");
    assert_eq!(
        cached_version_with(&slow, second - Duration::from_secs(1), timed_out),
        Asked::Unanswered
    );
    assert_eq!(probes.get(), 2, "the back-off doubled");
    // Answered at last: remembered.
    let v = Version::parse("2.1.282").expect("v");
    let answers = |_: &Path| {
        probes.set(probes.get() + 1);
        Probed::Version(Version::parse("2.1.282").expect("v"))
    };
    assert_eq!(
        cached_version_with(&slow, second, answers),
        Asked::Answered(Some(v.clone()))
    );
    assert_eq!(
        cached_version_with(&slow, second + VERSION_RETRY_MAX, answers),
        Asked::Answered(Some(v))
    );
    assert_eq!(probes.get(), 3, "an answer is not asked again");
    // Negative control: a clean failure is remembered for the file as it is.
    let bare = dir.join("bare");
    std::fs::write(&bare, "#!/bin/sh\n").expect("file");
    let fails = |_: &Path| {
        probes.set(probes.get() + 1);
        Probed::NoVersion
    };
    assert_eq!(cached_version_with(&bare, t0, fails), Asked::Answered(None));
    assert_eq!(
        cached_version_with(&bare, t0 + VERSION_RETRY_MAX, fails),
        Asked::Answered(None)
    );
    assert_eq!(probes.get(), 4, "a clean failure is not asked again");
    // ... until the file changes.
    std::fs::write(&bare, "#!/bin/sh\necho 2.1.283\n").expect("file");
    let _ = cached_version_with(&bare, t0, fails);
    assert_eq!(probes.get(), 5, "a changed file is asked at once");
    // No file: nothing to ask, and nothing to ask again.
    assert_eq!(
        cached_version_with(&dir.join("absent"), t0, fails),
        Asked::Answered(None)
    );
    assert_eq!(probes.get(), 5);
    let _ = std::fs::remove_dir_all(dir);
}

/// One `turn` reply's head: a `skipped` verdict sent nothing (the fence's
/// `reason=changed` typed nothing and is a wait); a guarded Enter that missed
/// (`reason=guard`) left the text typed and unsent, a cut ([`Typed::Cut`]),
/// not a refusal; and an `OK` verdict typed.
#[test]
fn a_skipped_turn_typed_nothing() {
    assert_eq!(turn_verdict("OK 3 id=1 submitted=1 status=settled"), Ok(()));
    assert_eq!(
        turn_verdict("OK 0 turn skipped reason=changed submitted=0 seq=9 id=1"),
        Err(Typed::Changed)
    );
    assert_eq!(
        turn_verdict("OK 0 turn skipped reason=guard submitted=0 seq=9 id=1"),
        Err(Typed::Cut("guard".to_string()))
    );
    assert!(matches!(
        turn_verdict("ERR busy turn=4"),
        Err(Typed::Refused(_))
    ));
    // A person typing through the whole yield: nothing typed, a wait.
    assert_eq!(
        turn_verdict("ERR yield timeout momentum=0.84"),
        Err(Typed::Yielded)
    );
    assert!(is_generation("12.345") && !is_generation("12") && !is_generation("1.x"));
    // The server's own reader decides: an epoch past `u64::MAX` is no fence.
    assert!(!is_generation("18446744073709551616.15"));
}

/// A draft begun after the restart's own screen check is read again right before the
/// SIGTERM and vetoes it (`wait:changed-before-signal`, still announced). Negative
/// control: the same draft on the restart's first look stops it there (`wait:changed`).
#[cfg(unix)]
#[test]
fn a_draft_begun_before_the_signal_vetoes_it() {
    let rule = "─".repeat(20);
    let draft = format!(
        r#"{{"rows":["{rule}","❯ wait, one more thing","{rule}"],"cursor":{{"row":1,"col":22}},"seq":79,"human_ms":null,"first":0}}"#
    );
    for (case, clean_reads, expect) in [
        // The visit's read and restart's own check see an empty composer;
        // the read before the signal sees the draft.
        ("before-signal", 2, "wait:changed-before-signal"),
        ("first-look", 1, "wait:changed"),
    ] {
        let answers = Answers {
            after: Some((clean_reads, draft.clone())),
            ..Answers::default()
        };
        let h = Parked::new(&format!("presignal-{case}"), answers, St::default(), 3600);
        let marker = "ATERM-UPGRADE-READY-0badf00d";
        let project = h.opts.home.join(".claude/projects/p");
        std::fs::create_dir_all(&project).expect("project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            format!(
                r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"Saved.\n{marker}"}}]}}}}"#
            ) + "\n",
        )
        .expect("transcript");
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - 60,
                asks: 1,
            },
            marker: marker.to_string(),
            tab: TAB.to_string(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            last_seq: 77,
            seq_since_s: now_s() - 3600,
            ..unsettled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        assert_eq!(h.visit().step, expect, "{case}");
        assert!(alive(h.sf.pid), "{case}: never signalled");
        assert!(
            matches!(
                load(&h.opts, SESSION).map(|s| s.phase),
                Some(Phase::Announced { .. })
            ),
            "{case}: still announced"
        );
    }
}

/// An idle Claude's screen as a host with the generation fence sends it (a
/// `gen` beside the `seq`), and that host's `help turn` naming the fence.
fn fenced_answers(turn: Vec<&'static str>) -> Answers {
    let rule = "─".repeat(20);
    Answers {
        screen: format!(
            r#"{{"rows":["{rule}","❯ ","{rule}"],"cursor":{{"row":1,"col":2}},"seq":77,"first":0,"gen":"4.77","human_ms":null}}"#
        ),
        help: "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...",
        turn,
        ..Answers::default()
    }
}

/// An announced state for [`SESSION`] in [`TAB`] whose notice the parked
/// agent of `h` holds, `marker` out, its screen just seen to move.
#[cfg(unix)]
fn announced_to(h: &Parked, marker: &str) -> St {
    St {
        phase: Phase::Announced {
            at_s: now_s() - 60,
            asks: 1,
        },
        marker: marker.to_string(),
        tab: TAB.to_string(),
        notice_pid: h.sf.pid,
        notice_start: squash(&h.sf.proc_start),
        ..unsettled()
    }
}

/// The transcript of [`SESSION`] under `h`'s home, its last assistant row
/// saying `text`.
#[cfg(unix)]
fn transcript_says(h: &Parked, text: &str) {
    let project = h.opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        format!(
            r#"{ASKED}
{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        ) + "\n",
    )
    .expect("transcript");
}

/// A continuation on a screen that keeps redrawing is typed unfenced after its fenced
/// tries (still checked for an empty composer), never `done:no-continue`. Negative
/// control: a refused turn is not typed again.
#[cfg(unix)]
#[test]
fn a_continuation_on_a_screen_that_keeps_moving_is_still_typed() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let moved = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let mut replies = vec![moved; CONTINUE_FENCE_TRIES];
    replies.push("OK 0 id=2 submitted=1 status=settled");
    let rig = Rig::with("continue-moving", &before, fenced_answers(replies));
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    let typed = rig.typed();
    assert_eq!(typed.len(), CONTINUE_FENCE_TRIES + 1, "{typed:#?}");
    assert!(
        typed[..CONTINUE_FENCE_TRIES]
            .iter()
            .all(|t| t.contains(" turn if-gen=4.77 yield=0.2 ")),
        "{typed:#?}"
    );
    assert!(
        !typed[CONTINUE_FENCE_TRIES].contains("if-gen="),
        "{typed:#?}"
    );
    drop(rig);
    // Negative control: refused, not typed at again.
    let rig = Rig::with(
        "continue-refused",
        &before,
        fenced_answers(vec!["ERR denied"]),
    );
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:no-continue", "{r:?}");
    assert_eq!(rig.typed().len(), 1);
}

/// A fenced relaunch line that parks past the turn's timeout on a person's typing (`ERR
/// yield timeout`) waits `yield`: it never fails the relaunch nor types unfenced.
/// Negative control: a real refusal is still one, at once.
#[cfg(unix)]
#[test]
fn a_yield_that_times_out_never_fails_the_relaunch() {
    let dir = scratch("relaunch-yield");
    let yielded = "ERR yield timeout momentum=0.84";
    let (sock, asked) = instance_with(
        &dir,
        fenced_answers(vec![yielded, "OK 0 id=2 submitted=1 status=settled"]),
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let shell = std::process::id();
    let line = "claude --resume";
    assert_eq!(
        type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            line,
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0
            },
            |_, _, _| true
        ),
        Err(RelaunchLineError::Wait("yield"))
    );
    let typed: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    assert_eq!(
        typed.len(),
        1,
        "one fenced try, nothing unfenced: {typed:#?}"
    );
    assert!(typed[0].contains("if-gen="), "{typed:#?}");
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
    // Negative control.
    let dir = scratch("relaunch-refused");
    let (sock, asked) = instance_with(&dir, fenced_answers(vec!["ERR denied"]));
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("complete empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    assert!(matches!(
        type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            shell,
            TAB,
            SESSION,
            line,
            None,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0
            },
            |_, _, _| true
        ),
        Err(RelaunchLineError::Turn(_))
    ));
    assert_eq!(turns(&asked), 1);
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// With the shell's dialect known, the relaunch attempt asks the window for a typed re-key
/// (`rekey shell=<pid>`) and types the healed line. A refusal, an older window, or a
/// healed line past the tty's bound (its key taken back) types the plain line; no dialect
/// asks nothing; an attempt that types nothing takes the key back (`rekey withdraw`).
#[cfg(unix)]
#[test]
fn the_relaunch_line_carries_a_rekey_and_never_waits_on_one() {
    const KEY: &str = "/c/aterm/rekey/s-0a.1";
    const ISSUED: &str = "OK rekey ttl=120 path=/c/aterm/rekey/s-0a.1";
    const SETTLED: &str = "OK 0 id=2 submitted=1 status=settled";
    let line = " '/pkg/claude' '--resume' 'x'";
    let run = |name: &str, rekey: &'static str, turn: &'static str, dialect, line: &str| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                rekey,
                ..fenced_answers(vec![turn])
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        let mut c = connect(&opts, TAB).expect("control connection");
        let got = type_relaunch_line_with(
            &opts,
            &mut c,
            4242,
            TAB,
            line,
            dialect,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
            },
            || true,
            |_, _, _| true,
        );
        drop(c);
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (got, asked)
    };
    let of = |asked: &[String], word: &str| -> Vec<String> {
        asked.iter().filter(|l| l.contains(word)).cloned().collect()
    };
    let healed = upgrade::with_rekey(Dialect::Zsh, line, Path::new(KEY)).expect("fits");
    assert!(healed.contains(&format!("<'{KEY}';")) && healed.ends_with(&line[1..]));

    // Issued: the healed line is typed, after one `rekey` naming the shell.
    let (got, asked) = run("rekey-issued", ISSUED, SETTLED, Some(Dialect::Zsh), line);
    assert_eq!(got, Ok(()));
    assert_eq!(of(&asked, " rekey "), [format!("@{TAB} rekey shell=4242")]);
    let typed = of(&asked, " turn ");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(typed[0].ends_with(&healed), "{typed:#?}");

    // Refused (a healthy tab), and a window that knows no such verb: plain.
    for (name, reply) in [
        (
            "rekey-healthy",
            "ERR rekey integration=on (a working key is never moved)",
        ),
        ("rekey-older", "ERR unknown verb (try: help)"),
    ] {
        let (got, asked) = run(name, reply, SETTLED, Some(Dialect::Zsh), line);
        assert_eq!(got, Ok(()), "{name}");
        let typed = of(&asked, " turn ");
        assert!(
            typed[0].ends_with(line) && !typed[0].contains("__aterm_shell_nonce"),
            "{name}: {typed:#?}"
        );
        assert!(of(&asked, "rekey withdraw").is_empty(), "{name}");
    }

    // No dialect read: nothing asked.
    let (got, asked) = run("rekey-no-dialect", ISSUED, SETTLED, None, line);
    assert_eq!(got, Ok(()));
    assert!(of(&asked, " rekey ").is_empty(), "{asked:#?}");

    // A healed line the tty could cut: the key is taken back, the plain line goes.
    let long = format!(" '/pkg/claude' '{}'", "p".repeat(upgrade::MAX_LINE - 20));
    let (got, asked) = run("rekey-too-long", ISSUED, SETTLED, Some(Dialect::Zsh), &long);
    assert_eq!(got, Ok(()));
    assert_eq!(of(&asked, "rekey withdraw").len(), 1, "{asked:#?}");
    assert!(of(&asked, " turn ")[0].ends_with(&long));

    // Issued, then nothing typed (a person typing through the yield): taken
    // back before the attempt returns. Negative control: the same yield with no
    // re-key issued withdraws nothing.
    for (name, rekey, withdrawn) in [
        ("rekey-yield", ISSUED, 1),
        (
            "rekey-yield-none",
            "ERR rekey integration=off (nothing to heal)",
            0,
        ),
    ] {
        let yielded = "ERR yield timeout momentum=0.84";
        let (got, asked) = run(name, rekey, yielded, Some(Dialect::Zsh), line);
        assert_eq!(got, Err(RelaunchLineError::Wait("yield")), "{name}");
        let withdraws: Vec<usize> = asked
            .iter()
            .enumerate()
            .filter(|(_, l)| l.contains("rekey withdraw"))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(withdraws.len(), withdrawn, "{name}: {asked:#?}");
        if let Some(&at) = withdraws.first() {
            let last_turn = asked
                .iter()
                .rposition(|l| l.contains(" turn "))
                .expect("a try");
            assert!(at > last_turn, "{name}: taken back after the last try");
        }
    }
}

/// At bash the dialect is read off the live shell ([`shell_dialect`]) and the healed line
/// is typed (`send`, never pasted). Negative control: a refused re-key types the plain
/// line and takes nothing back.
#[cfg(unix)]
#[test]
fn a_relaunch_typed_at_bash_takes_the_key_its_live_shell_names() {
    const ISSUED: &str = "OK rekey ttl=120 path=/c/aterm/rekey/s-0a.1";
    let line = " '/pkg/claude' '--resume' 'x'";
    let mut bash = stand_in_shell();
    let mut dialect = None;
    for _ in 0..6_000 {
        dialect = shell_dialect(bash.id(), &table());
        if dialect == Some(Dialect::Bash) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(dialect, Some(Dialect::Bash), "the live shell names bash");
    let healed =
        upgrade::with_rekey(Dialect::Bash, line, Path::new("/c/aterm/rekey/s-0a.1")).expect("fits");
    for (name, rekey, want) in [
        ("rekey-bash", ISSUED, healed.as_str()),
        (
            "rekey-bash-refused",
            "ERR rekey integration=on (a working key is never moved)",
            line,
        ),
    ] {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                rekey,
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
        let mut c = connect(&opts, TAB).expect("control connection");
        type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            bash.id(),
            TAB,
            SESSION,
            line,
            dialect,
            PromptLook {
                mark: &mut None,
                ended_at_s: 0,
            },
            |_, _, _| true,
        )
        .expect("typed at bash");
        drop(c);
        let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
        assert!(
            asked
                .iter()
                .any(|r| r == &format!("@{TAB} rekey shell={}", bash.id())),
            "{name}: {asked:#?}"
        );
        assert!(
            asked
                .iter()
                .any(|r| r.ends_with(&format!("send -- {want}"))),
            "{name}: {asked:#?}"
        );
        assert_eq!(turns_in(&asked), 0, "{name}: never a paste");
        assert!(
            !asked.iter().any(|r| r.ends_with(" rekey withdraw")),
            "{name}: {asked:#?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    let _ = bash.kill();
    let _ = bash.wait();
}

/// The requests of `asked` that paste into the tab.
#[cfg(unix)]
fn turns_in(asked: &[String]) -> usize {
    asked.iter().filter(|l| l.contains(" turn ")).count()
}

/// The relaunch line is never typed onto a person's command (the fence proves only that
/// the screen held still, not that the input line is empty): the first read marks where
/// the prompt left the cursor, kept across attempts, and typing after the mark waits
/// `typing` until deleted. Negative controls: a still prompt types fenced; a ticking one
/// types unfenced after its tries; an async redraw waits `prompt-moved`, marked next time.
#[cfg(unix)]
#[test]
fn the_relaunch_line_waits_for_a_person_typing_at_the_prompt() {
    let (prompt, help) = (prompt_screen, FENCED_HELP);
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let run = |name: &str, answers: Answers, mark: &mut Option<PromptMark>| {
        relaunch_attempt(name, answers, mark, |_, _, _| true)
    };
    // The prompt, then a person's `rm -rf build` after it.
    let mut mark = None;
    let (got, typed) = run(
        "relaunch-typing",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            after: Some((1, prompt("~/proj % rm -rf build", 21, 78))),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "nothing typed onto theirs: {typed:#?}");
    // The next attempt: the person paused on their half-typed command. The
    // mark the prompt came back with still judges it — never theirs.
    let (got, typed) = run(
        "relaunch-still-typed",
        Answers {
            screen: prompt("~/proj % rm -rf build", 21, 78),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "a later attempt types nothing either");
    // They deleted it: the prompt reads as marked again, and the line goes.
    let (got, typed) = run(
        "relaunch-deleted",
        Answers {
            screen: prompt("~/proj % ", 9, 79),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
    // The mark survives the state file.
    let st = St {
        prompt: Some(PromptMark {
            row: 3,
            col: 9,
            left: "a, b % ".to_string(),
            right: Some(40),
        }),
        ..St::default()
    };
    assert_eq!(
        St::from_json(&st.to_json()).map(|s| s.prompt),
        Some(st.prompt)
    );
    // Negative control: a prompt that holds its cursor.
    let (got, typed) = run(
        "relaunch-still",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut None,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(
        typed[0].contains(" turn if-gen=4.77 yield=0.2 "),
        "{typed:#?}"
    );
    // Negative control: an async prompt redrawn after the mark — a wait, the
    // mark dropped; the next attempt marks the redrawn prompt and types.
    let mut mark = None;
    let (got, typed) = run(
        "relaunch-redrawn",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            after: Some((1, prompt("~/proj (main) % ", 16, 78))),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("prompt-moved")));
    assert!(typed.is_empty(), "{typed:#?}");
    assert_eq!(mark, None, "the moved prompt's mark is dropped");
    let (got, typed) = run(
        "relaunch-redrawn-next",
        Answers {
            screen: prompt("~/proj (main) % ", 16, 78),
            help,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
    );
    assert_eq!(got, Ok(()), "marked by the next attempt, then typed");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert!(typed[0].contains(" turn if-gen=4.78 "), "{typed:#?}");
    // Negative control: a screen that never holds still, its cursor in place.
    let moved = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let mut replies = vec![moved; RELAUNCH_FENCE_TRIES];
    replies.push(settled);
    let (got, typed) = run(
        "relaunch-ticking",
        Answers {
            screen: prompt("~/proj % ", 9, 77),
            help,
            turn: replies,
            ..Answers::default()
        },
        &mut None,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), RELAUNCH_FENCE_TRIES + 1, "{typed:#?}");
    assert!(
        !typed[RELAUNCH_FENCE_TRIES].contains("if-gen="),
        "{typed:#?}"
    );
}

/// A person's typing that wraps is typing too: at the bottom of the screen the
/// wrap scrolls it, so the cursor keeps its row further left, and the marked
/// prompt, with the command after it, is on the row above.
#[test]
fn typing_that_wraps_at_the_bottom_of_the_screen_is_still_typing() {
    let at = |rows: &[&str], row: usize, col: usize| Screen {
        rows: rows.iter().map(|r| (*r).to_string()).collect(),
        cursor: Some((row, col)),
        seq: 1,
        first: 0,
        generation: None,
        human: HumanInput::Never,
    };
    let mark = PromptMark::of(&at(&["out", "~/p % "], 1, 6)).expect("mark");
    assert_eq!(mark.at(&at(&["out", "~/p % "], 1, 6)), AtPrompt::Same);
    assert_eq!(mark.at(&at(&["out", "~/p % ls"], 1, 8)), AtPrompt::Typed);
    assert_eq!(
        mark.at(&at(&["~/p % a-very-long-comm", "and"], 1, 3)),
        AtPrompt::Typed,
        "wrapped and scrolled"
    );
    assert_eq!(
        mark.at(&at(&["out", "~/p (main) % "], 1, 13)),
        AtPrompt::Moved,
        "an async redraw of the prompt"
    );
}

/// A shell prompt's one-row `text --json`: `text`, the cursor at `col`, the
/// content sequence (and generation `4.<seq>`) `seq`.
#[cfg(unix)]
fn prompt_screen(text: &str, col: usize, seq: u64) -> String {
    format!(
        r#"{{"rows":["{text}"],"cursor":{{"row":0,"col":{col}}},"seq":{seq},"first":0,"gen":"4.{seq}"}}"#
    )
}

/// One relaunch attempt ([`type_relaunch_line`]) against a stand-in instance
/// answering `answers`, the shell's tab claim read by `probe`: what it
/// returned, and every `turn` it sent.
#[cfg(unix)]
fn relaunch_attempt(
    name: &str,
    answers: Answers,
    mark: &mut Option<PromptMark>,
    probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> (Result<(), RelaunchLineError>, Vec<String>) {
    relaunch_attempt_ended(name, answers, mark, 0, probe)
}

/// [`relaunch_attempt`] for an agent whose end was asked at `ended_at_s`
/// (Unix seconds, [`PromptLook::ended_at_s`]).
#[cfg(unix)]
fn relaunch_attempt_ended(
    name: &str,
    answers: Answers,
    mark: &mut Option<PromptMark>,
    ended_at_s: u64,
    probe: impl FnMut(&mut Client, u32, &str) -> bool,
) -> (Result<(), RelaunchLineError>, Vec<String>) {
    let dir = scratch(name);
    let (sock, asked) = instance_with(&dir, answers);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
    let mut c = connect(&opts, TAB).expect("control connection");
    let got = type_relaunch_line(
        &opts,
        &Records::of(&opts.home, None),
        &mut c,
        std::process::id(),
        TAB,
        SESSION,
        " '/pkg/claude' '--resume' 'x'",
        None,
        PromptLook { mark, ended_at_s },
        probe,
    );
    drop(c);
    let typed: Vec<String> = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (got, typed)
}

/// The `help turn` reply of a host whose `turn` takes the fence.
#[cfg(unix)]
const FENCED_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...";

/// At a bash prompt (typed to with `send`, which takes no `if-gen=` fence) the prompt is
/// read against its mark once more right before the keys, so a person's typing after it
/// is a wait. Negative control: the bare marked prompt is typed at.
#[cfg(unix)]
#[test]
fn a_bash_prompt_with_typing_after_its_mark_is_never_typed_at() {
    let mut bash = stand_in_shell();
    for _ in 0..6_000 {
        if atpkg::caller_shell::process_args(bash.id())
            .is_some_and(|a| Dialect::from_exe_name(&a.exec_path) == Some(Dialect::Bash))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let attempt = |name: &str, screen: String| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen,
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        std::fs::create_dir_all(opts.home.join(".claude/sessions")).expect("empty roster");
        let mut c = connect(&opts, TAB).expect("control connection");
        let mut mark = Some(PromptMark {
            row: 0,
            col: 9,
            left: "~/proj % ".to_string(),
            right: None,
        });
        let got = type_relaunch_line(
            &opts,
            &Records::of(&opts.home, None),
            &mut c,
            bash.id(),
            TAB,
            SESSION,
            "claude --resume x",
            None,
            PromptLook {
                mark: &mut mark,
                ended_at_s: 0,
            },
            |_, _, _| true,
        );
        drop(c);
        let sent = asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" send ")).count())
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (got, sent)
    };
    let (got, sent) = attempt("bash-typing", prompt_screen("~/proj % ls", 11, 78));
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert_eq!(sent, 0, "nothing sent over a person's typing");
    let (got, sent) = attempt("bash-clear", prompt_screen("~/proj % ", 9, 78));
    assert_eq!(got, Ok(()), "negative control: the bare prompt is typed at");
    assert_eq!(sent, 1);
    let _ = bash.kill();
    let _ = bash.wait();
}

/// The relaunch line is never typed into a program a person started at the returned
/// prompt during the attempt's tries (a fenced turn can park 30 s): a prompt that moves
/// after the mark is a wait, and the shell is proven to hold the tab right before every
/// typed try, the unfenced fallback included. Negative control: the shell holding, typed.
#[cfg(unix)]
#[test]
fn the_relaunch_line_is_never_typed_into_a_program_started_at_the_prompt() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    // The probe's scenario: the bare prompt, then an alt-screen program.
    let program = r#"{"rows":["hello","world","~"],"cursor":{"row":1,"col":0},"seq":90,"first":0,"gen":"5.90"}"#;
    let mut calls = 0;
    let mut mark = None;
    let (got, typed) = relaunch_attempt(
        "relaunch-program",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            after: Some((1, program.to_string())),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
        |_, _, _| {
            calls += 1;
            calls <= 2
        },
    );
    assert!(matches!(got, Err(RelaunchLineError::Wait(_))), "{got:?}");
    assert!(
        typed.is_empty(),
        "nothing typed into the program: {typed:#?}"
    );
    assert_eq!(mark, None, "a later attempt marks afresh");
    // A program that leaves the screen as it was (`cat`, a job started
    // without output): the prompt reads unmoved, and only the tab's owner
    // tells — the proof right before the typed try refuses it.
    let mut calls = 0;
    let (got, typed) = relaunch_attempt(
        "relaunch-silent-program",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut None,
        |_, _, _| {
            calls += 1;
            calls <= 2
        },
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("tab-ownership-changed")));
    assert!(typed.is_empty(), "{typed:#?}");
    // The unfenced fallback is proven too: every fenced try skipped (the
    // screen moving), then the shell gone from the front before the plain
    // turn — nothing typed unfenced.
    let skipped = "OK 0 turn skipped reason=changed submitted=0 seq=78 id=1";
    let mut replies = vec![skipped; RELAUNCH_FENCE_TRIES];
    replies.push(settled);
    let mut calls = 0;
    let (got, typed) = relaunch_attempt(
        "relaunch-fallback-program",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            help: FENCED_HELP,
            turn: replies,
            ..Answers::default()
        },
        &mut None,
        |_, _, _| {
            calls += 1;
            calls <= 2 + RELAUNCH_FENCE_TRIES
        },
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("tab-ownership-changed")));
    assert_eq!(typed.len(), RELAUNCH_FENCE_TRIES, "{typed:#?}");
    assert!(typed.iter().all(|t| t.contains("if-gen=")), "{typed:#?}");
    // Negative control.
    let (got, typed) = relaunch_attempt(
        "relaunch-shell-holds",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut None,
        |_, _, _| true,
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
}

/// A caret moved home over a half-typed command is typing (a pure cursor move changes no
/// content, so the fence passes it): the mark keeps where a right prompt began, and the
/// cells from the cursor up to it must stay blank. Negative controls: a right-prompt clock
/// ticking in place, and a bare prompt, are the prompt unmoved.
#[cfg(unix)]
#[test]
fn a_caret_moved_home_over_a_half_typed_command_is_typing() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let mut mark = None;
    let (got, typed) = relaunch_attempt(
        "relaunch-caret-typing",
        Answers {
            screen: prompt_screen("~/proj % ", 9, 77),
            after: Some((1, prompt_screen("~/proj % rm -rf build", 21, 78))),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
        |_, _, _| true,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty());
    // The next attempt: the caret moved home (ctrl-a) over the command.
    let (got, typed) = relaunch_attempt(
        "relaunch-caret-home",
        Answers {
            screen: prompt_screen("~/proj % rm -rf build", 9, 78),
            help: FENCED_HELP,
            turn: vec![settled],
            ..Answers::default()
        },
        &mut mark,
        |_, _, _| true,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "never typed in front of it: {typed:#?}");
    // The mark itself, over rows: a right prompt learned at the mark.
    let at = |row: &str, col: usize| Screen {
        rows: vec![row.to_string()],
        cursor: Some((0, col)),
        seq: 1,
        first: 0,
        generation: None,
        human: HumanInput::Never,
    };
    let clock = PromptMark::of(&at("~/proj %            12:34:56", 9)).expect("mark");
    assert_eq!(clock.right, Some(20));
    assert_eq!(
        clock.at(&at("~/proj %            12:34:57", 9)),
        AtPrompt::Same,
        "the clock ticked in place"
    );
    assert_eq!(
        clock.at(&at("~/proj % rm         12:34:57", 9)),
        AtPrompt::Typed,
        "a caret home over `rm`"
    );
    assert_eq!(
        clock.at(&at("~/proj %  rm        12:34:57", 9)),
        AtPrompt::Typed,
        "a leading blank hides nothing"
    );
    let bare = PromptMark::of(&at("~/proj %", 9)).expect("mark");
    assert_eq!(bare.right, None);
    assert_eq!(bare.at(&at("~/proj %", 9)), AtPrompt::Same);
    assert_eq!(bare.at(&at("~/proj % x", 9)), AtPrompt::Typed);
    // It survives the state file; an older build's mark does not.
    assert_eq!(PromptMark::parse(&clock.word()), Some(clock));
    assert_eq!(PromptMark::parse("3,9,a, b % "), None);
}

/// Typeahead before the first read is never the mark (marking where the cursor sits would
/// append the line to it): with the shell's `133;B` input start (`blocks --json`'s
/// `cmdcol`) the cursor must sit exactly there, else after the prompt's trailing blank, and
/// the cells at the cursor must be blank. Negative controls: a clean prompt is typed at.
#[cfg(unix)]
#[test]
fn typeahead_before_the_first_read_is_never_the_mark() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let entering = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":9,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;
    let attempt = |name: &str, screen: String, blocks: &'static str, line: &'static str| {
        let mut mark = None;
        let (got, typed) = relaunch_attempt(
            name,
            Answers {
                screen,
                help: FENCED_HELP,
                turn: vec![settled],
                blocks,
                line,
                ..Answers::default()
            },
            &mut mark,
            |_, _, _| true,
        );
        (got, typed, mark)
    };
    // No integration: the typeahead does not end in the prompt's blank.
    let (got, typed, mark) = attempt(
        "typeahead-plain",
        prompt_screen("~/proj % rm -rf build", 21, 77),
        "ERR unscripted",
        "ERR unscripted",
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty() && mark.is_none(), "{typed:#?} {mark:?}");
    // The integration's input start: typeahead that does end in a blank, which the plain
    // rule alone would take for the prompt, is still typing.
    let (got, typed, _) = attempt(
        "typeahead-133",
        prompt_screen("~/proj % ls ", 12, 77),
        entering,
        "OK ~/proj % ls",
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "{typed:#?}");
    // A caret moved home over the typeahead before the first read.
    let (got, typed, _) = attempt(
        "typeahead-home",
        prompt_screen("~/proj % rm -rf build", 9, 77),
        entering,
        "OK ~/proj % rm -rf build",
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "{typed:#?}");
    // Negative controls: a clean prompt, with the integration and without.
    for (name, blocks, line) in [
        ("clean-133", entering, "OK ~/proj %"),
        ("clean-plain", "ERR unscripted", "ERR unscripted"),
    ] {
        let (got, typed, mark) = attempt(name, prompt_screen("~/proj %", 9, 77), blocks, line);
        assert_eq!(got, Ok(()), "{name}");
        assert_eq!(typed.len(), 1, "{name}: {typed:#?}");
        assert!(mark.is_some(), "{name}");
    }
}

/// A shell mark that names another row (the `133;B` mark's row read as the line above the
/// prompt) is no person's typing when the read's `human_ms` stamp shows nobody keyed since the
/// agent's end was asked: the line is typed. Negative controls: a key after the end, no stamp,
/// a never-keyed session (`null`) or an unknown end waits `mark-elsewhere`; wrapped typing is
/// never typed onto; typeahead on the mark's row is `typing`.
#[cfg(unix)]
#[test]
fn a_mark_that_names_another_row_is_no_persons_typing() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    const PROMPT: &str = "me@a-studio-of-128gb aterm %";
    let col = PROMPT.len() + 1;
    // The returned prompt's last rows (`text --json`, tail-trimmed), the cursor where the
    // prompt ends.
    let screen = |rows: &[&str], cursor: (usize, usize), human: &str| {
        let rows: Vec<String> = rows.iter().map(|r| format!("{r:?}")).collect();
        format!(
            r#"{{"rows":[{}],"cursor":{{"row":{},"col":{}}},"seq":77,"first":0,"gen":"4.77"{human}}}"#,
            rows.join(","),
            cursor.0,
            cursor.1
        )
    };
    let returned = [
        " '--resume' '253a40df-d85e-420b-85f5-0e322e9e663b'",
        "Resume this session with:",
        "claude --resume 253a40df-d85e-420b-85f5-0e322e9e663b",
        PROMPT,
    ];
    // `blocks 1 --json` and `line 69`, as the tab answered them.
    let blocks = r#"OK {"blocks":[{"id":0,"state":"entering","exit":null,"prompt":69,"cmd":69,"cmdcol":29,"out":null,"end":null,"cwd":"/Users//me/aterm","cmdline":""}]}"#;
    let above = "OK claude --resume 253a40df-d85e-420b-85f5-0e322e9e663b";
    assert_eq!(col, 29, "the fixture's mark column is the prompt's length");
    let ended = now_s() - 3;
    let attempt = |name: &str, screen: String, line: &'static str, ended_at_s: u64| {
        let mut mark = None;
        let (got, typed) = relaunch_attempt_ended(
            name,
            Answers {
                screen,
                help: FENCED_HELP,
                turn: vec![settled],
                blocks,
                line,
                ..Answers::default()
            },
            &mut mark,
            ended_at_s,
            |_, _, _| true,
        );
        (got, typed, mark)
    };
    // Nobody keyed since the end (the last key 19 h ago, the end 3 s ago): typed.
    let quiet = r#","human_ms":70000000,"human_seq":51"#;
    let (got, typed, mark) = attempt(
        "mark-elsewhere-quiet",
        screen(&returned, (3, col), quiet),
        above,
        ended,
    );
    assert_eq!(got, Ok(()), "the prompt nobody touched is typed at");
    assert_eq!(typed.len(), 1, "{typed:#?}");
    assert_eq!(mark.map(|m| (m.row, m.col)), Some((3, col)));
    // A person's key after the end (half a second ago): the line holds what they typed,
    // for all the screen shows, so it waits, named. So does a read that cannot date the
    // last key against the end.
    for (name, human, ended_at_s) in [
        ("mark-elsewhere-keyed", r#","human_ms":500"#, ended),
        ("mark-elsewhere-unstamped", "", ended),
        ("mark-elsewhere-never", r#","human_ms":null"#, ended),
        ("mark-elsewhere-end-unknown", quiet, 0),
    ] {
        let (got, typed, mark) =
            attempt(name, screen(&returned, (3, col), human), above, ended_at_s);
        assert_eq!(
            got,
            Err(RelaunchLineError::Wait("mark-elsewhere")),
            "{name}"
        );
        assert!(typed.is_empty() && mark.is_none(), "{name}: {typed:#?}");
    }
    // A person's typing that wrapped past the prompt's row: the mark names the row above
    // (the prompt and the start of their command), the cursor sits on the continuation,
    // after a blank, at the mark's very column.
    let wrapped = [
        "me@a-studio-of-128gb aterm % git commit -am 'fix the relaunch: a mark that na",
        "mes another row",
    ];
    let (got, typed, _) = attempt(
        "mark-elsewhere-wrapped",
        screen(&wrapped, (1, 16), r#","human_ms":800"#),
        "OK me@a-studio-of-128gb aterm % git commit -am 'fix the relaunch: a mark that na",
        ended,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("mark-elsewhere")));
    assert!(typed.is_empty(), "never typed onto theirs: {typed:#?}");
    // The mark on the cursor's row with typeahead after it: `typing`, the
    // person's keys since or not — what stands after the prompt is shown.
    let (got, typed, _) = attempt(
        "mark-here-typeahead",
        screen(&["me@a-studio-of-128gb aterm % ls"], (0, col + 3), quiet),
        "OK me@a-studio-of-128gb aterm % ls",
        ended,
    );
    assert_eq!(got, Err(RelaunchLineError::Wait("typing")));
    assert!(typed.is_empty(), "{typed:#?}");
}

/// Tier-1 of `RelaunchPromptRead` (`aterm_spec::derive::relaunch_prompt_read_model`): the real
/// [`type_relaunch_line`] in every world of the model (shell mark none, here or elsewhere; input
/// stamped or not; text before the cursor ending in a blank or not; a person's keys or not), in
/// every spelling the host has, makes exactly the model's one move. Non-vacuity: both verdicts
/// occur, and the stamp decides the mark-elsewhere world.
#[cfg(unix)]
#[test]
fn the_real_prompt_read_conforms_to_the_model() {
    let model = aterm_spec::derive::relaunch_prompt_read_model();
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let ended = now_s() - 3;
    let mut typed_worlds = 0;
    let mut waited_worlds = 0;
    for mark in 0..=2i64 {
        for stamped in 0..=1i64 {
            for blank in 0..=1i64 {
                for person in 0..=1i64 {
                    // The cursor's row: the prompt, then what a person typed.
                    let (row, col) = match (person, blank) {
                        (0, 1) => ("me@box proj %", 14),
                        (0, _) => ("[me@box proj]$", 14),
                        (_, 1) => ("me@box proj % ls", 17),
                        _ => ("me@box proj % ls", 16),
                    };
                    let humans: &[&str] = match (stamped, person) {
                        (0, _) => &["", r#","human_ms":null"#],
                        (_, 0) => &[r#","human_ms":60000"#],
                        _ => &[r#","human_ms":300"#],
                    };
                    let entering: &'static str = r#"OK {"blocks":[{"id":3,"state":"entering","exit":null,"prompt":10,"cmd":10,"cmdcol":14,"out":null,"end":null,"cwd":"","cmdline":""}]}"#;
                    let running: &'static str = r#"OK {"blocks":[{"id":3,"state":"running","exit":null,"prompt":9,"cmd":9,"cmdcol":14,"out":10,"end":null,"cwd":"","cmdline":"claude; read"}]}"#;
                    let marks: Vec<(&'static str, String)> = match mark {
                        0 => vec![
                            ("ERR unscripted", "ERR unscripted".to_string()),
                            (running, "OK me@box proj % claude; read".to_string()),
                        ],
                        1 => vec![(entering, format!("OK {row}"))],
                        _ => vec![(entering, "OK claude --resume 253a40df".to_string())],
                    };
                    for (spelling, (human, (blocks, line))) in humans
                        .iter()
                        .flat_map(|h| marks.iter().map(move |m| (h, m)))
                        .enumerate()
                    {
                        let screen = format!(
                            r#"{{"rows":["{row}"],"cursor":{{"row":0,"col":{col}}},"seq":77,"first":0,"gen":"4.77"{human}}}"#
                        );
                        let line: &'static str = Box::leak(line.clone().into_boxed_str());
                        let name = format!("prompt-read-{mark}{stamped}{blank}{person}-{spelling}");
                        let (got, typed) = relaunch_attempt_ended(
                            &name,
                            Answers {
                                screen,
                                help: FENCED_HELP,
                                turn: vec![settled],
                                blocks,
                                line,
                                ..Answers::default()
                            },
                            &mut None,
                            ended,
                            |_, _, _| true,
                        );
                        let real = got.is_ok() && typed.len() == 1;
                        assert!(
                            got.is_ok() || typed.is_empty(),
                            "{name}: a wait types nothing: {got:?} {typed:#?}"
                        );
                        let mut state = model.init_state();
                        for (var, value) in [
                            ("phase", 3),
                            ("mark", mark),
                            ("stamped", stamped),
                            ("blank", blank),
                            ("person", person),
                        ] {
                            state.insert(var, value);
                        }
                        // Exactly one of the lane's four moves is the model's here, and it
                        // is the one the real read made: the line, or the wait with its word.
                        let lane: Vec<&str> =
                            ["Read", "WaitTyping", "WaitElsewhere", "WaitUnmarked"]
                                .into_iter()
                                .filter(|a| model.action_enabled(a, &state))
                                .collect();
                        let made = match &got {
                            Ok(()) => "Read",
                            Err(RelaunchLineError::Wait("typing")) => "WaitTyping",
                            Err(RelaunchLineError::Wait("mark-elsewhere")) => "WaitElsewhere",
                            Err(RelaunchLineError::Wait("unmarked")) => "WaitUnmarked",
                            other => panic!("{name}: a read the model has no move for: {other:?}"),
                        };
                        assert_eq!(
                            lane,
                            [made],
                            "{name}: the real read ({got:?}) against the model's lane at {state:?}"
                        );
                        let modelled = model.action_enabled("Read", &state);
                        assert_eq!(real, modelled, "{name}: typed exactly where `Read` is");
                        if real {
                            typed_worlds += 1;
                        } else {
                            waited_worlds += 1;
                        }
                        if (mark, stamped, person) == (2, 1, 0) {
                            assert!(real, "{name}: tab #1's world types");
                            let mut unstamped = state.clone();
                            unstamped.insert("stamped", 0);
                            assert!(
                                !model.action_enabled("Read", &unstamped),
                                "{name}: a projection blind to the stamp would call the real \
                             verdict wrong here — the bind reads the stamp"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(
        typed_worlds > 0 && waited_worlds > 0,
        "{typed_worlds} {waited_worlds}"
    );
}

/// The stale-exit line (`expired`) names the wait that held the relaunch line, in a
/// person's words; with no wait, or one with no words, the bare fact; an end not yet seen
/// is only the one asked for.
#[test]
fn a_stale_exit_names_the_wait_that_held_its_line() {
    let now = now_s();
    let st = St {
        phase: Phase::Exiting { at_s: now - 400 },
        exited_at: now - 400,
        shell: std::process::id(),
        wait: "mark-elsewhere".to_string(),
        ..St::default()
    };
    let (why, detail) = expired(&st, now).expect("past the stale bound");
    assert_eq!(why, "stale-exit");
    assert!(
        detail.starts_with("the agent ended over 5 minutes ago and was not relaunched: ")
            && detail.contains("its shell and its screen disagree")
            && !detail.contains("mark-elsewhere"),
        "{detail}"
    );
    let unworded = St {
        wait: "no-such-wait".to_string(),
        ..st.clone()
    };
    assert_eq!(
        expired(&unworded, now).map(|(_, detail)| detail).as_deref(),
        Some("the agent ended over 5 minutes ago and was not relaunched")
    );
    // Slow to die after its signal, and not yet seen gone: never "ended over
    // 5 minutes ago", and the relaunch's `exiting` wait in words.
    let unseen = St {
        exited_at: 0,
        wait: "exiting".to_string(),
        ..st.clone()
    };
    assert_eq!(
        expired(&unseen, now).map(|(_, detail)| detail).as_deref(),
        Some(
            "the agent was asked to end over 5 minutes ago and was not relaunched: it has not \
             exited yet"
        )
    );
    let plain = St {
        wait: String::new(),
        ..st
    };
    assert_eq!(
        expired(&plain, now).map(|(_, detail)| detail).as_deref(),
        Some("the agent ended over 5 minutes ago and was not relaunched")
    );
}

/// With no mark that a prompt is there, no blank ending it, and nobody keyed, the line is
/// never typed on the stamp alone: it waits `unmarked`. The stamp proves nobody typed, not
/// that the shell is at a prompt (a `while :; do claude; read -k1; done` loop's `read`
/// holds the terminal). A key since waits `typing`; a prompt ending in a blank is typed at.
#[cfg(unix)]
#[test]
fn a_prompt_no_mark_attests_is_never_typed_on_the_stamp_alone() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let ended = now_s() - 3;
    let screen = |row: &str, col: usize, human: &str| {
        format!(
            r#"{{"rows":["{row}"],"cursor":{{"row":0,"col":{col}}},"seq":77,"first":0,"gen":"4.77"{human}}}"#
        )
    };
    let running = r#"OK {"blocks":[{"id":7,"state":"running","exit":null,"prompt":40,"cmd":40,"cmdcol":14,"out":41,"end":null,"cwd":"/Users//me/proj","cmdline":"while :; do claude; read -k1; done"}]}"#;
    let run = |name: &str, screen: String, blocks: &'static str| {
        relaunch_attempt_ended(
            name,
            Answers {
                screen,
                help: FENCED_HELP,
                turn: vec![settled],
                blocks,
                ..Answers::default()
            },
            &mut None,
            ended,
            |_, _, _| true,
        )
    };
    let quiet = r#","human_ms":60000"#;
    for (name, row, col, blocks) in [
        (
            "unmarked-no-integration",
            "[me@box proj]$",
            14,
            "ERR unscripted",
        ),
        ("unmarked-read-builtin", "", 0, running),
    ] {
        let (got, typed) = run(name, screen(row, col, quiet), blocks);
        assert_eq!(got, Err(RelaunchLineError::Wait("unmarked")), "{name}");
        assert!(typed.is_empty(), "{name}: {typed:#?}");
        let (got, typed) = run(name, screen(row, col, r#","human_ms":200"#), blocks);
        assert_eq!(got, Err(RelaunchLineError::Wait("typing")), "{name}");
        assert!(typed.is_empty(), "{name}: {typed:#?}");
    }
    // The shape rule: a prompt that ends in a blank is typed at.
    let (got, typed) = run(
        "unmarked-blank",
        screen("me@box proj %", 14, quiet),
        "ERR unscripted",
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
}

/// `relaunch::end_asked_at`: only a restart that ended its agent itself (stamped before its
/// signal) dates a person's keys against its end; a relaunch on exit stamps after the
/// agent left and the back-off ran, so its end is unknown (`0`).
#[test]
fn only_a_restart_that_ended_its_agent_dates_a_persons_keys() {
    use super::super::relaunch::{
        CAUSE_EXIT, CAUSE_FRESH, CAUSE_HOST, CAUSE_MEMORY, CAUSE_STALL, CAUSE_UPGRADE_FRESH,
        end_asked_at,
    };
    let at = |cause: &str| {
        end_asked_at(&St {
            phase: Phase::Exiting {
                at_s: 1_790_880_325,
            },
            cause: cause.to_string(),
            ..St::default()
        })
    };
    for asked in [
        "",
        CAUSE_UPGRADE_FRESH,
        CAUSE_MEMORY,
        "model:opus",
        "model-back:",
    ] {
        assert_eq!(at(asked), 1_790_880_325, "{asked:?} ended its agent itself");
    }
    for seen in [
        CAUSE_EXIT,
        CAUSE_FRESH,
        CAUSE_STALL,
        CAUSE_HOST,
        "a-cause-added-later",
    ] {
        assert_eq!(at(seen), 0, "{seen:?} was filed after its agent left");
    }
    assert_eq!(
        end_asked_at(&St {
            phase: Phase::Relaunched {
                at_s: 1_790_880_325
            },
            ..St::default()
        }),
        0,
        "no end is asked once the line is typed"
    );
}

// ---------------------------------------------------------------- the owner's view and word

/// A `--now` ([`ask`]) waives exactly the settling window and the next visit types the
/// notice; until then the state keeps the wait (its start, the tab, the age) across
/// sweeps, and the act clears it. Negative control: with no request, nothing is typed.
#[cfg(unix)]
#[test]
fn an_owners_now_moves_a_settling_session_and_the_wait_is_kept_until_then() {
    let dir = scratch("owner-now");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let behind = now_s() - 600;
    let st = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        // The screen `instance` shows, first seen now: not yet quiet.
        last_seq: 77,
        seq_since_s: now_s(),
        pending_since: behind,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".to_string())];
    let run = || {
        visit(
            &opts,
            &sf,
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
        )
    };

    let first = run();
    let kept = load(&opts, SESSION).expect("state");
    let second = run();
    let still = load(&opts, SESSION).expect("state");
    let typed_before = turns(&asked);
    let row = ask(&opts, TAB, Ask::Now, false).expect("the owner's word lands");
    let moved = run();
    let after = load(&opts, SESSION).expect("state");
    let typed_after = turns(&asked);
    let _ = agent.kill();
    let _ = agent.wait();

    assert_eq!(first.step, "wait:settling");
    assert_eq!(second.step, "wait:settling");
    assert_eq!(typed_before, 0, "no request: the settling window stands");
    assert_eq!(kept.wait, "settling");
    assert!(kept.wait_since >= behind, "{kept:?}");
    assert_eq!(
        still.wait_since, kept.wait_since,
        "the same wait keeps its start"
    );
    assert_eq!(kept.tab, TAB, "a pending upgrade names its tab");
    assert_eq!(kept.behind_since(), behind);
    assert_eq!(row.request, Request::Now);
    assert_eq!(
        row.stall(now_s()),
        None,
        "ten minutes behind is not stalled"
    );
    assert_eq!(
        moved.step, "announced:1",
        "the owner's word waived the window"
    );
    assert_eq!(typed_after, 1);
    assert!(after.wait.is_empty(), "an act clears the kept wait");
    assert_eq!(after.behind_since(), behind, "the age is the session's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ladder on Claude Code through the real `visit` (whose `look` shares `ladder_look`
/// with the Codex visit), across two looks with the state rewound between them:
///
/// * a repaint at Settled announces on the second look (at Prefer it waits `settling`);
/// * a person near the tab at KeysOnly announces (at Settled it holds);
/// * a keystroke within `KEYS_GAP_S` at Land waits, look after look.
#[cfg(unix)]
#[test]
fn the_real_look_climbs_the_ladder_on_claude_code() {
    let near = idle_screen().replace(r#""human_ms":null"#, r#""human_ms":60000"#);
    let typing = idle_screen().replace(r#""human_ms":null"#, r#""human_ms":1000"#);
    let case = |name: &str, behind_s: u64, screen: &str, quiet: bool, looks: usize| {
        let dir = scratch(name);
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                screen: screen.to_string(),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            ..drive(&dir)
        };
        // The stand-in agent with an argv the relaunch plan can carry (no
        // harness flag of its own).
        let mut agent = parked_bare().spawn().expect("agent");
        wait_exec(agent.id());
        let sf = register(&opts.home, agent.id(), SESSION);
        file_transcript_in(&opts.home.join(".claude"), "p");
        let now = now_s();
        let st = St {
            phase: Phase::Pending,
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            // The screen `instance` shows is `seq` 77: last read as it is and
            // quiet a minute, or as 76 a moment ago (it just repainted).
            last_seq: if quiet { 77 } else { 76 },
            seq_since_s: if quiet { now - 60 } else { now },
            pending_since: now - behind_s,
            ..St::default()
        };
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
        let shell = dead_pid();
        let table = vec![(shell, 1, "zsh".to_string())];
        let mut steps = Vec::new();
        for n in 0..looks {
            if n > 0 {
                // QUIET_S (20 s) on: the still run began 20 s earlier, and
                // the screen repainted again unless it was quiet.
                let mut st = load(&opts, SESSION).expect("state");
                assert!(st.still_since != 0, "{name}: the look began a still run");
                st.still_since -= upgrade::QUIET_S;
                st.still_at -= upgrade::QUIET_S;
                if !quiet {
                    st.last_seq = 76;
                    st.seq_since_s = now_s();
                }
                std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
            }
            let r = visit(
                &opts,
                &sf,
                &table,
                &newer(),
                &Script::new(shell, usize::MAX, None),
            );
            steps.push(r.step);
        }
        let typed = turns(&asked);
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (steps, typed)
    };
    let settled = upgrade::RUNG_SETTLED_S + 60;
    // A repaint at Settled: the second look announces.
    assert_eq!(
        case("ladder-c-repaint", settled, &idle_screen(), false, 2),
        (
            vec!["wait:settling".to_string(), "announced:1".to_string()],
            1
        )
    );
    // Negative control: at Prefer the screen's own quiet is asked.
    assert_eq!(
        case("ladder-c-prefer", 600, &idle_screen(), false, 2),
        (
            vec!["wait:settling".to_string(), "wait:settling".to_string()],
            0
        )
    );
    // A person near the tab at KeysOnly: announced.
    assert_eq!(
        case("ladder-c-near", upgrade::RUNG_KEYS_S + 60, &near, true, 1),
        (vec!["announced:1".to_string()], 1)
    );
    // Negative control: the same person at Settled holds it.
    assert_eq!(
        case("ladder-c-near-settled", settled, &near, true, 1),
        (vec!["held-back:attended".to_string()], 0)
    );
    // A keystroke within KEYS_GAP_S at Land: waited out, look after look.
    assert_eq!(
        case(
            "ladder-c-typing",
            upgrade::RUNG_LAND_S + 60,
            &typing,
            true,
            2
        ),
        (
            vec![
                "held-back:attended".to_string(),
                "wait:attended".to_string()
            ],
            0
        )
    );
}

/// A `--now` waives the settling window only in the tab it named; recorded for another
/// tab it is no word here. Negative control: recorded for this tab, the notice goes at once.
#[cfg(unix)]
#[test]
fn the_owners_word_waives_nothing_in_a_tab_it_did_not_name() {
    let dir = scratch("owner-tab");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    // No `--exact` in its argv: the announcement's plan refuses a launch
    // flag it cannot carry into a resume.
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    file_transcript_in(&opts.home.join(".claude"), "p");
    let settling = |named: &str| St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        // The screen `instance` shows, first seen now: not yet quiet.
        last_seq: 77,
        seq_since_s: now_s(),
        request: Request::Now,
        request_tab: named.to_string(),
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".to_string())];
    let run = |named: &str| {
        std::fs::write(state_path(&opts, SESSION), settling(named).to_json()).expect("write");
        visit(
            &opts,
            &sf,
            &table,
            &newer(),
            &Script::new(shell, usize::MAX, None),
        )
    };
    let elsewhere = run("s-4906566e7ab0a0c15ee3");
    let typed_elsewhere = turns(&asked);
    let here = run(TAB);
    let typed_here = turns(&asked);
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(elsewhere.step, "wait:settling", "another tab's word");
    assert_eq!(typed_elsewhere, 0, "nothing typed on it");
    assert_eq!(
        here.step, "announced:1",
        "this tab's word waives the window"
    );
    assert_eq!(typed_here, 1);
}

/// A READY answer a hold (`--defer`, `--skip`) superseded is no consent: the hold ends the
/// notice (pending again, marker gone), so after the hold the next turn end gets a fresh
/// notice, never the signal. Negative control: the same READY with no hold is ended.
#[cfg(unix)]
#[test]
fn a_ready_answer_the_owners_hold_superseded_is_never_signalled_after_it() {
    use std::os::unix::process::ExitStatusExt as _;
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    for (case, word) in [("defer", Ask::Defer(600)), ("skip", Ask::Skip)] {
        let mut h = Parked::new(
            &format!("hold-superseded-{case}"),
            Answers::default(),
            St::default(),
            3600,
        );
        // The agent answered READY, the hold released its supervisor, and
        // the READY row is still the last answer the tail holds.
        transcript_says(&h, &format!("Saved; nothing is running.\\n{marker}"));
        let announced = St {
            phase: Phase::Announced {
                at_s: now_s() - 3_600,
                asks: 1,
            },
            to: "9.9.9".to_string(),
            ..announced_to(&h, marker)
        };
        std::fs::write(state_path(&h.opts, SESSION), announced.to_json()).expect("state");
        ask(&h.opts, TAB, word, false).expect("the owner's word lands");
        let held = load(&h.opts, SESSION).expect("state");
        assert_eq!(
            held.phase,
            Phase::Pending,
            "{case}: the hold ends the notice"
        );
        assert!(held.marker.is_empty(), "{case}: its marker with it");
        // The hold ends: the deferral runs out, or the owner says `--now`
        // over the skip.
        if case == "defer" {
            // Its screen settled since: the next turn end is one the
            // notice may be typed at.
            let lapsed = St {
                request: Request::DeferUntil(now_s() - 1),
                last_seq: 77,
                seq_since_s: now_s() - 3_600,
                ..held
            };
            std::fs::write(state_path(&h.opts, SESSION), lapsed.to_json()).expect("lapse");
        } else {
            ask(&h.opts, TAB, Ask::Now, false).expect("--now");
        }
        // The next turn end: a fresh notice, never the signal.
        let ended = h.reaped();
        let mut shell = tty_zsh();
        let r = h.visit_under(shell.id());
        let _ = shell.kill();
        let _ = shell.wait();
        assert!(ended.try_recv().is_err(), "{case}: never signalled: {r:?}");
        assert!(h.details("terminated").is_empty(), "{case}: {r:?}");
        assert_eq!(r.step, "announced:1", "{case}: a fresh notice");
        let fresh = load(&h.opts, SESSION).expect("state");
        assert!(
            !fresh.marker.is_empty() && fresh.marker != marker,
            "{case}: a fresh READY is asked for: {fresh:?}"
        );
    }
    // Negative control: no hold, the same READY at a settled point: ended.
    let mut h = Parked::new("hold-none", Answers::default(), St::default(), 3600);
    transcript_says(&h, &format!("Saved; nothing is running.\\n{marker}"));
    let announced = St {
        seq_since_s: now_s() - 3_600,
        last_seq: 77,
        ..announced_to(&h, marker)
    };
    std::fs::write(state_path(&h.opts, SESSION), announced.to_json()).expect("state");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert_eq!(h.details("terminated"), ["SIGTERM"], "{r:?}");
}

/// A READY session a person gave input in within the grace (`human_ms`) waits `attended`
/// however long past the re-ask clock, never signalled in front of them. Negative control:
/// `--now` ends it at once.
#[cfg(unix)]
#[test]
fn an_attended_ready_session_is_never_signalled_without_the_owners_now() {
    use std::os::unix::process::ExitStatusExt as _;
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let rig = |name: &str| {
        let h = Parked::new(
            name,
            Answers {
                screen: stamped_screen(Some("30000")),
                ..Answers::default()
            },
            St::default(),
            3600,
        );
        transcript_says(&h, &format!("Saved; nothing is running.\\n{marker}"));
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - upgrade::REASK_S - 60,
                asks: 1,
            },
            seq_since_s: now_s() - 3_600,
            last_seq: 77,
            ..announced_to(&h, marker)
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h
    };
    let mut h = rig("attended-ready");
    assert_eq!(h.visit().step, "held-back:attended");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let _ = shell.kill();
    let _ = shell.wait();
    assert!(ended.try_recv().is_err(), "never signalled: {r:?}");
    assert_eq!(r.step, "wait:attended");
    assert!(h.details("terminated").is_empty());
    drop(h);
    // Negative control: the owner's word.
    let mut h = rig("attended-ready-now");
    ask(&h.opts, TAB, Ask::Now, false).expect("--now");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert_eq!(h.details("terminated"), ["SIGTERM"], "{r:?}");
}

/// A newer target keeps how long the session has been behind and the request
/// word (except a skip, which is of one version) while nothing has begun; a
/// finished upgrade is history, and the next starts fresh.
#[test]
fn a_newer_target_keeps_the_age_and_the_owners_word_but_not_a_skip() {
    let from = Version::parse("2.1.280").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.283").expect("v"),
        source: Source::Managed,
    };
    let prior = St {
        phase: Phase::Announced { at_s: 5, asks: 1 },
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        request: Request::Now,
        request_tab: TAB.to_string(),
        ..St::default()
    };
    let next = St::for_target(Some(prior.clone()), &from, &to, None, Proc::default(), 900);
    assert_eq!(next.phase, Phase::Pending, "a new target is asked afresh");
    assert_eq!(next.to, "2.1.283");
    assert_eq!(next.behind_since(), 100);
    assert_eq!(next.request, Request::Now);
    assert_eq!(next.request_tab, TAB, "the word stays on the tab it named");
    assert_eq!(next.tab, TAB);
    let skipped = St {
        request: Request::Skip("2.1.282".to_string()),
        ..prior.clone()
    };
    let unskipped = St::for_target(Some(skipped), &from, &to, None, Proc::default(), 900);
    assert_eq!(
        unskipped.request,
        Request::None,
        "a skip of 2.1.282 is not a skip of 2.1.283"
    );
    assert!(unskipped.request_tab.is_empty(), "nor on any tab");
    let done = St {
        phase: Phase::Done,
        ..prior.clone()
    };
    let fresh = St::for_target(Some(done), &from, &to, None, Proc::default(), 900);
    assert_eq!(fresh.behind_since(), 900, "behind again from now");
    assert_eq!(fresh.request, Request::None);
    let same_target = Candidate {
        version: Version::parse("2.1.282").expect("v"),
        ..to.clone()
    };
    let same = St::for_target(
        Some(prior.clone()),
        &from,
        &same_target,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(same, prior, "the same target is the same upgrade");
    // The same build with a model the announcement did not name is another
    // restart: announced afresh, keeping how long the session has waited and
    // the request word.
    let remodel = St::for_target(
        Some(prior.clone()),
        &from,
        &same_target,
        Some("claude-opus-5-5"),
        Proc::default(),
        900,
    );
    assert_eq!(remodel.phase, Phase::Pending, "a new model is asked afresh");
    assert_eq!(remodel.behind_since(), 100);
    assert_eq!(remodel.request, Request::Now);
    let pending = St {
        phase: Phase::Pending,
        ..prior.clone()
    };
    assert_eq!(
        St::for_target(
            Some(pending.clone()),
            &from,
            &same_target,
            Some("claude-opus-5-5"),
            Proc::default(),
            900
        ),
        pending,
        "nothing announced yet: the pending state is the same upgrade"
    );
}

/// A process named by its kernel start alone.
fn proc(start: &str) -> Proc<'_> {
    Proc {
        start,
        started_s: 0,
    }
}

/// An age belongs to one process: a newer target carries a pending record's age only
/// while the same process is behind, told by kernel start (for an older build's record,
/// by the build it was found behind). Negative controls: the same process, or a record
/// naming neither, carries; stopped and finished rounds are not this arm's.
#[test]
fn an_age_belongs_to_one_process() {
    let running = Version::parse("2.1.283").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.284").expect("v"),
        source: Source::Managed,
    };
    let minted = |from: &str, proc_start: &str, phase: Phase| St {
        phase,
        from: from.to_string(),
        to: "2.1.283".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        proc_start: proc_start.to_string(),
        ..St::default()
    };
    // The record the hand restart left: minted behind the 2.1.274 process A;
    // the process now holding the conversation is B, on 2.1.283.
    for phase in [Phase::Pending, Phase::Announced { at_s: 5, asks: 1 }] {
        let stale = minted("2.1.274", "A", phase.clone());
        let next = St::for_target(Some(stale), &running, &to, None, proc("B"), 900);
        assert_eq!(next.behind_since(), 900, "{phase:?}: B's age begins now");
        assert_eq!(next.proc_start, "B");
        assert_eq!(next.from, "2.1.283");
    }
    // Legacy (no process named): the build it was found behind says it.
    let legacy = minted("2.1.274", "", Phase::Pending);
    let next = St::for_target(Some(legacy), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 900, "found behind another build");
    // Negative controls.
    let same_proc = minted("2.1.283", "B", Phase::Pending);
    let next = St::for_target(Some(same_proc), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 100, "the same process is still behind");
    let same_build = minted("2.1.283", "", Phase::Pending);
    let next = St::for_target(Some(same_build), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 100, "a legacy record, the same build");
    assert_eq!(next.proc_start, "B", "and it learns its process");
    let unknown = minted("", "", Phase::Pending);
    let next = St::for_target(Some(unknown), &running, &to, None, proc("B"), 900);
    assert_eq!(next.behind_since(), 100, "names nothing: carried as ever");
    // A pending record for the same target follows its process too: a session restarted
    // by hand onto another still-behind build is behind since the restart, with none of
    // the old process's waits.
    let waiting = St {
        wait: "background".to_string(),
        wait_since: 50,
        salt: 10,
        held_by: vec![HeldBy {
            pid: 7,
            name: "zsh".to_string(),
            since: 60,
        }],
        // What process A answered: three refused signals in a row.
        stop_streak: 3,
        streak_why: "signal-refused".to_string(),
        last_stop: "signal-refused".to_string(),
        refused: 3,
        refused_at: 40,
        ..minted("2.1.274", "A", Phase::Pending)
    };
    let target = Candidate {
        version: Version::parse("2.1.283").expect("v"),
        ..to.clone()
    };
    let from = Version::parse("2.1.275").expect("v");
    let next = St::for_target(Some(waiting.clone()), &from, &target, None, proc("B"), 900);
    assert_eq!(
        (
            next.behind_since(),
            next.from.as_str(),
            next.wait.as_str(),
            next.held_by.len()
        ),
        (900, "2.1.275", "", 0),
        "the record follows its process"
    );
    assert_eq!(
        (
            next.stop_streak,
            next.streak_why.as_str(),
            next.last_stop.as_str(),
            next.refused
        ),
        (0, "", "", 0),
        "and nothing the old process answered: B never refused a signal"
    );
    // Negative control: the same process keeps its streak (it is still the one
    // that stopped).
    let same = St {
        proc_start: "B".to_string(),
        from: "2.1.275".to_string(),
        ..waiting
    };
    let kept_same = St::for_target(Some(same), &from, &target, None, proc("B"), 900);
    assert_eq!(
        (kept_same.stop_streak, kept_same.streak_why.as_str()),
        (3, "signal-refused"),
        "the same process is still the one that stopped"
    );
    assert_eq!(
        (next.salt, next.proc_start.as_str()),
        (900, "B"),
        "its minting is the new process's too: the classifier's horizons count from it"
    );
    // An announced record of another process for the same target keeps its own process's
    // start, so the next build's record is minted afresh rather than read as the same
    // process one visit later.
    let announced = St {
        salt: 10,
        ..minted("2.1.274", "A", Phase::Announced { at_s: 50, asks: 1 })
    };
    let kept = St::for_target(Some(announced), &from, &target, None, proc("B"), 900);
    assert_eq!(kept.proc_start, "A", "not stamped with the holder's start");
    let target_next = Candidate {
        version: Version::parse("2.1.284").expect("v"),
        ..to.clone()
    };
    let later = St::for_target(Some(kept), &from, &target_next, None, proc("B"), 950);
    assert_eq!(
        later.behind_since(),
        950,
        "B's age begins when B's target does"
    );
    // An older build's record that predates the process now holding the conversation
    // cannot be its age: the age begins when this target was first seen (the record's
    // salt) or when the process began, whichever is later.
    let target_284 = Candidate {
        version: Version::parse("2.1.284").expect("v"),
        ..to.clone()
    };
    let old_record = St {
        phase: Phase::Announced { at_s: 650, asks: 1 },
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        salt: 700,
        ..St::default()
    };
    let born = |started_s| Proc {
        start: "B",
        started_s,
    };
    for (started_s, want) in [(500, 700), (800, 800), (0, 100)] {
        let next = St::for_target(
            Some(old_record.clone()),
            &running,
            &target_284,
            None,
            born(started_s),
            900,
        );
        assert_eq!(
            next.behind_since(),
            want,
            "the process began at {started_s}"
        );
        assert_eq!(next.phase, old_record.phase, "the notice stands");
        assert_eq!(next.proc_start, "B", "and the record learns its process");
    }
    let after = St {
        pending_since: 600,
        ..old_record
    };
    let next = St::for_target(Some(after), &running, &target_284, None, born(500), 900);
    assert_eq!(
        next.behind_since(),
        600,
        "an age from within the process stands"
    );
    // A stopped round is about its process or launch line: kept as it was for the
    // process it was about ...
    let stopped = minted("2.1.275", "B", Phase::Failed("no-resume".to_string()));
    let next = St::for_target(Some(stopped.clone()), &from, &target, None, proc("B"), 900);
    assert_eq!((next.phase, next.pending_since), (stopped.phase, 100));
    // ... and not for another: a session restarted by hand onto this still-behind build
    // never had that round, and is asked afresh, from now.
    let of_a = St {
        wait: "failed".to_string(),
        held_by: vec![HeldBy {
            pid: 7,
            name: "zsh".to_string(),
            since: 60,
        }],
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 3,
        own_work_at: 80,
        ..minted("2.1.274", "A", Phase::Failed(upgrade::GAVE_UP.to_string()))
    };
    let next = St::for_target(Some(of_a), &from, &target, None, proc("B"), 900);
    assert_eq!(
        (
            next.phase.clone(),
            next.behind_since(),
            next.held_by.len(),
            next.stop_streak,
            next.own_work_at
        ),
        (Phase::Pending, 900, 0, 0, 0),
        "another process starts afresh"
    );
    assert_eq!(next.proc_start, "B");
}

/// A newer build landing in a rest keeps the same process's age, unanswered rounds, wait
/// log and last work seen (Claude Code ships about daily, so builds land in rests);
/// another process, or a finished upgrade, starts fresh.
#[test]
fn a_newer_build_landing_in_a_rest_keeps_the_process_s_history() {
    let running = Version::parse("2.1.283").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.285").expect("v"),
        source: Source::Managed,
    };
    let rested = |proc_start: &str, phase: Phase| St {
        phase,
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        proc_start: proc_start.to_string(),
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 2,
        own_work_at: 700,
        waits: vec![WaitNote {
            at: 650,
            word: "background".to_string(),
            note: "holders=3".to_string(),
        }],
        ..St::default()
    };
    let gave_up = Phase::Failed(upgrade::GAVE_UP.to_string());
    let next = St::for_target(
        Some(rested("B", gave_up.clone())),
        &running,
        &to,
        None,
        proc("B"),
        900,
    );
    assert_eq!(next.phase, Phase::Pending, "a new target is asked afresh");
    assert_eq!(next.to, "2.1.285");
    assert_eq!(next.behind_since(), 100, "the same process keeps its age");
    assert_eq!(
        (next.streak_why.as_str(), next.stop_streak, next.own_work_at),
        (upgrade::GAVE_UP, 2, 700),
        "and the rounds it answered not yet"
    );
    assert_eq!(next.waits.len(), 1, "and why it has waited");
    // Any other stop in a row continues with the process too: every `Failed`
    // reason is about the process or its launch line, so a new build is no
    // reason to rest it less (one rule for every stop).
    let refused = St {
        streak_why: "no-resume".to_string(),
        stop_streak: 2,
        ..rested("B", Phase::Failed("no-resume".to_string()))
    };
    let next = St::for_target(Some(refused), &running, &to, None, proc("B"), 900);
    assert_eq!(
        (
            next.behind_since(),
            next.streak_why.as_str(),
            next.stop_streak
        ),
        (100, "no-resume", 2)
    );
    // Negative controls: another process, and a finished upgrade.
    let next = St::for_target(
        Some(rested("A", gave_up)),
        &running,
        &to,
        None,
        proc("B"),
        900,
    );
    assert_eq!(
        (next.behind_since(), next.stop_streak, next.waits.len()),
        (900, 0, 0),
        "another process starts fresh"
    );
    let next = St::for_target(
        Some(rested("B", Phase::Done)),
        &running,
        &to,
        None,
        proc("B"),
        900,
    );
    assert_eq!(
        (next.behind_since(), next.stop_streak, next.own_work_at),
        (900, 0, 0),
        "a finished upgrade is history"
    );
}

/// Tier-1: `harness_upgrade_age_model` (aterm-spec) on every reachable state and enabled
/// action against the real code ([`St::for_target`], [`forget_settled_pending`], the
/// owner's view's `rows`). Negative control: the model's `Buggy = 1` breaks
/// `AgeIsThisProcess`.
#[test]
fn the_real_age_conforms_to_the_age_model() {
    use aterm_spec::derive::harness_upgrade_age_model;
    use aterm_spec::interp;
    let model = harness_upgrade_age_model();
    let day = STALLED_AFTER_S;
    let now = 10 * day;
    let v = |t: &str| Version::parse(t).expect("version");
    let target = |t: &str| Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: v(t),
        source: Source::Managed,
    };
    let holder = |proc: i64| if proc == 0 { "A" } else { "B" };
    let dir = scratch("age-model");
    let opts = drive(&dir);
    // The model's state -> the record it stands for, in the world it stands
    // for: process 0 runs 2.1.274 behind 2.1.283; process 1 runs 2.1.283
    // (behind 2.1.284 once a newer build has landed).
    let record = |s: &std::collections::BTreeMap<&'static str, i64>| -> Option<St> {
        let counts = s["rec"] - 1;
        (s["rec"] > 0).then(|| St {
            phase: Phase::Pending,
            from: if counts == 0 { "2.1.274" } else { "2.1.283" }.to_string(),
            to: if counts == 0 { "2.1.283" } else { "2.1.284" }.to_string(),
            tab: TAB.to_string(),
            proc_start: holder(counts).to_string(),
            pending_since: if s["aged"] == 1 { now - day } else { now - 60 },
            ..St::default()
        })
    };
    let running = |s: &std::collections::BTreeMap<&'static str, i64>| {
        // Where the holder stands, as `record` says.
        if s["proc"] == 0 {
            v("2.1.274")
        } else {
            v("2.1.283")
        }
    };
    let project = |st: Option<&St>| -> (i64, i64) {
        st.map_or((0, 0), |st| {
            (
                if st.proc_start == "A" { 1 } else { 2 },
                i64::from(now.saturating_sub(st.behind_since()) >= day),
            )
        })
    };
    let alarm = |st: &St| -> i64 {
        save(&opts, SESSION, st);
        i64::from(
            rows(&opts)
                .iter()
                .any(|r| r.session == SESSION && r.stall(now).as_deref() == Some("overdue")),
        )
    };
    // The reachable states.
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([model.init_state()]);
    let mut states = Vec::new();
    while let Some(s) = queue.pop_front() {
        if !seen.insert(s.clone()) {
            continue;
        }
        for a in &model.actions {
            let mut next = s.clone();
            if model.fire(a.name, &mut next) {
                queue.push_back(next);
            }
        }
        states.push(s);
    }
    assert!(states.len() > 5, "{} states", states.len());
    let mut fired = std::collections::BTreeSet::new();
    for s in &states {
        for inv in &model.invariants {
            assert!(model.check_invariant(inv.name, s), "{} at {s:?}", inv.name);
        }
        for a in &model.actions {
            let mut next = s.clone();
            if !model.fire(a.name, &mut next) {
                continue;
            }
            fired.insert(a.name);
            let at = format!("{} at {s:?}", a.name);
            let rec = record(s);
            let real: (i64, i64, i64) = match a.name {
                // The bound passes on the record.
                "Time" => {
                    let st = St {
                        pending_since: now - day,
                        ..rec.expect("a record")
                    };
                    let (r, aged) = project(Some(&st));
                    (r, aged, s["alarm"])
                }
                // A new process, on the newest build; the record is untouched.
                "HandRestart" => {
                    let (r, aged) = project(rec.as_ref());
                    (r, aged, 0)
                }
                // The next step finds nothing to move: the record for a build
                // the session runs is closed.
                "Visit" => {
                    let st = rec.expect("a record");
                    save(&opts, SESSION, &st);
                    let closed =
                        forget_settled_pending(&opts, SESSION, Some(&st), &v("2.1.283")).is_some();
                    let (r, aged) = project(load(&opts, SESSION).as_ref());
                    assert!(closed || load(&opts, SESSION).is_none(), "{at}");
                    (r, aged, 0)
                }
                // A newer build lands: retargeted onto the holder (process 1
                // on 2.1.283) or minted for it.
                "NewBuild" => {
                    let st = St::for_target(
                        rec,
                        &running(s),
                        &target("2.1.284"),
                        None,
                        proc(holder(s["proc"])),
                        now,
                    );
                    let (r, aged) = project(Some(&st));
                    (r, aged, s["alarm"])
                }
                // The owner's view judges the age of a behind session.
                "Judge" => {
                    let st = rec.expect("a record");
                    let (r, aged) = project(Some(&st));
                    (r, aged, alarm(&st))
                }
                other => panic!("an action the bind does not drive: {other}"),
            };
            // The real code must match the fixed model (`Visit`, absent from the buggy
            // one, is driven too).
            assert_eq!(
                (next["rec"], next["aged"], next["alarm"]),
                real,
                "the real code left another state than the model at {at}"
            );
        }
    }
    assert_eq!(
        fired,
        std::collections::BTreeSet::from(["HandRestart", "Judge", "NewBuild", "Time", "Visit"]),
        "every action of the model is driven"
    );
    // Negative control: the model's buggy variant breaks the property on a reachable state.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in ["Time", "HandRestart", "NewBuild", "Judge"] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(!buggy.check_invariant("AgeIsThisProcess", &b), "{b:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A stopped upgrade retargeted keeps its move clock, the owner's word and its stop
/// streak, so a re-armed tab's `behind_since` survives each new build and a `--defer`
/// holds. Negative controls: a skip is of one build; a session moved since
/// ([`behind_anew`]) or a finished upgrade begins again from now.
#[test]
fn a_stopped_upgrade_retargeted_keeps_its_clock_word_and_streak() {
    let from = Version::parse("2.1.280").expect("v");
    let to = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.283").expect("v"),
        source: Source::Managed,
    };
    let prior = St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        from: "2.1.280".to_string(),
        to: "2.1.282".to_string(),
        source: "managed".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        request: Request::DeferUntil(5_000),
        request_tab: TAB.to_string(),
        request_at: 50,
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 3,
        ..St::default()
    };
    let next = St::for_target(Some(prior.clone()), &from, &to, None, Proc::default(), 900);
    assert_eq!(next.phase, Phase::Pending, "a new target is asked afresh");
    assert_eq!(next.behind_since(), 100, "the move clock is kept");
    assert_eq!(
        next.request,
        Request::DeferUntil(5_000),
        "the defer is kept"
    );
    assert_eq!((next.request_tab.as_str(), next.request_at), (TAB, 50));
    assert_eq!(
        (next.streak_why.as_str(), next.stop_streak),
        (upgrade::GAVE_UP, 3),
        "the same stop in a row is kept"
    );
    assert_eq!(next.tab, TAB);
    let skipped = St::for_target(
        Some(St {
            request: Request::Skip("2.1.282".to_string()),
            ..prior.clone()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(skipped.request, Request::None, "a skip is of one build");
    assert!(skipped.request_tab.is_empty());
    assert_eq!(skipped.behind_since(), 100);
    let moved = St::for_target(
        Some(St {
            from: "2.1.279".to_string(),
            ..prior.clone()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(moved.behind_since(), 900, "moved since: behind from now");
    assert_eq!(
        moved.request,
        Request::DeferUntil(5_000),
        "the owner's snooze is the session's, not the process's"
    );
    assert_eq!(moved.stop_streak, 0);
    // ... but a standing `--now` hurried the process it was given for: a
    // session the owner has just restarted by hand is one they are at.
    let moved_now = St::for_target(
        Some(St {
            from: "2.1.279".to_string(),
            request: Request::Now,
            ..prior.clone()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(moved_now.request, Request::None);
    let same_now = St::for_target(
        Some(St {
            request: Request::Now,
            ..prior.clone()
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(same_now.request, Request::Now, "the process it hurried");
    let done = St::for_target(
        Some(St {
            phase: Phase::Done,
            ..prior
        }),
        &from,
        &to,
        None,
        Proc::default(),
        900,
    );
    assert_eq!(done.behind_since(), 900, "a finished one: behind from now");
    assert_eq!(done.request, Request::None);
    assert_eq!(done.stop_streak, 0);
}

/// The label follows the build until the restart begins: managed and native
/// builds share a version, and a state minted for one must not keep naming it
/// after the sweep moves to the other. Once the restart is in flight, or the
/// upgrade failed, the label is history and stays.
#[test]
fn a_reused_state_names_the_build_the_sweep_would_start_now() {
    let from = Version::parse("2.1.280").expect("v");
    let managed = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.282").expect("v"),
        source: Source::Managed,
    };
    for phase in [Phase::Pending, Phase::Announced { at_s: 5, asks: 1 }] {
        let prior = St {
            phase: phase.clone(),
            to: "2.1.282".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        let next = St::for_target(
            Some(prior.clone()),
            &from,
            &managed,
            None,
            Proc::default(),
            900,
        );
        assert_eq!(next.source, "managed", "{phase:?}: relabelled");
        assert_eq!(
            St {
                source: prior.source.clone(),
                ..next
            },
            prior,
            "{phase:?}: nothing but the label moved"
        );
    }
    for phase in [
        Phase::Exiting { at_s: 5 },
        Phase::Failed("no-resume".to_string()),
    ] {
        let prior = St {
            phase: phase.clone(),
            to: "2.1.282".to_string(),
            source: "native".to_string(),
            ..St::default()
        };
        let next = St::for_target(
            Some(prior.clone()),
            &from,
            &managed,
            None,
            Proc::default(),
            900,
        );
        assert_eq!(next, prior, "{phase:?}: begun or final, kept as it was");
    }
}

/// [`prune_states`] removes a settled upgrade of a conversation no live Claude holds once
/// it has sat [`STATE_KEEP_S`], and nothing else; a dry run removes nothing.
#[test]
fn settled_states_of_dead_conversations_are_pruned_after_a_week_and_nothing_else() {
    let dir = scratch("prune-states");
    let opts = drive(&dir);
    let me = std::process::id();
    let live = register(&opts.home, me, SESSION);
    let settled = |phase: Phase| St {
        phase,
        to: "2.1.282".to_string(),
        ..St::default()
    };
    let gone = [
        ("dead-done", settled(Phase::Done)),
        (
            "dead-failed",
            settled(Phase::Failed("unanswered".to_string())),
        ),
        ("dead-pending", settled(Phase::Pending)),
    ];
    let kept = [
        (SESSION, settled(Phase::Done)),
        (
            "dead-announced",
            settled(Phase::Announced { at_s: 5, asks: 1 }),
        ),
        ("dead-exiting", settled(Phase::Exiting { at_s: 5 })),
        (
            "dead-skipped",
            St {
                request: Request::Skip("2.1.282".to_string()),
                ..settled(Phase::Pending)
            },
        ),
        (
            "codex-s-feed",
            St {
                agent: upgrade::Agent::Codex,
                ..settled(Phase::Done)
            },
        ),
    ];
    for (id, st) in gone.iter().chain(kept.iter()) {
        save(&opts, id, st);
    }
    let models = state_dir(&opts).join("models.json");
    std::fs::write(&models, "{}").expect("models");
    let files = [live];
    let week = STATE_KEEP_S;
    let present = |id: &str| state_path(&opts, id).exists();

    // A day old: nothing has sat long enough.
    prune_states(&opts, &files, now_s() + 24 * 60 * 60);
    assert!(gone.iter().chain(kept.iter()).all(|(id, _)| present(id)));
    // A dry run removes nothing, however old.
    prune_states(
        &Opts {
            dry_run: true,
            ..drive(&dir)
        },
        &files,
        now_s() + 2 * week,
    );
    assert!(gone.iter().chain(kept.iter()).all(|(id, _)| present(id)));

    prune_states(&opts, &files, now_s() + week + 60);
    for (id, _) in &gone {
        assert!(!present(id), "{id}: settled, dead and a week old");
    }
    for (id, _) in &kept {
        assert!(present(id), "{id}: kept");
    }
    assert!(
        models.exists(),
        "a file that is no state is not the prune's"
    );
    // The explicit restart's stamps: no reader looks one up once its tab is gone, so the
    // prune removes one once stale, never while it stands.
    let stamp_at = |at| super::super::restart_stamp::Stamp {
        pid: dead_pid(),
        start: "Thu Jan 1 00:00:00 1970".to_string(),
        cause: super::super::restart_stamp::Cause::Asked,
        at,
    };
    let at = now_s();
    let fresh_at = at + super::super::relaunch::STALE_S;
    super::super::restart_stamp::write(&opts.state, "s-old", &stamp_at(at)).expect("stamp");
    super::super::restart_stamp::write(&opts.state, "s-new", &stamp_at(fresh_at)).expect("stamp");
    prune_states(&opts, &files, fresh_at + 1);
    assert_eq!(
        super::super::restart_stamp::read(&opts.state, "s-old"),
        None,
        "a stale stamp is pruned"
    );
    assert!(
        super::super::restart_stamp::read(&opts.state, "s-new").is_some(),
        "a standing stamp is kept"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A record of an agent this build has no lane for is a newer build's
/// ([`upgrade::Agent::parse`]): every scan of the records passes it by and leaves its file as it
/// lies (the load, the sweep's prunes, its orphan pass and limit hold, the host's carry and the
/// continuation it owes, the owner's view and word, the watch). Each such record is the Codex
/// record beside it, text for text but for its word, under a conversation's name, a tab's, and
/// the Codex lane's own. Negative control: the same scans read the Codex records (and those
/// that write, write them), and Claude Code's prune removes Claude's settled record.
#[test]
fn a_record_of_an_unknown_agent_is_passed_by_and_left_as_it_lies() {
    use super::super::relaunch::{carry_restart, restarted};
    const THEIRS: &str = "s-9e31a1c0ffee0000beef";
    const CODEX: &str = "s-c0dec0dec0dec0dec0de";
    let dir = scratch("unknown-agent");
    let opts = Opts {
        sock: None,
        ..drive(&dir)
    };
    let in_tab = |tab: &str| Opts {
        only_sid: Some(tab.to_string()),
        ..opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let now = now_s();
    // The Codex TUI its `/exit` was typed into, gone.
    let tui = dead_pid();
    let codex = |tab: &str, phase: Phase| St {
        agent: upgrade::Agent::Codex,
        phase,
        pid: tui,
        tab: tab.to_string(),
        from: "0.157.0".to_string(),
        to: "0.157.1".to_string(),
        source: "managed".to_string(),
        ready_since: now - 10,
        ..St::default()
    };
    // The Codex record's text under another word.
    let worded = |st: &St, word: &str| {
        let text = st.to_json();
        let out = text.replace(r#""agent":"codex""#, &format!(r#""agent":"{word}""#));
        assert_ne!(out, text, "the word is the record's");
        out
    };
    let plant = |name: &str, text: &str| {
        std::fs::write(state_path(&opts, name), text).expect("plant");
    };
    let mut theirs: Vec<(String, String)> = Vec::new();
    let mut plant_theirs = |name: String, st: &St| {
        let text = worded(st, "gemini");
        plant(&name, &text);
        theirs.push((name, text));
    };
    let codex_name = format!("codex-{CODEX}");

    // Settled, a week old, no live conversation holding any, every tab gone.
    let claude_done = "5d1c7f0e-2b8a-4c3d-9e6f-1a2b3c4d5e6f";
    plant(claude_done, &worded(&codex(THEIRS, Phase::Done), ""));
    plant_theirs(
        "7a9e0d1b-3c4d-4e5f-8a9b-0c1d2e3f4a5b".to_string(),
        &codex(THEIRS, Phase::Done),
    );
    plant_theirs(format!("codex-{THEIRS}"), &codex(THEIRS, Phase::Done));
    plant(&codex_name, &codex(CODEX, Phase::Done).to_json());
    // A restart in flight: relaunched, its continuation owed, its READY clock running.
    let relaunched = Phase::Relaunched { at_s: now };
    plant_theirs(
        format!("gemini-{THEIRS}"),
        &codex(THEIRS, relaunched.clone()),
    );
    let as_it_lies = |theirs: &[(String, String)]| {
        for (name, text) in theirs {
            let on_disk = std::fs::read_to_string(state_path(&opts, name)).ok();
            assert_eq!(on_disk.as_deref(), Some(text.as_str()), "{name}");
        }
    };
    for (name, _) in &theirs {
        assert_eq!(load(&opts, name), None, "{name}");
    }
    assert_eq!(
        load(&opts, claude_done).map(|st| st.agent),
        Some(upgrade::Agent::Claude)
    );
    assert_eq!(
        load(&opts, &codex_name).map(|st| st.agent),
        Some(upgrade::Agent::Codex)
    );
    let week_on = now + STATE_KEEP_S + 60;
    prune_states(&opts, &[], week_on);
    assert!(
        !state_path(&opts, claude_done).exists(),
        "control: Claude Code's settled record is pruned"
    );
    codex::prune(&opts, week_on, |_| codex::TabLife::Gone);
    assert!(
        !state_path(&opts, &codex_name).exists(),
        "control: the Codex lane's settled record is pruned"
    );
    as_it_lies(&theirs);

    plant(&codex_name, &codex(CODEX, relaunched).to_json());
    assert_eq!(in_flight(&opts), [codex_name.as_str()], "control");
    // Claude Code's orphan pass: the Codex record is its own lane's.
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    let orphaned = orphans(&dry, &[], None);
    assert!(orphaned.is_empty(), "{orphaned:?}");
    let (mine, cdx) = (in_tab(THEIRS), in_tab(CODEX));
    for lane in [upgrade::Agent::Codex, upgrade::Agent::Claude] {
        assert!(!restarted(&mine, lane, Some(tui)));
        assert_eq!(
            carry_restart(&mine, lane, Some(tui)).step,
            "refused:no-restart-in-flight"
        );
    }
    assert!(!owed(&mine));
    assert_eq!(resume(&mine).step, "current");
    assert!(
        restarted(&cdx, upgrade::Agent::Codex, Some(tui)),
        "control: carried"
    );
    assert!(owed(&cdx), "control: its continuation owed");
    as_it_lies(&theirs);
    assert!(hold_clock(&mine, now + 600));
    as_it_lies(&theirs);
    assert!(hold_clock(&cdx, now + 600));
    assert_eq!(
        load(&opts, &codex_name).map(|st| st.ready_since),
        Some(now + 600),
        "control: the Codex record's clock is held"
    );
    let rows: Vec<String> = super::upgrade_status::rows(&opts)
        .into_iter()
        .map(|row| row.session)
        .collect();
    assert_eq!(rows, [codex_name.as_str()], "control: the owner's view");
    let word = |tab: &str| {
        super::upgrade_status::ask(&opts, tab, super::upgrade_status::Ask::Skip, false)
            .expect_err("nothing to skip")
    };
    let refused = word(THEIRS);
    assert!(refused.starts_with("no upgrade is recorded"), "{refused}");
    let under_way = word(CODEX);
    assert!(
        under_way.contains("already under way"),
        "control: {under_way}"
    );
    as_it_lies(&theirs);
    let told = Watcher {
        told: true,
        ..Watcher::default()
    };
    let read = |o: &Opts| Report {
        pid: 0,
        tab: o.only_sid.clone().unwrap_or_default(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: "wait:not-idle".to_string(),
    };
    let looked = |tab: &str| watch_with(&opts, tab, &told, now, &read);
    assert_eq!(looked(THEIRS).skipped.as_deref(), Some("no-record"));
    let control = looked(CODEX);
    assert_ne!(
        control.skipped.as_deref(),
        Some("no-record"),
        "control: {control:?}"
    );
    as_it_lies(&theirs);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What a report says about waiting: a wait is recorded with Claude's status
/// beside `not-idle` and keeps its start while it repeats; an act clears it;
/// a skip or a dry run's `would-` changes nothing.
#[test]
fn a_report_records_its_wait_and_an_act_clears_it() {
    let mut st = St::default();
    st.note_step("wait:not-idle", "busy", 10);
    assert_eq!((st.wait.as_str(), st.wait_since), ("not-idle:busy", 10));
    st.note_step("wait:not-idle", "busy", 70);
    assert_eq!(st.wait_since, 10, "the same wait keeps its start");
    st.note_step("wait:not-idle", "shell", 130);
    assert_eq!((st.wait.as_str(), st.wait_since), ("not-idle:shell", 130));
    st.note_step("skip:not-selected", "idle", 190);
    st.note_step("would-announce", "idle", 190);
    assert_eq!(st.wait, "not-idle:shell", "neither is an act");
    st.note_step("announced:1", "idle", 250);
    assert!(st.wait.is_empty() && st.wait_since == 0);
    // The attended guard's wait is said once as `held-back:attended`, then
    // `wait:attended`: one wait from its first sweep, never an act between.
    st.note_step("held-back:attended", "idle", 300);
    assert_eq!((st.wait.as_str(), st.wait_since), ("attended", 300));
    assert_eq!(st.wait_seen, 300);
    st.note_step("wait:attended", "idle", 360);
    assert_eq!(st.wait_since, 300, "the same wait keeps its start");
    // …and each look that finds the person is recorded (the owner's view
    // names them only while one has lately: `Row::wait_seen`).
    assert_eq!(st.wait_seen, 360, "the last look that saw the person");
    // Another wait carries no look time: its rows change only with the upgrade.
    st.note_step("wait:settling", "idle", 380);
    assert_eq!(st.wait_seen, 0);
    st.note_step("wait:attended", "idle", 400);
    // The give-up is an act.
    st.note_step("gave-up", "idle", 420);
    assert!(st.wait.is_empty() && st.wait_since == 0 && st.wait_seen == 0);
}

/// The ledger is bounded: past the disk journal's 1 MiB it is cut back to its
/// newest rows at the next write, the newest row the one just written.
/// Negative control: a small ledger is only appended to.
#[test]
fn the_upgrade_ledger_is_cut_back_to_its_newest_rows() {
    let dir = scratch("ledger-bound");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let r = |step: &str| Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1.0.0".to_string(),
        to: "9.9.9(managed)".to_string(),
        step: step.to_string(),
    };
    ledger(&opts, &r("announced:1"), "small");
    ledger(&opts, &r("announced:2"), "small");
    assert_eq!(ledger_lines(&opts), 2, "a small ledger is appended to");
    let row = format!(
        "{{\"step\":\"old\",\"detail\":\"{}\"}}\n",
        "x".repeat(1_000)
    );
    let big: String = std::iter::repeat_n(row.as_str(), 1_200).collect();
    std::fs::write(ledger_path(&opts), big).expect("a ledger past 1 MiB");
    ledger(&opts, &r("done"), "the newest");
    let text = std::fs::read_to_string(ledger_path(&opts)).expect("ledger");
    assert_eq!(text.lines().count(), LEDGER_KEEP_ROWS);
    assert!(
        text.lines()
            .last()
            .is_some_and(|l| l.contains(r#""step":"done""#)),
        "the newest row survives the cut"
    );
    assert!(text.len() < 1_200 * row.len(), "the file shrank");
    let _ = std::fs::remove_dir_all(dir);
}

/// The model on the relaunch line: every spelling of a launch `--model` is
/// replaced by the list's model, placed before `--resume <id>`, which stays
/// last; with no model asked for, the flags are untouched; an empty one drops
/// them.
#[test]
fn a_listed_model_replaces_every_launch_model_spelling() {
    use super::super::relaunch::with_model;
    let v = |xs: &[&str]| xs.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    assert_eq!(
        with_model(
            v(&[
                "--model",
                "opus",
                "--verbose",
                "--model=sonnet",
                "--resume",
                "ID"
            ]),
            Some("claude-opus-5-5")
        ),
        v(&["--verbose", "--model", "claude-opus-5-5", "--resume", "ID"])
    );
    assert_eq!(
        with_model(v(&["--resume", "ID"]), Some("claude-fable-5-1")),
        v(&["--model", "claude-fable-5-1", "--resume", "ID"])
    );
    let kept = v(&["--model", "opus", "--resume", "ID"]);
    assert_eq!(with_model(kept.clone(), None), kept);
    // An empty model drops the launch's (a bucket's way back to the default): no `--model`,
    // `--resume` still last.
    assert_eq!(
        with_model(
            v(&[
                "--model",
                "opus",
                "--verbose",
                "--model=x",
                "--resume",
                "ID"
            ]),
            Some("")
        ),
        v(&["--verbose", "--resume", "ID"])
    );
    let line = line_for(
        Dialect::Zsh,
        None,
        None,
        std::path::Path::new("/x/claude"),
        &v(&[
            "claude",
            "--model",
            "claude-opus-5",
            "--dangerously-skip-permissions",
        ]),
        Some("446a0b3c-1979-414f-ae20-e06e26a2ef44"),
        Some("claude-opus-5-5"),
        None,
        &[],
    )
    .unwrap_or_else(|_| panic!("a line"));
    assert!(
        line.contains("'--model' 'claude-opus-5-5'") && !line.contains("'claude-opus-5'"),
        "{line}"
    );
}

/// The relaunch argv end to end, by the calls the upgrade, the relaunch on exit and the
/// restart in place make ([`upgrade::launch_model`], [`models::model_due`], [`line_for`])
/// over a launch's argv, the conversation's model and what the build offers.
#[test]
fn the_relaunch_line_moves_the_model_within_its_family_first_and_keeps_the_rest() {
    const ID: &str = "446a0b3c-1979-414f-ae20-e06e26a2ef44";
    let offered: Vec<String> = [
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-fable-5-1",
        "claude-opus-5",
    ]
    .map(String::from)
    .to_vec();
    let relaunch = |argv: &[&str], live: &str, offered: &[String]| -> String {
        let argv: Vec<String> = argv.iter().map(|w| (*w).to_string()).collect();
        let launch = upgrade::launch_model(&argv);
        let live = models::LiveModel {
            id: live.to_string(),
            by_command: false,
        };
        let asked = match models::model_due(
            &Priority::seed(0),
            Some(&live),
            offered,
            launch.as_deref(),
            None,
            &ModelRecord::default(),
        ) {
            ModelVerdict::Due { to, .. } => Some(to),
            ModelVerdict::Keep(_) => None,
        };
        line_for(
            Dialect::Bash,
            None,
            None,
            std::path::Path::new("/x/claude"),
            &argv,
            Some(ID),
            asked.as_deref(),
            None,
            &[],
        )
        .unwrap_or_else(|_| panic!("a line for {argv:?}"))
    };
    let line = |words: &[&str]| {
        // Every Claude relaunch exports the survey switch, ahead of the program.
        std::iter::once("/x/claude")
            .chain(words.iter().copied())
            .chain(["--resume", ID])
            .map(|w| format!(" '{w}'"))
            .collect::<String>()
            .replacen(' ', " CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 ", 1)
    };
    let rows: [(&[&str], &str, &[&str]); 10] = [
        // Nothing named: the newest of the family asked for.
        (
            &["claude", "--dangerously-skip-permissions"],
            "claude-opus-5",
            &[
                "--dangerously-skip-permissions",
                "--model",
                "claude-opus-5-5",
            ],
        ),
        // An exact id the launch pinned moves within its family; every other
        // flag, `--fallback-model` included, rides verbatim.
        (
            &[
                "claude",
                "--fallback-model",
                "claude-opus-5",
                "--model",
                "claude-opus-5",
            ],
            "claude-opus-5",
            &[
                "--fallback-model",
                "claude-opus-5",
                "--model",
                "claude-opus-5-5",
            ],
        ),
        // The 1M window rides the move, from an inline spelling too.
        (
            &["claude", "--model=claude-opus-5[1m]"],
            "claude-opus-5",
            &["--model", "claude-opus-5-5[1m]"],
        ),
        // A family the list never named, by the build's own newest.
        (
            &["claude", "--model", "claude-sonnet-5"],
            "claude-sonnet-5",
            &["--model", "claude-sonnet-5-5"],
        ),
        // An alias is kept verbatim: the build resolves it to its newest.
        (
            &["claude", "--model", "opus"],
            "claude-opus-5",
            &["--model", "opus"],
        ),
        (
            &["claude", "--model", "sonnet[1m]"],
            "claude-sonnet-5",
            &["--model", "sonnet[1m]"],
        ),
        // An id outside the grammar is untouched: never guessed at.
        (
            &["claude", "--model", "us.anthropic.claude-opus-5-v1:0"],
            "claude-opus-5",
            &["--model", "us.anthropic.claude-opus-5-v1:0"],
        ),
        // Nothing newer of its family, and a Fable nobody chose: the list's
        // step moves it up to the Opus 5.5 ranked above it...
        (
            &["claude", "--verbose"],
            "claude-fable-5-1",
            &["--verbose", "--model", "claude-opus-5-5"],
        ),
        // ...but a Fable the launch chose is never moved across families.
        (
            &["claude", "--model", "claude-fable-5-1"],
            "claude-fable-5-1",
            &["--model", "claude-fable-5-1"],
        ),
        // A family with nothing on offer, off the list: no `--model` added.
        (&["claude", "--verbose"], "claude-haiku-4-5", &["--verbose"]),
    ];
    for (argv, live, want) in rows {
        assert_eq!(relaunch(argv, live, &offered), line(want), "{argv:?}");
    }
    // Never down: the build offers only an older Opus, and a Fable the list
    // ranks below Opus 5.5, which the list's step never takes.
    assert_eq!(
        relaunch(
            &["claude", "--verbose"],
            "claude-opus-5-5",
            &["claude-opus-5".to_string(), "claude-fable-5-1".to_string()]
        ),
        line(&["--verbose"])
    );
    // Nothing newer of Opus 5's family (Opus 5.5 not on offer) and nobody
    // chose it: the list's step takes it up to Fable 5.1.
    assert_eq!(
        relaunch(
            &["claude", "--verbose"],
            "claude-opus-5",
            &["claude-fable-5-1".to_string(), "claude-opus-5".to_string()]
        ),
        line(&["--verbose", "--model", "claude-fable-5-1"])
    );
}

/// [`forget_settled_pending`] forgets a same-build (model) restart the list no longer asks
/// for before it begins, and reports a pending record for the build the session now runs
/// (reached by hand). Negative controls: a newer build, an announced or stopped round, an
/// owed release and a dry run keep their state.
#[test]
fn an_unwanted_model_restart_is_forgotten_before_it_begins() {
    let dir = scratch("forget-model-restart");
    let opts = drive(&dir);
    let running = Version::parse("2.1.282").expect("v");
    let model_only = St {
        from: "2.1.282".to_string(),
        to: "2.1.282".to_string(),
        pending_since: 5,
        ..St::default()
    };
    let build = St {
        from: "2.1.281".to_string(),
        to: "2.1.283".to_string(),
        ..model_only.clone()
    };
    let announced = St {
        phase: Phase::Announced { at_s: 9, asks: 1 },
        ..model_only.clone()
    };
    let stopped = St {
        phase: Phase::Failed("unanswered".to_string()),
        from: "2.1.281".to_string(),
        ..model_only.clone()
    };
    let owing = St {
        release: "gave-up".to_string(),
        from: "2.1.281".to_string(),
        ..model_only.clone()
    };
    for (why, st) in [
        ("a build upgrade to a newer build", &build),
        ("an announced one", &announced),
        ("a stopped round", &stopped),
        ("a record that owes a release", &owing),
    ] {
        save(&opts, SESSION, st);
        // As written: the one writer stamps its progress (`St::progress_at`).
        let written = load(&opts, SESSION);
        assert_eq!(
            forget_settled_pending(&opts, SESSION, Some(st), &running),
            None
        );
        assert_eq!(load(&opts, SESSION), written, "{why} is kept");
    }
    save(&opts, SESSION, &model_only);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    forget_settled_pending(&dry, SESSION, Some(&model_only), &running);
    assert!(load(&opts, SESSION).is_some(), "a dry run writes nothing");
    assert_eq!(
        forget_settled_pending(&opts, SESSION, Some(&model_only), &running),
        None,
        "a model restart is forgotten without a word"
    );
    assert!(
        load(&opts, SESSION).is_none(),
        "the pending model restart is gone"
    );
    // A pending record for a build the session runs: reached by hand.
    let reached = St {
        from: "2.1.274".to_string(),
        to: "2.1.282".to_string(),
        pending_since: 5,
        ..St::default()
    };
    save(&opts, SESSION, &reached);
    assert_eq!(
        forget_settled_pending(&opts, SESSION, Some(&reached), &running).as_ref(),
        Some(&reached),
        "reported, so the ledger says it once"
    );
    assert!(load(&opts, SESSION).is_none(), "and gone");
    let _ = std::fs::remove_dir_all(dir);
}

/// [`pick`] answers the window's step with the tab's own word, never its daemon's act: a
/// Codex client's wait stands over `daemon-updated`. A current client leaves the daemon's
/// word; with nothing of the tab's, the first act, then the first wait.
#[test]
fn the_windows_step_answers_with_the_tabs_own_word_before_its_daemons() {
    let report = |tab: &str, step: &str| Report {
        pid: 1,
        tab: tab.to_string(),
        session: "-".to_string(),
        from: "-".to_string(),
        to: "-".to_string(),
        step: step.to_string(),
    };
    let tab = "s-c0dec0dec0dec0dec0de";
    let daemon = report("-", "daemon-updated");
    let words = |reports: &[Report]| pick(reports, tab).map(|r| r.step.clone());
    assert_eq!(
        words(&[daemon.clone(), report(tab, "wait:not-idle")]),
        Some("wait:not-idle".to_string()),
        "the client's wait, not the daemon's act"
    );
    assert_eq!(
        after("wait:not-idle", 0),
        After::Later(LATER[0]),
        "and the client is looked at again"
    );
    assert_eq!(
        words(&[
            daemon.clone(),
            report(tab, "exit-typed"),
            report(tab, "done")
        ]),
        Some("exit-typed".to_string()),
        "the tab's own act first"
    );
    assert_eq!(
        words(&[daemon.clone(), report(tab, "current")]),
        Some("daemon-updated".to_string()),
        "a current client leaves the daemon's word"
    );
    // Negative control: nothing of the tab's — the first act, then the first
    // wait, then nothing.
    assert_eq!(
        words(&[report("-", "wait:session-files-unreadable"), daemon.clone()]),
        Some("daemon-updated".to_string())
    );
    assert_eq!(
        words(&[report("-", "wait:session-files-unreadable")]),
        Some("wait:session-files-unreadable".to_string())
    );
    assert_eq!(words(&[report(tab, "current")]), None);
}

/// A busy Claude Code (its turn over, waiting on a workflow) is announced to once at a
/// break (`Opts::background`), then waits `background` within the re-ask interval, ending
/// nothing. Negative control: the same session at an idle point's step waits `not-idle`.
#[cfg(unix)]
#[test]
fn a_break_of_background_work_takes_a_notice_and_ends_nothing() {
    let busy = |h: &mut Parked| set_status(h, "busy");
    // The screen such a session draws: the workflow waited on, over the
    // composer (a `busy` status over a bare idle screen is a status that lags
    // it, read so at the second look).
    let waiting = Answers {
        screen: waiting_screen(),
        ..Answers::default()
    };
    let mut h = Parked::new("bg-idle-point", waiting, unsettled(), 3600);
    busy(&mut h);
    for _ in 0..3 {
        assert_eq!(h.visit().step, "wait:not-idle", "an idle point's step");
    }
    assert!(h.typed().is_empty());
    drop(h);
    let mut h = Parked::new("bg-break", Answers::default(), unsettled(), 3600);
    busy(&mut h);
    h.opts.background = true;
    assert_eq!(
        h.visit().step,
        "announced:1",
        "the first notice, at the break"
    );
    assert_eq!(h.typed().len(), 1);
    assert!(h.typed()[0].contains("[aterm harness]"), "{:?}", h.typed());
    assert_eq!(h.visit().step, "wait:background", "and nothing more there");
    assert_eq!(h.typed().len(), 1);
    assert!(
        !h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal")),
        "nothing ended"
    );
}

/// Claude's own session file for `h`'s agent, rewritten to say `status`.
#[cfg(unix)]
fn set_status(h: &mut Parked, status: &str) {
    let path = h
        .opts
        .home
        .join(format!(".claude/sessions/{}.json", h.sf.pid));
    let text = std::fs::read_to_string(&path).expect("session file");
    let was = format!(r#""status":"{}""#, h.sf.status);
    std::fs::write(
        &path,
        text.replace(&was, &format!(r#""status":"{status}""#)),
    )
    .expect("rewrite");
    h.sf = session_file_of(&h.opts.home, h.sf.pid).expect("parses");
    assert_eq!(h.sf.status, status);
}

/// Stamps `h`'s record with its agent's process start ([`St::proc_start`]), as this build
/// writes it: a record naming none is an older build's, whose age is repaired to the
/// process's start at the next look.
#[cfg(unix)]
fn own_record(h: &Parked) {
    let mut st = load(&h.opts, SESSION).expect("state");
    st.proc_start = squash(&h.sf.proc_start);
    save(&h.opts, SESSION, &st);
}

/// The process table's rows for two shells under `h`'s agent that never end
/// (a widowed `tail -f … | grep -m1 …`, its `grep` long gone).
#[cfg(unix)]
fn shells_under(h: &Parked) -> Vec<(u32, u32, String)> {
    vec![
        (999_901, h.sf.pid, "zsh".to_string()),
        (999_902, h.sf.pid, "zsh".to_string()),
    ]
}

/// [`SESSION`]'s transcript under `h`'s home: a person's task, the notice
/// whose marker is `marker`, and the agent's last answer `text`.
#[cfg(unix)]
fn asked_then_answered(h: &Parked, marker: &str, text: &str) {
    let notice = upgrade::prepare_prompt(
        &Version::parse("1.0.0").expect("from"),
        &Version::parse("9.9.9").expect("to"),
        Source::Managed,
        marker,
    );
    let project = h.opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let rows = [
        user_row("fix the bug"),
        user_row(&notice),
        turn_by("claude-opus-5-5", text),
    ];
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        rows.join("\n") + "\n",
    )
    .expect("transcript");
}

/// The quiet flow releases an agent an older build announced to: at a break past the
/// window it gives up and types its one release, worded for the quiet flow, then rests
/// (`wait:failed`); nothing is ended. Negative control: inside the window, `background`.
#[cfg(unix)]
#[test]
fn a_quiet_round_an_older_build_announced_gives_up_and_releases_its_agent() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let rig = |name: &str, at_s: u64| {
        let mut h = quietly(|| Parked::new(name, Answers::default(), unsettled(), 3600));
        assert!(!h.opts.ask, "the default flow");
        set_status(&mut h, "shell");
        asked_then_answered(&h, marker, "Not yet: my shells still run.");
        let st = St {
            phase: Phase::Announced { at_s, asks: 3 },
            markers: vec![marker.to_string()],
            ..announced_to(&h, marker)
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h.opts.background = true;
        h
    };
    let h = rig("quiet-announced-old", now_s() - 3 * 86_400);
    let shells = shells_under(&h);
    let words: Vec<String> = (0..2)
        .map(|_| h.visit_with(dead_pid(), &shells).step)
        .collect();
    assert_eq!(
        words,
        ["released:gave-up", "wait:failed"],
        "typed: {:?}",
        h.typed()
    );
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains("Upgrade off:"), "{typed:?}");
    assert!(
        !typed[0].contains("without asking"),
        "no promise the quiet flow does not keep: {typed:?}"
    );
    let gave_up = h.details("gave-up");
    assert_eq!(gave_up.len(), 1);
    assert!(gave_up[0].contains("asking nothing"), "{gave_up:?}");
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(kept.release.is_empty(), "nothing owed: {kept:?}");
    assert_eq!(kept.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
    // The owner's row tells this give-up by its work seen: `Upgrade now` does not move it
    // past that work.
    assert!(kept.work_seen, "{kept:?}");
    assert!(
        !h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal")),
        "nothing ended"
    );
    drop(h);
    // Negative control: inside the notice's window.
    let h = rig("quiet-announced-new", now_s() - 60);
    let shells = shells_under(&h);
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:background");
    assert!(h.typed().is_empty(), "{:?}", h.typed());
}

/// A quiet pending round owing a release (a notice reached this agent, its round abandoned
/// since) types it where the agent's own work holds the restart (`released:retargeted`) and
/// stays pending. Negative controls: a release owed to another process is dropped; owing
/// nothing types nothing.
#[cfg(unix)]
#[test]
fn a_quiet_pending_round_types_the_release_it_owes_where_work_holds_its_restart() {
    let rig = |name: &str, owed: Option<bool>| {
        let mut h = quietly(|| Parked::new(name, Answers::default(), unsettled(), 3600));
        set_status(&mut h, "shell");
        let mut st = St {
            tab: TAB.to_string(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            asked: vec!["ATERM-UPGRADE-READY-0badf00d".to_string()],
            ..unsettled()
        };
        if let Some(to_this) = owed {
            st.release = "retargeted".to_string();
            if !to_this {
                st.notice_pid = dead_pid();
            }
        }
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        h.opts.background = true;
        h
    };
    let h = rig("quiet-owed-here", Some(true));
    let shells = shells_under(&h);
    assert_eq!(
        h.visit_with(dead_pid(), &shells).step,
        "released:retargeted",
        "typed: {:?}",
        h.typed()
    );
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains("Upgrade off:"), "{typed:?}");
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(kept.release.is_empty(), "{kept:?}");
    assert_eq!(
        kept.phase,
        Phase::Pending,
        "its quiet restart still to come"
    );
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:background");
    assert_eq!(h.typed().len(), 1, "once");
    drop(h);
    // Negative controls.
    for (name, owed) in [
        ("quiet-owed-elsewhere", Some(false)),
        ("quiet-owes-nothing", None),
    ] {
        let h = rig(name, owed);
        let shells = shells_under(&h);
        assert_eq!(
            h.visit_with(dead_pid(), &shells).step,
            "wait:background",
            "{name}"
        );
        assert!(h.typed().is_empty(), "{name}: {:?}", h.typed());
    }
}

/// The owner's `--now` moves a quiet session whose status only lags (`shell` over an idle
/// screen, nothing under the agent) to its signal: the second look reads the lag past, and
/// the last look takes that status only unrewritten ([`status_lets_signal`]). Negative
/// controls: unasked it holds at every look; the first look under the word is no lag.
#[cfg(unix)]
#[test]
fn the_owners_now_restarts_a_quiet_session_whose_status_only_lags() {
    use std::os::unix::process::ExitStatusExt as _;
    let rig = |name: &str| {
        let behind = St {
            pending_since: now_s() - 3_600,
            ..settled()
        };
        let mut h = quietly(|| Parked::new(name, Answers::default(), behind, 3600));
        assert!(!h.opts.ask, "the default flow");
        set_status(&mut h, "shell");
        own_record(&h);
        transcript_says(&h, "Done: the report is written.");
        h
    };
    // Unasked: never on the status alone.
    let h = rig("quiet-lag-unasked");
    for _ in 0..3 {
        assert_eq!(h.visit().step, "wait:status-stale");
    }
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    assert!(h.details("terminated").is_empty());
    drop(h);
    // The owner's word: its first look shows no lag yet; the second reads
    // it past, to the signal.
    let mut h = rig("quiet-lag-now");
    ask(&h.opts, TAB, Ask::Now, false).expect("--now");
    assert_eq!(h.visit().step, "wait:status-stale", "one look is no lag");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    let terminated = h.details("terminated");
    assert_eq!(terminated.len(), 1, "{r:?}");
    assert!(terminated[0].starts_with("SIGTERM"), "{terminated:?}");
    assert!(h.typed().is_empty(), "nothing typed first: {:?}", h.typed());
    drop(h);
    // The last look's reading of the status, by itself.
    let looked = SessionFile {
        pid: 4242,
        session_id: SESSION.into(),
        cwd: "/".into(),
        version: "1.0.0".into(),
        status: "shell".into(),
        status_updated_at_ms: 1_000,
        proc_start: "Wed Sep 30 12:00:00 2026".into(),
        kind: "interactive".into(),
        entrypoint: "cli".into(),
        claude_dir: None,
        profile: None,
    };
    let idle = SessionFile {
        status: "idle".into(),
        status_updated_at_ms: 9_000,
        ..looked.clone()
    };
    let rewritten = SessionFile {
        status: "busy".into(),
        status_updated_at_ms: 9_000,
        ..looked.clone()
    };
    let restamped = SessionFile {
        status_updated_at_ms: 9_000,
        ..looked.clone()
    };
    for lagging in [false, true] {
        assert!(status_lets_signal(&idle, &looked, lagging), "{lagging}");
        assert!(
            !status_lets_signal(&rewritten, &looked, lagging),
            "{lagging}"
        );
        assert!(
            !status_lets_signal(&restamped, &looked, lagging),
            "{lagging}"
        );
    }
    assert!(
        status_lets_signal(&looked, &looked, true),
        "the lag read past"
    );
    assert!(
        !status_lets_signal(&looked, &looked, false),
        "never unasked"
    );
}

/// The owner's `--now` never signals a turn begun since the look over a lagging status: a
/// stale `busy` need not be restamped when a turn begins, so the last look and the re-read
/// before the signal also ask the screen for the turn over ([`turn_over_shown`]).
/// Negative control: a screen that stays idle ends the agent (SIGTERM), nothing typed.
#[cfg(unix)]
#[test]
fn the_owners_now_never_signals_a_turn_begun_since_the_look_over_a_lagging_status() {
    use std::os::unix::process::ExitStatusExt as _;
    let rule = "─".repeat(20);
    let turn = format!(
        r#"{{"rows":["✻ Pondering… (3s · esc to interrupt)","{rule}","❯ ","{rule}"],"cursor":{{"row":2,"col":2}},"seq":79,"human_ms":null,"first":0}}"#
    );
    let rig = |name: &str, after: Option<(usize, String)>| {
        let behind = St {
            pending_since: now_s() - 3_600,
            ..settled()
        };
        let answers = Answers {
            after,
            ..Answers::default()
        };
        let mut h = quietly(|| Parked::new(name, answers, behind, 3600));
        set_status(&mut h, "busy");
        own_record(&h);
        transcript_says(&h, "Done: the report is written.");
        ask(&h.opts, TAB, Ask::Now, false).expect("--now");
        assert_eq!(h.visit().step, "wait:status-stale", "one look is no lag");
        h
    };
    let signalled = |h: &Parked| {
        h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains(" signal "))
    };
    // Negative control: the screen stays idle — ended, nothing typed.
    let mut h = rig("lag-turn-idle", None);
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    // The screen reads up to the restart's hand on the tab: the next is the
    // last look's.
    let before_hand = h
        .asked
        .lock()
        .expect("asked")
        .iter()
        .take_while(|l| !l.contains(" lease acquire "))
        .filter(|l| l.contains(" text "))
        .count();
    drop(h);
    // A turn begun since the look: on the last look's screen, and on the
    // re-read before the signal alone.
    for (after, want, name) in [
        (before_hand, "wait:changed", "lag-turn-at-last-look"),
        (
            before_hand + 1,
            "wait:changed-before-signal",
            "lag-turn-before-signal",
        ),
    ] {
        let h = rig(name, Some((after, turn.clone())));
        let mut shell = tty_zsh();
        let r = h.visit_under(shell.id());
        let _ = shell.kill();
        let _ = shell.wait();
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert!(!signalled(&h), "{name}: signalled mid-turn");
        assert!(h.details("terminated").is_empty(), "{name}");
        assert!(alive(h.sf.pid), "{name}: the agent lives on");
        assert!(h.typed().is_empty(), "{name}: {:?}", h.typed());
        let st = load(&h.opts, SESSION).expect("state");
        assert_eq!(st.phase, Phase::Pending, "{name}: the round as it was");
    }
}

/// A give-up at a break types its one owed release (`released:gave-up`) in the same visit,
/// then waits `wait:failed`; a late READY voided past the drain is released the same way,
/// over shells or work inside the agent. Nothing is ended. Negative control: a notice
/// inside its window types nothing (`wait:background`) and names its holders.
#[cfg(unix)]
#[test]
fn a_give_up_at_a_break_types_the_release_it_owes() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let signalled = |h: &Parked| {
        h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal"))
    };
    let mut h = Parked::new("bg-giveup", Answers::default(), unsettled(), 3600);
    set_status(&mut h, "shell");
    asked_then_answered(&h, marker, "Waiting on the closers.");
    let st = St {
        phase: Phase::Announced {
            at_s: now_s() - upgrade::REASK_S - 60,
            asks: upgrade::MAX_ASKS,
        },
        markers: vec![marker.to_string()],
        ..announced_to(&h, marker)
    };
    std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    h.opts.background = true;
    let shells = shells_under(&h);
    let words: Vec<String> = (0..3)
        .map(|_| h.visit_with(dead_pid(), &shells).step)
        .collect();
    assert_eq!(
        words,
        ["released:gave-up", "wait:failed", "wait:failed"],
        "typed: {:?}",
        h.typed()
    );
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains("Upgrade off:"), "{typed:?}");
    assert_eq!(
        h.details("gave-up").len(),
        1,
        "the give-up is on the record"
    );
    assert_eq!(h.details("released:gave-up").len(), 1);
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(kept.release.is_empty(), "nothing owed: {kept:?}");
    assert_eq!(kept.phase, Phase::Failed(upgrade::GAVE_UP.to_string()));
    assert!(!signalled(&h), "nothing ended");
    drop(h);

    // The late READY the shells outlive past the drain: void, then released.
    let mut h = Parked::new("bg-giveup-ready", Answers::default(), unsettled(), 3600);
    set_status(&mut h, "shell");
    asked_then_answered(&h, marker, &format!("Done.\\n{marker}"));
    let mut st = St {
        markers: vec![marker.to_string()],
        ready_since: now_s() - upgrade::DRAIN_S - 60,
        ..announced_to(&h, marker)
    };
    st.give_up(now_s());
    assert_eq!(st.release, "gave-up");
    std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    h.opts.background = true;
    let shells = shells_under(&h);
    let words: Vec<String> = (0..2)
        .map(|_| h.visit_with(dead_pid(), &shells).step)
        .collect();
    assert_eq!(
        words,
        ["released:gave-up", "wait:failed"],
        "typed: {:?}",
        h.typed()
    );
    assert_eq!(h.details("drain-expired:background").len(), 1);
    assert_eq!(h.typed().len(), 1);
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(
        kept.release.is_empty() && kept.markers.is_empty(),
        "{kept:?}"
    );
    assert!(!signalled(&h), "nothing ended");
    drop(h);

    // The late READY that work inside the agent outlives (workflows it waits on, status
    // `busy`, nothing under it): voided past the drain and released, as over a shell.
    let waiting = Answers {
        screen: waiting_screen(),
        ..Answers::default()
    };
    let mut h = Parked::new("bg-giveup-inproc", waiting, unsettled(), 3600);
    set_status(&mut h, "busy");
    asked_then_answered(&h, marker, &format!("Done.\\n{marker}"));
    let mut st = St {
        markers: vec![marker.to_string()],
        ready_since: now_s() - 3 * upgrade::DRAIN_S,
        ..announced_to(&h, marker)
    };
    st.give_up(now_s());
    std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    h.opts.background = true;
    let words: Vec<String> = (0..2).map(|_| h.visit().step).collect();
    assert_eq!(
        words,
        ["released:gave-up", "wait:failed"],
        "typed: {:?}",
        h.typed()
    );
    assert_eq!(h.details("drain-expired:background").len(), 1);
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains("Upgrade off:"), "{typed:?}");
    let kept = load(&h.opts, SESSION).expect("state");
    assert!(
        kept.release.is_empty() && kept.markers.is_empty(),
        "{kept:?}"
    );
    assert!(!signalled(&h), "nothing ended");
    drop(h);

    // Negative control: a notice inside its window stands; nothing is typed —
    // and the owner is told what holds it, by pid and name alone.
    let mut h = Parked::new("bg-standing", Answers::default(), unsettled(), 3600);
    set_status(&mut h, "shell");
    asked_then_answered(&h, marker, "Waiting on the closers.");
    let st = St {
        markers: vec![marker.to_string()],
        ..announced_to(&h, marker)
    };
    std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
    h.opts.background = true;
    let shells = shells_under(&h);
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:background");
    assert!(h.typed().is_empty(), "{:?}", h.typed());
    let held: Vec<(u32, String)> = load(&h.opts, SESSION)
        .expect("state")
        .held_by
        .iter()
        .map(|p| (p.pid, p.name.clone()))
        .collect();
    assert_eq!(
        held,
        [(999_901, "zsh".to_string()), (999_902, "zsh".to_string())]
    );
}

/// [`St::note_held`] keeps what holds the move by start, not age: `ps` ages are whole
/// seconds read off another clock, so the same process keeps its start and its status row
/// (sent only when it changes). A new process under an old pid is its own; an act clears
/// it, and a wait on nothing names nothing.
#[test]
fn what_holds_the_move_is_kept_by_its_start() {
    let zsh = |pid: u32, age_s: u64| upgrade::Held {
        pid,
        name: "zsh".to_string(),
        age_s,
        command: "tail -f journal.jsonl".to_string(),
    };
    let mut st = St::default();
    st.note_held("wait:background", &[zsh(63_492, 100)], 1_000);
    assert_eq!(st.held_by[0].since, 900);
    st.note_held("wait:background", &[zsh(63_492, 101)], 1_002);
    assert_eq!(st.held_by[0].since, 900, "the same process, the same start");
    st.note_held("wait:background", &[zsh(63_492, 5)], 1_002);
    assert_eq!(st.held_by[0].since, 997, "a new process under an old pid");
    let json = st.to_json();
    assert!(!json.contains("tail -f"), "never a command: {json}");
    assert_eq!(St::from_json(&json).expect("parses").held_by, st.held_by);
    st.note_held("announced:2", &[zsh(63_492, 5)], 1_003);
    assert!(st.held_by.is_empty(), "an act clears it");
    st.note_held("wait:awaiting-ready", &[], 1_004);
    assert!(st.held_by.is_empty());
}

/// A Claude Code whose turn is over, a dynamic workflow it started still
/// running: the wait drawn over the composer (`wait_bg.out`'s shape).
#[cfg(unix)]
fn waiting_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["⏺ Both workflows are launched.","","✻ Waiting for 2 dynamic workflows to finish","","{rule}","❯ ","{rule}","  ⏵⏵ bypass permissions on (shift+tab to cycle)"],"cursor":{{"row":5,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// A Claude Code mid-turn, a Bash call running in the foreground: the
/// spinner over the composer, the busy footer under it.
#[cfg(unix)]
fn mid_turn_screen() -> String {
    let rule = "─".repeat(20);
    format!(
        r#"{{"rows":["⏺ Bash(sleep 600)","  ⎿  Running…","","✻ Pondering… (12s · esc to interrupt)","","{rule}","❯ ","{rule}","  ⏵⏵ bypass permissions on · esc to interrupt"],"cursor":{{"row":6,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    )
}

/// An idle screen over Claude Code's own work (status `shell` over two never-ending
/// shells, or a stale `busy` over nothing) waits `status-stale` one look and is asked at
/// its second, the notice naming the shells; nothing is ended. Negative control: a turn
/// still running waits `not-idle`.
#[cfg(unix)]
#[test]
fn an_idle_screen_over_claudes_own_work_is_asked_at_its_second_look() {
    let mut h = Parked::new("shell-idle", Answers::default(), settled(), 3600);
    set_status(&mut h, "shell");
    let shells = shells_under(&h);
    assert_eq!(
        h.visit_with(dead_pid(), &shells).step,
        "wait:status-stale",
        "one look"
    );
    assert_eq!(
        load(&h.opts, SESSION).expect("state").wait,
        "status-stale:shell"
    );
    assert!(h.typed().is_empty());
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "announced:1");
    let typed = h.typed();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains("pid 999901 (zsh"), "{typed:?}");
    assert!(
        !h.asked
            .lock()
            .expect("asked")
            .iter()
            .any(|l| l.contains("signal")),
        "nothing ended"
    );
    drop(h);

    // A stale `busy` over an idle screen and nothing under the agent.
    let mut h = Parked::new("stale-busy", Answers::default(), settled(), 3600);
    set_status(&mut h, "busy");
    assert_eq!(h.visit().step, "wait:status-stale");
    assert_eq!(h.visit().step, "announced:1");
    drop(h);

    // Negative control: a turn still running.
    let mut h = Parked::new(
        "shell-mid-turn",
        Answers {
            screen: mid_turn_screen(),
            ..Answers::default()
        },
        settled(),
        3600,
    );
    set_status(&mut h, "shell");
    let shells = shells_under(&h);
    for _ in 0..3 {
        assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:not-idle");
    }
    assert!(h.typed().is_empty(), "{:?}", h.typed());
}

/// [`St::idle_looks`] counts one process's idle looks in a row, so an old process's idle
/// look (a gone notice's new holder, a record that followed its conversation) never lets
/// the notice go on a new agent's first look; a look older than [`IDLE_RUN_GAP_S`] starts
/// the run again. Negative control: the new agent's own second look announces.
#[cfg(unix)]
#[test]
fn an_idle_look_of_another_process_counts_toward_no_lag() {
    let mut old = Parked::new("idle-old-holder", Answers::default(), settled(), 3600);
    set_status(&mut old, "busy");
    assert_eq!(old.visit().step, "wait:status-stale", "its one look");
    let mut new = Parked::new("idle-new-holder", Answers::default(), settled(), 3600);
    set_status(&mut new, "busy");
    assert_ne!(new.sf.pid, old.sf.pid);
    std::fs::copy(
        state_path(&old.opts, SESSION),
        state_path(&new.opts, SESSION),
    )
    .expect("the record the old process's look left");
    drop(old);
    assert_eq!(
        new.visit().step,
        "wait:status-stale",
        "the new agent's first look is one look"
    );
    assert!(new.typed().is_empty(), "{:?}", new.typed());
    assert_eq!(new.visit().step, "announced:1", "its second");
    assert_eq!(new.typed().len(), 1);
    drop(new);

    // A look of this very agent's, but longer ago than any re-look waits:
    // the run begins again. Its next look, in a row, is its second.
    let mut late = Parked::new("idle-long-before", Answers::default(), settled(), 3600);
    set_status(&mut late, "busy");
    let mut st = load(&late.opts, SESSION).expect("state");
    (st.idle_looks, st.idle_pid, st.idle_at) = (1, late.sf.pid, now_s() - IDLE_RUN_GAP_S - 1);
    save(&late.opts, SESSION, &st);
    assert_eq!(late.visit().step, "wait:status-stale", "no run: one look");
    assert_eq!(late.visit().step, "announced:1", "in a row: its second");
}

// ---------------------------------------------------------------- no task

/// A user row the harness typed, as the E2E's transcripts hold one.
#[cfg(unix)]
fn harness_row(text: &str) -> String {
    user_row(&format!("[aterm harness] {text}"))
}

/// [`Tasked`] reads a conversation's task on from where its last read ended: no Claude
/// home, unlistable projects, or two transcripts of the id say nothing; no projects or no
/// transcript is no task, nor are the harness's own turns; a person's whole prompt is a
/// task at once and for good; a half-written line is read once whole.
#[cfg(unix)]
#[test]
fn whether_a_conversation_has_a_task_is_read_on_from_where_the_last_read_ended() {
    let dir = scratch("tasked-memo");
    let home = dir.join("home");
    assert_eq!(
        tasked(&home, SESSION, &[]),
        None,
        "no Claude home: nobody can say"
    );
    // A Claude home with no projects yet: no message, so no task.
    std::fs::create_dir_all(home.join(".claude")).expect("home");
    assert_eq!(tasked(&home, SESSION, &[]), Some(false), "no projects yet");
    // Projects that cannot be listed: nobody can say.
    std::fs::write(home.join(".claude/projects"), "").expect("not a directory");
    assert_eq!(tasked(&home, SESSION, &[]), None, "unreadable projects");
    std::fs::remove_file(home.join(".claude/projects")).expect("rm");
    let project = home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("projects");
    assert_eq!(tasked(&home, SESSION, &[]), Some(false), "no message yet");
    let path = project.join(format!("{SESSION}.jsonl"));
    let ready = turn_by("claude-haiku-4-5", "ATERM-UPGRADE-READY-5b2e0fc4");
    let own = [
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        ready.clone(),
        harness_row("Upgraded: carry on"),
        turn_by(
            "claude-haiku-4-5",
            "What would you like me to help you with?",
        ),
    ];
    std::fs::write(&path, own.join("\n") + "\n").expect("transcript");
    let mut memo = Tasked::default();
    assert_eq!(
        memo.of(&home, SESSION, &[]),
        Some(false),
        "the harness's own turns"
    );
    let read = memo.read_to;
    assert_eq!(read, std::fs::metadata(&path).expect("len").len());
    // A person's prompt caught half-written, then whole and answered.
    let person = user_row("Staged harness test, please follow exactly.");
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    write!(f, "{}", &person[..20]).expect("half");
    assert_eq!(memo.of(&home, SESSION, &[]), Some(false));
    assert_eq!(memo.read_to, read, "a half line is not taken");
    writeln!(f, "{}", &person[20..]).expect("rest");
    assert_eq!(
        memo.of(&home, SESSION, &[]),
        Some(true),
        "asked: a task before its answer"
    );
    writeln!(f, "{}", turn_by("claude-haiku-4-5", "Stage 1 complete.")).expect("answer");
    assert_eq!(memo.of(&home, SESSION, &[]), Some(true));
    // For good: nothing is read for it again (a rewrite is not looked at).
    std::fs::write(&path, "").expect("emptied");
    assert_eq!(memo.of(&home, SESSION, &[]), Some(true));
    // Negative control: another conversation starts over.
    let other = "0badf00d-9999-2222-3333-444455556666";
    std::fs::write(
        project.join(format!("{other}.jsonl")),
        own.join("\n") + "\n",
    )
    .expect("other");
    assert_eq!(memo.of(&home, other, &[]), Some(false));
    // Two transcripts claim it: nobody can say.
    let second = home.join(".claude/projects/q");
    std::fs::create_dir_all(&second).expect("second project");
    std::fs::write(second.join(format!("{other}.jsonl")), "").expect("twin");
    assert_eq!(tasked(&home, other, &[]), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in agent in [`TAB`] whose conversation holds `rows` (Pending on the newer build,
/// its screen quiet an hour): the visit's report under `kernel`, the state it left, and how
/// many turns it typed.
#[cfg(unix)]
fn visit_holding(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
) -> (Report, Option<St>, usize) {
    visit_seen(name, rows, dry_run, kernel, 77, None, None)
}

/// [`visit_holding`] with the last look an hour ago at `last_seq` (the screen now at `seq` 77),
/// the tab's `status` reading `status` (`None`: nothing held, no agent verdict), and the tab's
/// supervisor having ledgered `ledger` as typed ([`Opts::aterm_state`]; `None`: no ledger).
#[cfg(unix)]
fn visit_seen(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
) -> (Report, Option<St>, usize) {
    let (r, st, asked) = visit_answered(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        Answers::default(),
    );
    (r, st, asked.iter().filter(|l| l.contains(" turn ")).count())
}

/// [`visit_seen`] on a stand-in answering as `answers` says: every request
/// the visit made, in order, instead of the count of its turns.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_answered(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
) -> (Report, Option<St>, Vec<String>) {
    visit_launched(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        answers,
        &[],
        &|_| {},
    )
}

/// [`visit_answered`], the agent's argv carrying `launch` after its own
/// words — the flags a person launched it with, which the relaunch carries —
/// and `seed` run over the sweep's options before the visit (the harness's
/// own records a test lays down: a `/model` choice remembered).
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_launched(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
    launch: &[&str],
    seed: &dyn Fn(&Opts),
) -> (Report, Option<St>, Vec<String>) {
    visit_launched_env(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        answers,
        launch,
        seed,
        &[],
    )
}

/// [`visit_launched`], the agent's exec environment as the visit reads it
/// carrying `env` (`NAME=VALUE` each) beside its own — what a process
/// relaunched under a login of the harness's choosing has, which the
/// stand-in's real environment cannot be given after its start.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_launched_env(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
    launch: &[&str],
    seed: &dyn Fn(&Opts),
    env: &[String],
) -> (Report, Option<St>, Vec<String>) {
    visit_launched_probed(
        name,
        rows,
        dry_run,
        kernel,
        last_seq,
        status,
        ledger,
        answers,
        launch,
        seed,
        env,
        |pid, start| ParkedProcess {
            script: kernel,
            pid,
            start: Some(start),
            background: Vec::new(),
        },
    )
}

/// The same visit with its last-look process reads injected (`make_kernel`), so the parked
/// fixture and the last-look controls drive one real visit.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn visit_launched_probed<K: Kernel>(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
    last_seq: u64,
    status: Option<&str>,
    ledger: Option<&[&str]>,
    answers: Answers,
    launch: &[&str],
    seed: &dyn Fn(&Opts),
    env: &[String],
    make_kernel: impl FnOnce(u32, String) -> K,
) -> (Report, Option<St>, Vec<String>) {
    use crate::supervise::approvals;
    let dir = scratch(name);
    let (sock, asked) = instance_with(&dir, answers);
    if let Some(status) = status {
        std::fs::write(dir.join("t.status"), format!("{status}\n")).expect("status");
    }
    let aterm_state = ledger.map(|typed| {
        let root = dir.join("aterm-state");
        let path = approvals::path_under(&root, Some(TAB)).expect("ledger dir");
        let rows: Vec<String> = typed
            .iter()
            .map(|text| {
                approvals::Row {
                    rule_id: "continue@v1",
                    outcome: approvals::Outcome::Typed,
                    command: text,
                    reason: "the turn-end policy",
                    box_seq: 1,
                }
                .to_json(1_000, Some(TAB))
            })
            .collect();
        std::fs::write(path, rows.join("\n") + "\n").expect("ledger");
        root
    });
    let opts = Opts {
        sock: Some(sock),
        dry_run,
        aterm_state,
        ..drive(&dir)
    };
    // An argv a restart carries (the filter alone: a positional, dropped).
    let mut agent = parked_bare().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("projects");
    if !rows.is_empty() {
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            rows.join("\n") + "\n",
        )
        .expect("transcript");
    }
    let st = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq,
        seq_since_s: now_s() - 3600,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    seed(&opts);
    let table = vec![(kernel.shell, 1, "zsh".to_string())];
    let files = session_files(&opts.home);
    let mut args = atpkg::caller_shell::process_args(agent.id()).expect("agent argv");
    args.argv.extend(launch.iter().map(|w| (*w).to_string()));
    args.env.extend(env.iter().cloned());
    let stable = make_kernel(sf.pid, squash(&sf.proc_start));
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &table,
        &newer(),
        &stable,
        Some(&args),
        None,
    );
    let saved = load(&opts, SESSION);
    let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    (r, saved, asked)
}

/// A conversation with no task (fresh, or only the harness's own turns) is restarted
/// afresh, never announced to. Negative controls: an answered prompt, or a first prompt
/// recorded and not yet answered, is announced to.
#[cfg(unix)]
#[test]
fn an_upgrade_restarts_a_conversation_with_no_task_afresh_and_announces_to_one_with_a_task() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let person = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let first = vec![own[0].clone(), own[1].clone(), person[0].clone()];
    for (name, rows, want) in [
        ("fresh-none", Vec::new(), "would-restart:fresh"),
        ("fresh-own", own, "would-restart:fresh"),
        ("fresh-person", person, "would-announce"),
        ("fresh-first-prompt", first, "would-announce"),
    ] {
        let (r, _, typed) = visit_holding(name, &rows, true, &Script::new(shell, usize::MAX, None));
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}: a dry run types nothing");
    }
}

/// A Claude home with no `projects/` holds no transcript (Claude Code makes it with the
/// home's first message): both flows restart such a session afresh, never `--resume`.
/// Negative control: projects that exist but cannot be read say nothing.
#[cfg(unix)]
#[test]
fn an_upgrade_in_a_claude_home_with_no_projects_restarts_the_session_afresh() {
    let shell = dead_pid();
    let kernel = || Script::new(shell, usize::MAX, None);
    let projects_as = |file: bool| {
        move |opts: &Opts| {
            let projects = opts.home.join(".claude/projects");
            std::fs::remove_dir_all(&projects).expect("no projects yet");
            if file {
                std::fs::write(&projects, "").expect("projects that cannot be listed");
            }
        }
    };
    let visit = |name: &str, file: bool| {
        visit_launched(
            name,
            &[],
            true,
            &kernel(),
            77,
            None,
            None,
            Answers::default(),
            &[],
            &projects_as(file),
        )
    };
    let (r, _, typed) = quietly(|| visit("no-projects-quiet", false));
    assert_eq!(r.step, "would-restart:fresh", "quiet: {r:?}");
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "{typed:?}");
    let (r, _, typed) = visit("no-projects-ask", false);
    assert_eq!(r.step, "would-restart:fresh", "ask: {r:?}");
    assert!(typed.iter().all(|l| !l.contains(" turn ")), "{typed:?}");
    // Negative control: unreadable projects say nothing of a task.
    let (r, _, _) = quietly(|| visit("unreadable-projects-quiet", true));
    assert_eq!(r.step, "would-restart", "{r:?}");
}

/// A [`Script`] that, at its `at`-th job read, runs `then`: what changes
/// between a visit's step (its reads before) and its restart (the reads
/// after).
#[cfg(unix)]
struct Racing<'a> {
    script: Script,
    at: usize,
    then: Box<dyn Fn() + 'a>,
}

#[cfg(unix)]
impl Kernel for Racing<'_> {
    fn job(&self, agent: u32) -> Option<(Job, u32)> {
        if self.script.reads.get() == self.at {
            (self.then)();
        }
        self.script.job(agent)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.script.parent(pid)
    }

    fn terminal(&self, pid: u32) -> Option<(u32, String)> {
        self.script.terminal(pid)
    }
}

/// A restart never resumes a conversation whose transcript went after its step (Claude
/// Code refuses that `--resume` and exits, leaving the tab without an agent): it restarts
/// afresh (`CAUSE_UPGRADE_FRESH`, `restarted afresh`). Negative control: the transcript
/// there, it resumes.
#[cfg(unix)]
#[test]
fn a_restart_never_resumes_a_conversation_whose_transcript_went_after_its_step() {
    use super::super::relaunch::CAUSE_UPGRADE_FRESH;
    use std::os::unix::process::ExitStatusExt as _;
    let run = |name: &str, gone: bool| {
        let pending = St {
            phase: Phase::Pending,
            from: "1.0.0".to_string(),
            to: "9.9.9".to_string(),
            source: "managed".to_string(),
            last_seq: 77,
            seq_since_s: now_s() - 3_600,
            ..St::default()
        };
        let mut h = quietly(|| Parked::new(name, Answers::default(), pending, 3_600));
        let transcript = h
            .opts
            .home
            .join(format!(".claude/projects/p/{SESSION}.jsonl"));
        let ended = h.reaped();
        let mut shell = tty_zsh();
        let table = vec![(shell.id(), 1, "zsh".to_string())];
        // The visit's first job read is the step's; the second, after it.
        let kernel = Racing {
            script: Script::new(shell.id(), usize::MAX, None),
            at: 1,
            then: Box::new(|| {
                if gone {
                    let _ = std::fs::remove_file(&transcript);
                }
            }),
        };
        let _quick = QuickWaits::new();
        HELD_WAIT_IN_TEST.with(|w| w.set(Some(Duration::from_millis(100))));
        let r = visit(&h.opts, &h.sf, &table, &newer(), &kernel);
        HELD_WAIT_IN_TEST.with(|w| w.set(None));
        let status = ended.recv_timeout(HANG_DETECTOR).expect("the agent ended");
        let _ = shell.kill();
        let _ = shell.wait();
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{name}: {r:?}");
        let st = load(&h.opts, SESSION).expect("record");
        (st, h.details("terminated"))
    };
    let (st, terminated) = run("resume-gone", true);
    assert!(!st.line.contains("--resume"), "{}", st.line);
    assert_eq!(st.cause, CAUSE_UPGRADE_FRESH);
    assert_eq!(
        terminated,
        ["SIGTERM (no task: restarted afresh, nothing typed or resumed)"]
    );
    // Negative control: its transcript there, resumed as it was.
    let (st, terminated) = run("resume-kept", false);
    assert!(st.line.contains("'--resume'"), "{}", st.line);
    assert_eq!(st.cause, "");
    assert_eq!(
        terminated,
        ["SIGTERM (idle: nothing asked or typed; resumed as it was)"]
    );
}

/// The quiet upgrade (the default) restarts a tasked conversation at an idle point with
/// nothing typed first, resuming it (`--resume`, the launch's flags); a round first seen
/// behind at this look waits `notice-shown`. The knob (`upgrade_ask = true`, every other
/// test here) announces instead.
#[cfg(unix)]
#[test]
fn a_quiet_upgrade_restarts_an_idle_conversation_with_nothing_typed_first() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let kernel = || Script::new(shell, usize::MAX, None);
    let (r, _, typed) = quietly(|| visit_holding("quiet-idle", &asked, true, &kernel()));
    assert_eq!(r.step, "would-restart", "{r:?}");
    assert_eq!(typed, 0);
    // The knob (`upgrade_ask`): the notice.
    let (r, _, _) = visit_holding("ask-idle", &asked, true, &kernel());
    assert_eq!(r.step, "would-announce", "{r:?}");
    // The person is told first: first seen behind at this look.
    let just_behind = |opts: &Opts| {
        let mut st = load(opts, SESSION).expect("seeded");
        st.pending_since = now_s();
        std::fs::write(state_path(opts, SESSION), st.to_json()).expect("write");
    };
    for (name, rows, quiet) in [
        ("untold-quiet", &asked, true),
        ("untold-taskless-quiet", &own, true),
        ("untold-taskless-ask", &own, false),
    ] {
        let visit = || {
            visit_launched(
                name,
                rows,
                true,
                &kernel(),
                77,
                None,
                None,
                Answers::default(),
                &[],
                &just_behind,
            )
        };
        let (r, _, typed) = if quiet { quietly(visit) } else { visit() };
        assert_eq!(r.step, "wait:notice-shown", "{name}: {r:?}");
        assert!(!typed.iter().any(|l| l.contains(" turn ")), "{name}");
    }
    // The run up to the signal: every last look kept, nothing typed, and the
    // planned line resumes the conversation.
    let (r, saved, typed) =
        quietly(|| visit_holding("quiet-run", &asked, false, &Script::new(shell, 4, None)));
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert_eq!(typed, 0, "no notice, nothing typed");
    let saved = saved.expect("state");
    assert_eq!(saved.phase, Phase::Pending, "never signalled");
    assert!(
        saved.line.contains("--resume"),
        "resumed, not afresh: {}",
        saved.line
    );
    assert!(
        saved.marker.is_empty() && saved.asked.is_empty(),
        "no READY asked"
    );
}

/// The upgrade's relaunch line re-applies exactly the login the agent's exec environment
/// names (an account move's `work-2` stays `work-2`), and the record keeps it. Negative
/// control: no login in the environment, none on the line or the record.
#[cfg(unix)]
#[test]
fn the_upgrades_own_relaunch_types_back_the_login_the_agent_runs_under() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let run = |name: &str, env: &[String]| {
        let (r, saved, _) = quietly(|| {
            visit_launched_env(
                name,
                &asked,
                false,
                &Script::new(shell, 4, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                &|_| {},
                env,
            )
        });
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        saved.expect("state")
    };
    let moved = run(
        "upgrade-login-dir",
        &[format!("CLAUDE_SECURESTORAGE_CONFIG_DIR={LOGIN_DIR}")],
    );
    assert!(
        moved.line.contains(LOGIN_WORD) && moved.line.contains("--resume"),
        "{}",
        moved.line
    );
    assert_eq!(moved.login.as_deref(), Some(LOGIN_DIR));
    let named = run(
        "upgrade-login-empty",
        &["CLAUDE_SECURESTORAGE_CONFIG_DIR=".to_string()],
    );
    assert!(
        named.line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR='' "),
        "{}",
        named.line
    );
    assert_eq!(named.login.as_deref(), Some(""));
    // Negative control.
    let plain = run("upgrade-login-none", &[]);
    assert!(
        !plain.line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR") && plain.line.contains("--resume"),
        "{}",
        plain.line
    );
    assert_eq!(plain.login, None);
}

/// A quiet restart ([`St::quiet`]) and every account move (`relaunch::carried_quietly`)
/// are carried on with nothing: `done:resumed`, said once, nothing typed, the point left
/// to the tab's loop. Negative controls: an announced restart and a memory relaunch are
/// carried on.
#[cfg(unix)]
#[test]
fn a_quiet_restart_is_carried_on_with_nothing() {
    let asked = [
        user_row("Reply with one short sentence saying hello."),
        turn_by("claude-haiku-4-5", "Hello."),
    ];
    let rig = Rig::new("carry-quiet", &asked);
    let mut st = rig.st.clone();
    st.quiet = true;
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert!(rig.typed().is_empty(), "nothing typed: {:?}", rig.typed());
    assert_eq!(st.phase, Phase::Done);
    assert!(st.outcome.contains("nothing typed"), "{}", st.outcome);
    // Nothing waits for a model to confirm: never "unconfirmed", which reads as a failed check.
    assert!(
        st.outcome.contains("resumed while idle") && !st.outcome.contains("unconfirmed"),
        "{}",
        st.outcome
    );
    assert_eq!(rig.details("done:resumed").len(), 1, "said once");
    assert!(!moved("done:resumed"), "the point still stands");
    let rig = Rig::new("carry-account", &asked);
    let mut st = rig.st.clone();
    st.cause = "account:work-2".to_string();
    assert!(!st.quiet);
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:resumed", "{r:?}");
    assert!(rig.typed().is_empty(), "nothing typed: {:?}", rig.typed());
    assert!(st.outcome.contains("nothing typed"), "{}", st.outcome);
    assert!(!typed("done:resumed"), "no turn of the host's to await");
    // Negative controls.
    let rig = Rig::new("carry-announced", &asked);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_ne!(r.step, "done:resumed", "{r:?}");
    assert_eq!(rig.typed().len(), 1, "{r:?}");
    let rig = Rig::new("carry-memory", &asked);
    let mut st = rig.st.clone();
    st.cause = super::super::relaunch::CAUSE_MEMORY.to_string();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "continued", "{r:?}");
    assert_eq!(rig.typed().len(), 1, "{r:?}");
}

/// A pending round that still owes its agent a release is no quiet restart ([`St::quiet`]
/// false): a notice reached that agent, which may have wound down. It goes through the
/// notice's fence ([`St::notice_belongs_to`]) and, stopped at the last look, puts the
/// release back. Negative controls: owing nothing, or owing another process, is quiet.
#[cfg(unix)]
#[test]
fn a_pending_round_that_owes_its_agent_a_release_is_carried_on_not_quiet() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let owing = |to_this: bool| {
        move |opts: &Opts| {
            let sf = session_files(&opts.home)
                .and_then(|files| files.into_iter().find(|f| f.session_id == SESSION))
                .expect("the stand-in's session file");
            let mut st = load(opts, SESSION).expect("seeded");
            st.release = "retargeted".to_string();
            st.tab = TAB.to_string();
            st.notice_pid = if to_this { sf.pid } else { dead_pid() };
            st.notice_start = squash(&sf.proc_start);
            std::fs::write(state_path(opts, SESSION), st.to_json()).expect("write");
        }
    };
    let run = |name: &str, seed: &dyn Fn(&Opts)| {
        quietly(|| {
            visit_launched(
                name,
                &asked,
                false,
                &Script::new(shell, 4, None),
                77,
                None,
                None,
                Answers::default(),
                &[],
                seed,
            )
        })
    };
    let (r, saved, typed) = run("owes-here", &owing(true));
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert!(!typed.iter().any(|l| l.contains(" turn ")), "nothing typed");
    let saved = saved.expect("state");
    assert!(!saved.quiet, "carried on, not resumed as if never asked");
    assert_eq!(saved.phase, Phase::Pending, "never signalled");
    assert_eq!(saved.release, "retargeted", "the release put back");
    // Negative controls: owing nothing, and owing another process.
    for (name, seed) in [
        ("owes-nothing", &(|_: &Opts| {}) as &dyn Fn(&Opts)),
        ("owes-elsewhere", &owing(false)),
    ] {
        let (r, saved, _) = run(name, seed);
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        assert!(saved.expect("state").quiet, "{name}: a quiet restart");
    }
}

/// The tab's own supervisor's words (`keep going`, `answer_text`, found in its loop's
/// ledger under [`Opts::aterm_state`]) are no task: such a record is restarted afresh.
/// Negative controls: no ledger, or a ledger of other words, is announced to.
#[cfg(unix)]
#[test]
fn what_the_tabs_own_supervisor_typed_is_no_task() {
    let shell = dead_pid();
    let cfg = crate::supervise::SupervisorConfig::default();
    let rows = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
        // Recorded as the loop typed them: its ledger's rows are written at
        // 1970 + 1 s ([`visit_launched`]).
        user_row_at(&cfg.continue_text, "1970-01-01T00:00:01.000Z"),
        turn_by("claude-haiku-4-5", "What would you like me to work on?"),
        user_row_at(&cfg.answer_text, "1970-01-01T00:00:01.000Z"),
        turn_by("claude-haiku-4-5", "Please tell me what to do."),
    ];
    let typed = [cfg.continue_text.as_str(), cfg.answer_text.as_str()];
    for (name, ledger, want) in [
        ("ours-ledgered", Some(&typed[..]), "would-restart:fresh"),
        ("ours-unknown", None, "would-announce"),
        ("ours-other", Some(&["carry on"][..]), "would-announce"),
    ] {
        let (r, _, typed) = visit_seen(
            name,
            &rows,
            true,
            &Script::new(shell, usize::MAX, None),
            77,
            None,
            ledger,
        );
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}: a dry run types nothing");
    }
}

/// The tab's ledger spans every conversation it held: the loop's `keep going` in a new
/// conversation, recorded at another time, is a person's, so it is announced to, never
/// restarted afresh. Control: [`what_the_tabs_own_supervisor_typed_is_no_task`].
#[cfg(unix)]
#[test]
fn the_loops_words_in_another_conversation_are_no_turn_of_this_one() {
    let shell = dead_pid();
    let cfg = crate::supervise::SupervisorConfig::default();
    // The ledger's rows are written at 1970 + 1 s ([`visit_launched`]).
    let rows = vec![
        user_row_at(&cfg.continue_text, "2026-09-30T12:00:00.000Z"),
        turn_by("claude-haiku-4-5", "Picking up the plan where it stopped."),
    ];
    let (r, _, typed) = visit_seen(
        "another-conversation",
        &rows,
        true,
        &Script::new(shell, usize::MAX, None),
        77,
        None,
        Some(&[cfg.continue_text.as_str()][..]),
    );
    assert_eq!(r.step, "would-announce", "{r:?}");
    assert_eq!(typed, 0, "a dry run types nothing");
}

/// The fresh restart is the upgrade's restart without the notice and without `--resume`:
/// nothing typed, the newer build on the launch's flags alone, and every last look kept
/// (here one backgrounds the agent, so nothing is signalled and the state goes back).
#[cfg(unix)]
#[test]
fn a_fresh_restart_types_nothing_plans_no_resume_and_keeps_every_last_look() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    // Foreground for the visit's two job reads and the restart's two, then
    // backgrounded: the read right before the signal refuses it.
    let (r, saved, typed) =
        visit_holding("fresh-restart", &own, false, &Script::new(shell, 4, None));
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert_eq!(typed, 0, "no notice, nothing typed");
    let saved = saved.expect("state");
    assert_eq!(saved.phase, Phase::Pending, "never signalled");
    assert!(saved.cause.is_empty(), "{:?}", saved.cause);
    assert!(saved.line.contains("/nonexistent/claude"), "{}", saved.line);
    assert!(!saved.line.contains("--resume"), "afresh: {}", saved.line);
}

/// The upgrade resumes in the mode the tab's pill shows: Claude Code writes its
/// `permission-mode` row only when it re-appends session metadata, so the transcript can
/// lag a mode change. Negative control: with no pill, the transcript's mode decides.
#[cfg(unix)]
#[test]
fn an_upgrade_resumes_in_the_mode_the_pill_shows_not_the_transcripts_older_one() {
    let shell = dead_pid();
    let mode_row = |mode: &str| {
        format!(r#"{{"type":"permission-mode","permissionMode":"{mode}","sessionId":"{SESSION}"}}"#)
    };
    let rows = |mode: &str| {
        vec![
            harness_row("Claude Code 2.1.283 (managed) is installed"),
            mode_row(mode),
            turn_by("claude-haiku-4-5", "I cannot stop now."),
        ]
    };
    let rule = "─".repeat(20);
    let auto_pill = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}","  ⏵⏵ auto mode on (shift+tab to cycle)"],"cursor":{{"row":1,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    );
    let bypass = ["--dangerously-skip-permissions"];
    for (name, transcript, screen, launch, want, gone) in [
        // Launched in bypass, left in auto: auto, the bypass flag gone.
        (
            "pill-auto-over-bypass",
            "bypassPermissions",
            auto_pill.clone(),
            &bypass[..],
            Some("'--permission-mode' 'auto'"),
            Some("'--dangerously-skip-permissions'"),
        ),
        // Plan left for auto: auto, never plan.
        (
            "pill-auto-over-plan",
            "plan",
            auto_pill.clone(),
            &[][..],
            Some("'--permission-mode' 'auto'"),
            Some("'plan'"),
        ),
        // Negative control: no pill drawn — the transcript's plan.
        (
            "no-pill-transcript-plan",
            "plan",
            idle_screen(),
            &[][..],
            Some("'--permission-mode' 'plan'"),
            Some("'auto'"),
        ),
        // Negative control: no pill, the transcript's bypass — the launch's
        // own flags, never moved.
        (
            "no-pill-transcript-bypass",
            "bypassPermissions",
            idle_screen(),
            &bypass[..],
            Some("'--dangerously-skip-permissions'"),
            Some("'--permission-mode'"),
        ),
    ] {
        // Foreground for the visit's two job reads and the restart's two,
        // then backgrounded: the read right before the signal refuses it,
        // the planned line kept on the record.
        let (r, saved, _) = visit_launched(
            name,
            &rows(transcript),
            false,
            &Script::new(shell, 4, None),
            77,
            None,
            None,
            Answers {
                screen,
                ..Answers::default()
            },
            launch,
            &|_| {},
        );
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        let line = saved.expect("state").line;
        if let Some(want) = want {
            assert!(line.contains(want), "{name}: {line}");
        }
        if let Some(gone) = gone {
            assert!(!line.contains(gone), "{name}: {line}");
        }
    }
}

/// The announcement plans its relaunch line in the mode the pill shows, as the restart
/// does, and keeps the line. Negative control: no pill, the launch's own flags.
#[cfg(unix)]
#[test]
fn an_announcement_is_planned_in_the_mode_the_pill_shows() {
    let shell = dead_pid();
    let rows = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        format!(
            r#"{{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"{SESSION}"}}"#
        ),
        user_row("fix the parser"),
        turn_by("claude-haiku-4-5", "Fixed."),
    ];
    let rule = "─".repeat(20);
    let auto_pill = format!(
        r#"{{"rows":["{rule}","❯ ","{rule}","  ⏵⏵ auto mode on (shift+tab to cycle)"],"cursor":{{"row":1,"col":2}},"seq":77,"human_ms":null,"first":0}}"#
    );
    for (name, screen, want, gone) in [
        (
            "announce-pill-auto",
            auto_pill,
            "'--permission-mode' 'auto'",
            "'--dangerously-skip-permissions'",
        ),
        (
            "announce-no-pill",
            idle_screen(),
            "'--dangerously-skip-permissions'",
            "'--permission-mode'",
        ),
    ] {
        let (r, saved, _) = visit_launched(
            name,
            &rows,
            false,
            &Script::new(shell, usize::MAX, None),
            77,
            None,
            None,
            Answers {
                screen,
                ..Answers::default()
            },
            &["--dangerously-skip-permissions"],
            &|_| {},
        );
        assert_eq!(r.step, "announced:1", "{name}: {r:?}");
        let line = saved.expect("state").line;
        assert!(line.contains(want), "{name}: {line}");
        assert!(!line.contains(gone), "{name}: {line}");
    }
}

/// A model chosen by hand (`/model`, answered on since) rides the relaunch line when the
/// list moves none, in the announcement's plan and the fresh restart's. Negative control:
/// no `/model` remembered keeps the launch's flags.
#[cfg(unix)]
#[test]
fn an_upgrade_carries_a_model_chosen_by_hand_when_the_list_moves_none() {
    let shell = dead_pid();
    // Answered past the stand-in agent's start (its clock is now).
    let later = "2099-01-01T00:00:00Z";
    let task = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        user_row("fix the parser"),
        answer_at("claude-opus-5-5", later),
    ];
    let fresh = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        answer_at("claude-opus-5-5", later),
    ];
    let launch = ["--model", "claude-sonnet-5-5"];
    let chose = |opts: &Opts| chose_by_hand(opts, "claude-opus-5-5");
    for (name, rows, kernel, step, chosen) in [
        ("hand-announce", &task, usize::MAX, "announced:1", true),
        ("hand-fresh", &fresh, 4, "wait:changed-before-signal", true),
        ("nobody-announce", &task, usize::MAX, "announced:1", false),
        (
            "nobody-fresh",
            &fresh,
            4,
            "wait:changed-before-signal",
            false,
        ),
    ] {
        let seed: &dyn Fn(&Opts) = if chosen { &chose } else { &|_| {} };
        let (r, saved, _) = visit_launched(
            name,
            rows,
            false,
            &Script::new(shell, kernel, None),
            77,
            None,
            None,
            Answers::default(),
            &launch,
            seed,
        );
        assert_eq!(r.step, step, "{name}: {r:?}");
        let saved = saved.expect("state");
        assert!(saved.model_list.is_empty(), "{name}: the list moved none");
        let (want, gone) = if chosen {
            ("'claude-opus-5-5'", "'claude-sonnet-5-5'")
        } else {
            ("'claude-sonnet-5-5'", "'claude-opus-5-5'")
        };
        assert!(saved.line.contains(want), "{name}: {}", saved.line);
        assert!(!saved.line.contains(gone), "{name}: {}", saved.line);
    }
}

/// A hand `/model` choice on a bucket's fallback is relaunched and, once the signal is
/// sent, saved as the fallback's origin, so the way back does not undo it. Negative
/// controls: no `/model` keeps the old origin; a restart stopped before its signal leaves it.
#[cfg(unix)]
#[test]
fn an_upgrade_restart_makes_the_hand_choice_it_carries_the_fallbacks_origin() {
    let shell = dead_pid();
    let origin = "claude-fable-5-1";
    let hand = "claude-opus-5-5";
    // Answered past the stand-in agent's start (its clock is now).
    let later = "2099-01-01T00:00:00Z";
    let rows = |model: &str| {
        vec![
            harness_row("Claude Code 2.1.283 (managed) is installed"),
            answer_at(model, later),
        ]
    };
    let seed = |chose: bool| {
        move |opts: &Opts| {
            let mut st = load(opts, SESSION).expect("state");
            st.fallback_from = Some(origin.to_string());
            save(opts, SESSION, &st);
            if chose {
                chose_by_hand(opts, hand);
            }
        }
    };
    let refused = Answers {
        signal: "ERR busy",
        ..Answers::default()
    };
    // (case, a `/model` made, job reads in the foreground, the server's
    // answers, the step, the origin saved)
    let cases = [
        (
            "origin-hand-sent",
            true,
            usize::MAX,
            Answers::default(),
            None,
            hand,
        ),
        (
            "origin-nobody-sent",
            false,
            usize::MAX,
            Answers::default(),
            None,
            origin,
        ),
        (
            "origin-hand-changed",
            true,
            4,
            Answers::default(),
            Some("wait:changed-before-signal"),
            origin,
        ),
        (
            "origin-hand-refused",
            true,
            usize::MAX,
            refused,
            Some("wait:signal-held"),
            origin,
        ),
    ];
    // Side by side: a sent restart waits out the stand-in shell's prompt.
    std::thread::scope(|scope| {
        let runs: Vec<_> = cases
            .into_iter()
            .map(|(name, chose, foreground, answers, step, want)| {
                let seed = seed(chose);
                scope.spawn(move || {
                    let model = if chose { hand } else { "claude-sonnet-5-5" };
                    let (r, saved, asked) = visit_launched(
                        name,
                        &rows(model),
                        false,
                        &Script::new(shell, foreground, None),
                        77,
                        None,
                        None,
                        answers,
                        &["--model", "claude-sonnet-5-5"],
                        &seed,
                    );
                    let signalled = asked.iter().any(|l| l.contains(" signal "));
                    match step {
                        Some(step) => assert_eq!(r.step, step, "{name}: {r:?}"),
                        None => assert!(signalled, "{name}: {r:?}"),
                    }
                    let saved = saved.expect("state");
                    assert_eq!(saved.fallback_from.as_deref(), Some(want), "{name}: {r:?}");
                    assert!(
                        saved.line.contains(&format!("'--model' '{model}'")),
                        "{name}: {}",
                        saved.line
                    );
                })
            })
            .collect();
        for run in runs {
            run.join().expect("case");
        }
    });
}

/// Unknown process identity, running background work or an unreadable process table vetoes the
/// restart's last look (`wait:changed`), while the same fixture with stable facts reaches the
/// fake signal's refusal (`wait:signal-held`). No probe failure becomes leave to signal or
/// moves the fallback origin.
#[cfg(unix)]
#[test]
fn the_restart_last_look_keeps_unknown_identity_and_background_work_fenced() {
    let shell = dead_pid();
    for (name, known, background, expected) in [
        ("last-look-stable", true, Vec::new(), "wait:signal-held"),
        ("last-look-unknown", false, Vec::new(), "wait:changed"),
        (
            "last-look-work",
            true,
            vec!["sh".to_string()],
            "wait:changed",
        ),
        (
            "last-look-unreadable",
            true,
            vec![UNREADABLE_TABLE.to_string()],
            "wait:changed",
        ),
    ] {
        let script = Script::new(shell, usize::MAX, None);
        let (report, saved, asked) = visit_launched_probed(
            name,
            &[
                harness_row("Claude Code 2.1.283 (managed) is installed"),
                answer_at("claude-opus-5-5", "2099-01-01T00:00:00Z"),
            ],
            false,
            &script,
            77,
            None,
            None,
            Answers {
                signal: "ERR busy",
                ..Answers::default()
            },
            &["--model", "claude-sonnet-5-5"],
            &|opts| {
                let mut st = load(opts, SESSION).expect("state");
                st.fallback_from = Some("claude-fable-5-1".to_string());
                save(opts, SESSION, &st);
                chose_by_hand(opts, "claude-opus-5-5");
            },
            &[],
            |pid, start| ParkedProcess {
                script: &script,
                pid,
                start: known.then_some(start),
                background,
            },
        );
        assert_eq!(report.step, expected, "{name}: {report:?}");
        assert_eq!(
            asked.iter().any(|line| line.contains(" signal ")),
            known && name == "last-look-stable",
            "{name}: {asked:?}"
        );
        assert_eq!(
            saved.expect("state").fallback_from.as_deref(),
            Some("claude-fable-5-1")
        );
    }
}

/// A restart takes the tab's hand ([`Hand`]) before its last look, so a first prompt sent to
/// the bare shell cannot run as a command line, and gives it back when that look stops it.
/// Negative control: another driver's live lease waits `held`, reading and signalling nothing.
#[cfg(unix)]
#[test]
fn a_restart_holds_the_tab_from_its_last_look_and_gives_it_back_when_it_stops() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let acquire = format!(
        "lease acquire ttl={HAND_TTL_MS} holder={} hard",
        hand_name()
    );
    let release = format!("lease release holder={}", hand_name());
    let (r, saved, asked) = visit_answered(
        "hand-taken",
        &own,
        false,
        &Script::new(shell, 4, None),
        77,
        None,
        None,
        Answers {
            lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000",
            ..Answers::default()
        },
    );
    assert_eq!(r.step, "wait:changed-before-signal", "{r:?}");
    assert_eq!(saved.expect("state").phase, Phase::Pending);
    let at = |needle: &str| asked.iter().position(|l| l.ends_with(needle));
    let taken = at(&acquire).unwrap_or_else(|| panic!("the hand taken: {asked:#?}"));
    let given = at(&release).unwrap_or_else(|| panic!("the hand given back: {asked:#?}"));
    assert!(taken < given, "{asked:#?}");
    // The last look — its screen and its status — is read under the hand.
    for read in [" text ", " status"] {
        assert!(
            asked[taken..given].iter().any(|l| l.contains(read)),
            "{read} under the hand: {asked:#?}"
        );
    }
    assert!(!asked.iter().any(|l| l.contains(" signal ")), "{asked:#?}");
    assert_eq!(turns_in(&asked), 0, "nothing typed");
    // Negative control: another driver holds the tab.
    let (r, saved, asked) = visit_answered(
        "hand-theirs",
        &own,
        false,
        &Script::new(shell, usize::MAX, None),
        77,
        None,
        None,
        Answers {
            lease: "ERR lease held holder=orchestrator expires_in_ms=5000",
            ..Answers::default()
        },
    );
    assert_eq!(r.step, "wait:held", "{r:?}");
    assert_eq!(saved.expect("state").phase, Phase::Pending);
    let taken = at_in(&asked, &acquire).unwrap_or_else(|| panic!("asked: {asked:#?}"));
    assert!(
        !asked[taken..].iter().any(|l| l.contains(" text ")),
        "no last look: {asked:#?}"
    );
    assert!(!asked.iter().any(|l| l.contains(" signal ")), "{asked:#?}");
    assert!(!asked.iter().any(|l| l.ends_with(&release)), "{asked:#?}");
}

/// A key (`human_ms` inside the grace) or an unread prompt (`input=pending`) after the
/// restart's first look stops it before its signal, state and hand given back. The signal
/// itself is one server decision (`signal term pid=<p> quiet=<grace>`) that a later
/// keystroke refuses (`wait:signal-held`).
#[cfg(unix)]
#[test]
fn a_key_or_a_prompt_after_the_first_look_stops_the_restart_before_its_signal() {
    let shell = dead_pid();
    let own = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
    ];
    let release = format!("lease release holder={}", hand_name());
    let restart = |name: &str, after_first_look: Option<&'static str>, signal: &'static str| {
        visit_answered(
            name,
            &own,
            false,
            &Script::new(shell, usize::MAX, None),
            77,
            None,
            None,
            Answers {
                lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1",
                after_first_look,
                signal,
                ..Answers::default()
            },
        )
    };
    for (name, later) in [
        ("look-person", "OK schema=1 hold=0 hand=- human_ms=500"),
        ("look-prompt", "OK schema=1 hold=0 hand=- input=pending"),
    ] {
        let (r, saved, asked) = restart(name, Some(later), "");
        assert_eq!(r.step, "wait:changed-before-signal", "{name}: {r:?}");
        let saved = saved.expect("state");
        assert_eq!(saved.phase, Phase::Pending, "{name}: back as it was");
        assert!(saved.cause.is_empty(), "{name}: {saved:?}");
        assert!(
            !asked.iter().any(|l| l.contains(" signal ")),
            "{name}: {asked:#?}"
        );
        assert!(
            asked.last().is_some_and(|l| l.ends_with(&release)),
            "{name}: the hand given back: {asked:#?}"
        );
    }
    // A keystroke after the last look: the signal refuses itself.
    let (r, saved, asked) = restart("look-signal", None, "ERR busy person human_ms=300");
    assert_eq!(r.step, "wait:signal-held", "{r:?}");
    assert_eq!(saved.expect("state").phase, Phase::Pending);
    let signal = asked
        .iter()
        .find(|l| l.contains(" signal "))
        .unwrap_or_else(|| panic!("the signal asked: {asked:#?}"));
    assert!(signal.ends_with(" quiet=120"), "{signal}");
    assert!(
        asked.last().is_some_and(|l| l.ends_with(&release)),
        "the hand given back: {asked:#?}"
    );
}

/// A refused signal takes back the model ask its restart recorded, and the earlier ask it
/// replaced stands again, time and all; kept, it would settle failed with nothing relaunched.
#[cfg(unix)]
#[test]
fn a_refused_signal_takes_back_the_model_it_asked() {
    let dir = scratch("ask-refused");
    let (sock, asked) = instance_with(
        &dir,
        Answers {
            lease: "OK lease acquired holder=x ttl_ms=60000 expires_in_ms=60000 hard=1",
            signal: "ERR busy person human_ms=300",
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = parked_bare().spawn().expect("spawn");
    let pid = agent.id();
    wait_exec(pid);
    let sf = register(&opts.home, pid, SESSION);
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    let body = [
        ASKED.to_string(),
        turn_by("claude-opus-5", &format!("Saved.\\n{marker}")),
    ]
    .join("\n")
        + "\n";
    std::fs::write(project.join(format!("{SESSION}.jsonl")), &body).expect("transcript");
    let shell = dead_pid();
    let now = now_s();
    let st = St {
        phase: Phase::Announced {
            at_s: now - 60,
            asks: 1,
        },
        marker: marker.to_string(),
        model_list: "claude-opus-5-5".to_string(),
        tab: TAB.to_string(),
        notice_pid: sf.pid,
        notice_start: squash(&sf.proc_start),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        last_seq: 77,
        seq_since_s: now - 3600,
        ..St::default()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, SESSION), st.to_json()).expect("write");
    // An earlier ask of this harness's, inside its settle window.
    let earlier = ModelRecord {
        set: "claude-fable-5-1".to_string(),
        set_at: now - 10,
        ..ModelRecord::default()
    };
    save_model_record(&opts, SESSION, &earlier);
    let args = atpkg::caller_shell::process_args(pid).expect("agent argv");
    let files = session_files(&opts.home);
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &[(shell, 1, "zsh".to_string())],
        &newer(),
        &Script::new(shell, usize::MAX, None),
        Some(&args),
        None,
    );
    let record = load_model_record(&opts, SESSION);
    let saved = load(&opts, SESSION).expect("state");
    let asked = asked.lock().map(|a| a.clone()).unwrap_or_default();
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(r.step, "wait:signal-held", "{r:?}\n{asked:#?}");
    assert!(
        saved.line.contains("'--model' 'claude-opus-5-5'"),
        "the restart asked the model: {}",
        saved.line
    );
    assert_eq!(
        (record.set.as_str(), record.set_at),
        ("claude-fable-5-1", now - 10),
        "the refused restart's ask taken back, the one before it standing"
    );
    assert!(record.failed.is_empty(), "{record:?}");
    assert_eq!(
        (saved.model_list.as_str(), saved.marker.as_str()),
        ("claude-opus-5-5", marker),
        "the round, its model and its READY stand"
    );
}

/// The restart in place records a model ask only once the relaunch line carrying it is
/// typed (an ask recorded with no line would settle failed and bar the model): a refused
/// signal, or one whose line is never typed, leaves the record as it was. Negative
/// control: the line typed at the shell the roster names records the ask.
#[cfg(unix)]
#[test]
fn a_restart_in_place_records_its_model_ask_only_with_the_line_that_asks_it() {
    let case = |name: &str, signal: &'static str, roster: bool| {
        let dir = scratch(name);
        let mut shell = tty_zsh();
        let who = format!(
            "OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={}",
            shell.id()
        );
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                signal,
                who: roster.then_some(who),
                ..Answers::default()
            },
        );
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        seed_builds(&opts);
        let mut agent = Some(parked_bare().spawn().expect("agent"));
        let pid = agent.as_ref().map_or(0, aterm_uds::spawnfd::Child::id);
        wait_exec(pid);
        register(&opts.home, pid, SESSION);
        std::fs::create_dir_all(state_dir(&opts)).expect("state");
        write_transcript(
            &opts.home,
            &[answer_at("claude-opus-5", "2099-01-01T00:00:00Z")],
        );
        let cat = opts.home.join(".claude/cache/model-catalog");
        std::fs::create_dir_all(&cat).expect("catalog dir");
        std::fs::write(
            cat.join("a-cc.json"),
            r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"main"},{"id":"claude-opus-5","section":"main"}]}}}"#,
        )
        .expect("catalog");
        let snap = Snapshot {
            stored_login: false,
            tab: TAB.to_string(),
            pid,
            start: kernel_start(pid).expect("lstart"),
            shell: shell.id(),
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["/opt/claude/bin/claude".to_string()],
            session: Some(SESSION.to_string()),
            cwd: "/".to_string(),
            version: Some("1.0.0".to_string()),
            codex: None,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        // Reaped as the stand-in's signal ends it; a signal refused leaves it
        // parked, ended below.
        let ended = signal.is_empty().then(|| {
            let mut sent = agent.take().expect("the agent");
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(sent.wait());
            });
            rx
        });
        HELD_WAIT_IN_TEST.with(|w| w.set(Some(Duration::from_millis(100))));
        let restart = || {
            crate::harness::relaunch::restart_with(
                &opts,
                &Restart::Memory,
                Ok(snap.clone()),
                &Script::new(shell.id(), usize::MAX, None),
            )
        };
        let mut r = restart();
        if let Some(ended) = &ended {
            // The stand-in is this test's child, alive to `kill(0)` until reaped: carry the
            // relaunch on, as the host's next idle point would, once it is.
            assert!(
                ended.recv_timeout(HANG_DETECTOR).is_ok(),
                "{name}: the agent ended"
            );
            r = restart();
        }
        HELD_WAIT_IN_TEST.with(|w| w.set(None));
        let record = load_model_record(&opts, SESSION);
        let typed = turns(&asked);
        if let Some(mut parked) = agent {
            let _ = parked.kill();
            let _ = parked.wait();
        }
        let _ = shell.kill();
        let _ = shell.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (r, record, typed)
    };
    // Three restarts of their own, two of them waiting out the relaunch's
    // fixed waits (the shell's prompt, the relaunched agent's): side by side.
    let case = &case;
    side_by_side(vec![
        Box::new(move || {
            // The signal refused: nothing relaunched, nothing asked.
            let (r, record, typed) = case("ask-refused", "ERR busy person human_ms=300", false);
            assert_eq!(r.step, "wait:signal-held", "{r:?}");
            assert_eq!(typed, 0);
            assert_eq!(
                (record.set.as_str(), record.failed.len()),
                ("", 0),
                "nothing relaunched asked it: {record:?}"
            );
        }),
        Box::new(move || {
            // The signal sent, the line never typed.
            let (r, record, typed) = case("ask-untyped", "", false);
            assert_eq!(r.step, "wait:tab-ownership-changed", "{r:?}");
            assert_eq!(typed, 0);
            assert_eq!(
                (record.set.as_str(), record.failed.len()),
                ("", 0),
                "no line asked it: {record:?}"
            );
        }),
        Box::new(move || {
            // Negative control: the typed line asks the model, and the record says so.
            let (r, record, typed) = case("ask-typed", "", true);
            assert_eq!(typed, 1, "{r:?}");
            assert_eq!(record.set, "claude-opus-5-5", "{r:?} {record:?}");
            assert!(record.failed.is_empty(), "{record:?}");
        }),
    ]);
}

/// The first request of `asked` that ends with `needle`.
#[cfg(unix)]
fn at_in(asked: &[String], needle: &str) -> Option<usize> {
    asked.iter().position(|l| l.ends_with(needle))
}

/// A relaunched conversation whose record holds only the harness's own turns is done
/// (`done:taskless`, said once) and nothing is typed. Negative control: a tasked one is
/// carried on.
#[cfg(unix)]
#[test]
fn a_conversation_with_no_task_is_carried_on_with_nothing() {
    let own = [
        harness_row("Relaunched: Claude Code exited without anyone asking"),
        turn_by(
            "claude-haiku-4-5",
            "What would you like me to help you with?",
        ),
    ];
    let rig = Rig::new("carry-taskless", &own);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_eq!(r.step, "done:taskless", "{r:?}");
    assert!(rig.typed().is_empty(), "nothing typed: {:?}", rig.typed());
    assert_eq!(st.phase, Phase::Done);
    assert_eq!(rig.details("done:taskless").len(), 1, "said once");
    // Negative control: asked something, carried on.
    let asked = [
        user_row("Reply with one short sentence saying hello."),
        turn_by("claude-haiku-4-5", "Hello."),
    ];
    let rig = Rig::new("carry-tasked", &asked);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_ne!(r.step, "done:taskless", "{r:?}");
    assert_eq!(rig.typed().len(), 1, "{r:?}");
}

/// The settle counts how long the server's own verdict has said the agent's turn is over
/// (`status agent=idle agent_since_ms=`), not since a new screen `seq`, which a repainted
/// dim suggestion would restart. Negative controls: idle 5 s, busy, no verdict, or
/// `unknown` over a moved screen wait `settling`.
#[cfg(unix)]
#[test]
fn the_settle_is_measured_from_the_agents_verdict_not_from_a_repaint() {
    let shell = dead_pid();
    let asked = vec![
        user_row("Staged harness test, please follow exactly."),
        turn_by("claude-haiku-4-5", "Stage 1 complete."),
    ];
    let kernel = || Script::new(shell, usize::MAX, None);
    let idle = |ms: u32| format!("OK schema=1 hold=0 hand=- agent=idle agent_since_ms={ms}");
    let other = |word: &str| format!("OK schema=1 hold=0 hand=- agent={word} agent_since_ms=90000");
    // (name, the seq the last look saw — the screen reads 77 — the status, the step)
    for (name, last_seq, status, want) in [
        ("settle-idle-30s", 76, Some(idle(30_000)), "would-announce"),
        ("settle-idle-5s", 76, Some(idle(5_000)), "wait:settling"),
        ("settle-busy", 77, Some(other("busy")), "wait:settling"),
        ("settle-no-verdict", 76, None, "wait:settling"),
        // No evidence either way: the screen's own samples decide.
        (
            "settle-unknown-moved",
            76,
            Some(other("unknown")),
            "wait:settling",
        ),
        (
            "settle-unknown-still",
            77,
            Some(other("unknown")),
            "would-announce",
        ),
    ] {
        let (r, _, typed) = visit_seen(
            name,
            &asked,
            true,
            &kernel(),
            last_seq,
            status.as_deref(),
            None,
        );
        assert_eq!(r.step, want, "{name}: {r:?}");
        assert_eq!(typed, 0, "{name}");
    }
}

/// A turn the harness types returns once its submit is taken, not when the answer ends:
/// the step runs while the loop is still, so an answer ending inside it would go unseen.
/// Negative control: a turn asked to settle on quiet waits the answer out.
#[cfg(unix)]
#[test]
fn a_typed_turn_returns_once_its_submit_is_taken_not_after_the_answer() {
    const HANG: Duration = Duration::from_secs(60);
    let answers = |answer: Duration| Answers {
        answer,
        ..Answers::default()
    };
    let long = || answers(2 * HANG);
    let asked = [
        user_row("Reply with one short sentence saying hello."),
        turn_by("claude-haiku-4-5", "Hello."),
    ];
    // The carry-on.
    let rig = Rig::with("turn-wait-carry", &asked, long());
    let mut st = rig.st.clone();
    let t0 = Instant::now();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    let took = t0.elapsed();
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{r:?}");
    assert!(typed[0].contains(&format!(" {TURN_WAIT} ")), "{typed:?}");
    assert!(took < HANG, "the carry-on waited the answer out: {took:?}");
    drop(rig);
    // Negative control: a turn that settles on quiet waits the stand-in's answer out; the
    // stand-in sleeps before it replies, so no load returns it sooner.
    let answer = Duration::from_secs(1);
    let dir = scratch("turn-wait-control");
    let (sock, _asked) = instance_with(&dir, answers(answer));
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let t0 = Instant::now();
    let (head, _) = c
        .request_counted(&format!("@{TAB} turn yield=0.2 idle=1500 timeout=30000 hi"))
        .expect("turn");
    assert!(head.starts_with("OK"), "{head}");
    assert!(
        t0.elapsed() >= answer,
        "the stand-in answers as the server settles"
    );
    let _ = std::fs::remove_dir_all(dir);
    // The notice.
    let dir = scratch("turn-wait-notice");
    let (sock, log) = instance_with(&dir, long());
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let t0 = Instant::now();
    assert_eq!(
        notice_fenced(&mut c, TAB, "[aterm harness] notice", None),
        Ok(())
    );
    assert!(t0.elapsed() < HANG, "the notice waited the answer out");
    assert!(
        log.lock()
            .expect("log")
            .iter()
            .any(|l| l.contains(" turn ") && l.contains(&format!(" {TURN_WAIT} "))),
        "{log:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// Upgrade stall tests through the real driver, and the Tier-1 bind of
/// `harness_upgrade_never_strands_model`.
#[path = "upgrade_stall_tests.rs"]
mod stall;

/// A stop that repeats rests longer, never for good: the same reason in a row doubles the
/// rest up to four times `RETRY_S`; a new reason starts over, and the rest still ends.
#[test]
fn a_stop_that_repeats_rests_longer_and_never_for_good() {
    let now = 1_000_000;
    let rest = |st: &St| st.failed_at.saturating_add(upgrade::RETRY_S) - now;
    let mut st = St::default();
    st.fail("relaunch-refused", now);
    assert_eq!(rest(&st), upgrade::RETRY_S, "the first stop rests RETRY_S");
    let mut rests = Vec::new();
    for _ in 0..5 {
        st.fail("relaunch-refused", now);
        rests.push(rest(&st));
    }
    let r = upgrade::RETRY_S;
    assert_eq!(
        rests,
        [2 * r, 4 * r, 4 * r, 4 * r, 4 * r],
        "doubling, then capped"
    );
    assert_eq!(st.stop_streak, 6);
    st.fail("signal-refused", now);
    assert_eq!(
        (st.stop_streak, rest(&st)),
        (1, r),
        "a new reason starts over"
    );
    // The stretched rest still ends: the round re-arms once it has passed.
    st.fail("signal-refused", now);
    let due_at = st.failed_at + upgrade::RETRY_S;
    assert!(!upgrade::retry_due(
        &st.phase,
        st.failed_for(due_at - 1),
        false
    ));
    assert!(upgrade::retry_due(&st.phase, st.failed_for(due_at), false));
    // And it survives a restart of the sweep: the streak is on disk.
    let back = St::from_json(&st.to_json()).expect("parses");
    assert_eq!(
        (back.stop_streak, back.streak_why.as_str()),
        (2, "signal-refused")
    );
}

/// The login wall: no notice into it, no ask spent on a notice it answered, a
/// late READY honoured, and the Tier-1 bind of `harness_login_wall_model` to
/// the real code.
#[path = "upgrade_login_wall_tests.rs"]
mod login_wall;

/// The record's progress stamp and its deadlines.
#[path = "upgrade_watch_tests.rs"]
mod watch;

/// The end of the agent's own work re-arms a given-up round (`rearmed:unanswered`) and
/// types its first notice in the same visit; an announced round there asks again at once.
/// Negative controls: with no work seen a given-up round rests and an announced one waits
/// for its answer, as it does inside the floor or past the day's budget.
#[cfg(unix)]
#[test]
fn the_end_of_the_agents_work_re_arms_and_asks_in_the_same_visit() {
    let gave_up = |work_seen: bool| St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        failed_at: now_s() - 3_600,
        streak_why: upgrade::GAVE_UP.to_string(),
        stop_streak: 1,
        work_seen,
        ..settled()
    };
    let h = Parked::new("work-end-rearm", Answers::default(), gave_up(true), 3600);
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(
        h.details("rearmed:unanswered").len(),
        1,
        "the re-arm is said"
    );
    assert_eq!(h.typed().len(), 1, "one notice");
    let st = load(&h.opts, SESSION).expect("the record");
    assert!(
        matches!(st.phase, Phase::Announced { asks: 1, .. }),
        "{:?}",
        st.phase
    );
    assert!(!st.work_seen, "spent with the notice");
    drop(h);
    // Negative control: no work seen since the notice: it rests.
    let h = Parked::new("work-end-none", Answers::default(), gave_up(false), 3600);
    assert_eq!(h.visit().step, "wait:failed");
    assert!(h.typed().is_empty());
    drop(h);

    // An announced round: asked again at once, its next ask — but never
    // sooner than the floor after the last one, and within the day's budget.
    let floor = upgrade::WORK_END_FLOOR_S;
    for (work_seen, since, spent, want) in [
        (true, floor + 60, 0, "announced:2"),
        (true, 60, 0, "wait:awaiting-ready"),
        (false, floor + 60, 0, "wait:awaiting-ready"),
        (
            true,
            floor + 60,
            upgrade::WORK_END_DAILY,
            "wait:awaiting-ready",
        ),
    ] {
        let h = Parked::new(
            &format!("we-{}{since}-{spent}", u8::from(work_seen)),
            Answers::default(),
            settled(),
            3600,
        );
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - since,
                asks: 1,
            },
            marker: "ATERM-UPGRADE-READY-00000001".to_string(),
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            noticed_at: now_s() - since,
            work_seen,
            work_end_at: (0..spent).map(|k| now_s() - 3_600 + u64::from(k)).collect(),
            ..settled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("write");
        assert_eq!(
            h.visit().step,
            want,
            "work seen: {work_seen}, {since} s since the notice, {spent} spent"
        );
        drop(h);
    }
}

/// `Not today` is a `--defer` on the session, carried to the next build by the retarget
/// arm: closing a record its session reached by hand ([`forget_settled_pending`]) keeps a
/// word still in force and closes a spent one.
#[test]
fn closing_a_reached_pending_record_keeps_the_owners_defer() {
    let dir = scratch("rv-defer");
    let opts = drive(&dir);
    let running = Version::parse("2.1.284").expect("v");
    let record = |request| St {
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 5,
        request,
        request_tab: TAB.to_string(),
        request_at: 10,
        ..St::default()
    };
    // A defer still in force: the record stays, and so does the word.
    let deferred = record(Request::DeferUntil(now_s() + 3_600));
    save(&opts, SESSION, &deferred);
    assert!(forget_settled_pending(&opts, SESSION, Some(&deferred), &running).is_none());
    let kept = load(&opts, SESSION).expect("the record stays");
    assert!(matches!(kept.request, Request::DeferUntil(_)));
    // The next build's record carries it (the retarget arm).
    let to285 = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.285").expect("v"),
        source: Source::Managed,
    };
    let next = St::for_target(Some(kept), &running, &to285, None, proc("B"), now_s());
    assert!(
        matches!(next.request, Request::DeferUntil(t) if t > now_s()),
        "the snooze holds for the next build: {:?}",
        next.request
    );
    // Negative controls: a spent defer and no word at all close the record.
    for request in [Request::DeferUntil(1), Request::None] {
        let spent = record(request);
        save(&opts, SESSION, &spent);
        assert!(forget_settled_pending(&opts, SESSION, Some(&spent), &running).is_some());
        assert!(load(&opts, SESSION).is_none());
    }
}

/// A caller that names no process gets the build test, which cannot tell a same-build
/// restart by hand from the record's process: every retargeting caller names its process,
/// except the Codex lane, whose conversation is its daemon's.
#[test]
fn an_unnamed_process_falls_back_to_the_build_test() {
    let running = Version::parse("2.1.283").expect("v");
    let to285 = Candidate {
        exe: PathBuf::from("/pkg/agents/claude"),
        version: Version::parse("2.1.285").expect("v"),
        source: Source::Managed,
    };
    // Minted behind 2.1.284 for process A (on 2.1.283); the owner restarted by
    // hand, still on 2.1.283: process B. A newer build lands.
    let rec = St {
        phase: Phase::Pending,
        from: "2.1.283".to_string(),
        to: "2.1.284".to_string(),
        tab: TAB.to_string(),
        pending_since: 100,
        proc_start: "A".to_string(),
        ..St::default()
    };
    let named = St::for_target(Some(rec.clone()), &running, &to285, None, proc("B"), 900);
    let unnamed = St::for_target(Some(rec), &running, &to285, None, Proc::default(), 900);
    assert_eq!(named.behind_since(), 900, "B's age begins now");
    assert_eq!(
        unnamed.behind_since(),
        100,
        "unnamed: the same build reads as the same process"
    );
}

// ---------------------------------------------------------------- the cadence of asking

/// An idle, settled agent at an idle point: the facts of a look that saw nothing
/// under it (the simulation adds what the adversary runs).
fn cadence_facts() -> Facts {
    Facts {
        status: "idle".to_string(),
        status_age_s: 600,
        composer_empty: true,
        approval_box: false,
        busy_footer: false,
        background: Vec::new(),
        held: false,
        quiet_s: 600,
        hold_s: 0,
        owner_now: false,
        attended: false,
        background_point: false,
        taskless: false,
        limited: false,
        login: false,
        undelivered: false,
        queued: false,
        ready_s: 0,
        idle_looks: upgrade::IDLE_LOOKS,
        failed_s: 0,
        work_ended: false,
        ..Facts::default()
    }
}

/// The work-end ask is exempt from the round's ask cap and clock, so the driver bounds the
/// fact: no sooner than [`upgrade::WORK_END_FLOOR_S`] after the latest notice (or the rest's
/// start), at most [`upgrade::WORK_END_DAILY`] in a rolling day.
#[test]
fn the_end_of_the_work_asks_no_sooner_than_its_floor_and_no_more_than_its_budget() {
    let now = 1_790_645_000;
    let floor = upgrade::WORK_END_FLOOR_S;
    let ended = |st: &St| {
        let mut facts = cadence_facts();
        st.time_work(&mut facts, now);
        facts.work_ended
    };
    let announced = |since: u64, asks: u32| St {
        phase: Phase::Announced {
            at_s: now - since,
            asks,
        },
        noticed_at: now - since,
        work_seen: true,
        ..St::default()
    };
    assert!(!ended(&announced(60, 2)), "inside the floor");
    assert!(!ended(&announced(floor - 1, 2)), "one second short");
    assert!(ended(&announced(floor, 2)), "at the floor");
    assert!(
        ended(&announced(floor + 60, 9)),
        "the ask count is not the bound"
    );
    // No work seen since the notice: nothing ended.
    assert!(!ended(&St {
        work_seen: false,
        ..announced(floor + 60, 2)
    }));
    // A resting round counts from its stop (`failed_at`, no streak).
    let resting = |stopped_ago: u64| St {
        phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
        failed_at: now - stopped_ago,
        work_seen: true,
        ..St::default()
    };
    assert!(!ended(&resting(60)), "it gave up a minute ago");
    assert!(ended(&resting(floor + 1)));
    // A record with no streak whose `failed_at` is ahead has not stopped yet.
    assert!(!ended(&St {
        failed_at: now + 3_600,
        ..resting(0)
    }));
    // A repeated stop's longer rest counts from the stop: `failed_at` sits the ladder past
    // it, and the end of the agent's work asks at once.
    let declined = |stopped_ago: u64, streak: u32| {
        let mut st = St {
            phase: Phase::Failed(upgrade::GAVE_UP.to_string()),
            streak_why: upgrade::GAVE_UP.to_string(),
            stop_streak: streak,
            work_seen: true,
            ..St::default()
        };
        st.failed_at = (now - stopped_ago) + st.rest_extension();
        st
    };
    assert!(!ended(&declined(60, 3)), "inside the floor after the stop");
    assert!(ended(&declined(3_600, 2)), "an hour after a streak-2 stop");
    assert!(
        ended(&declined(3 * 3_600, 3)),
        "three hours after a streak-3 stop"
    );
    assert!(
        ended(&declined(10 * 3_600, 4)),
        "ten hours into a day's rest"
    );
    // The day's budget: six, then none until the window is a day old.
    let mut st = announced(floor + 60, 2);
    for spent in 0..upgrade::WORK_END_DAILY {
        assert!(ended(&st), "ask {} is within the budget", spent + 1);
        st.spend_work_end(now);
    }
    assert!(!ended(&st), "the budget is spent");
    assert_eq!(st.work_end_left(now), 0);
    assert_eq!(st.work_end_left(now + upgrade::WORK_END_WINDOW_S - 1), 0);
    assert_eq!(
        st.work_end_left(now + upgrade::WORK_END_WINDOW_S),
        upgrade::WORK_END_DAILY,
        "a day on the budget is whole again"
    );
    // A rolling window: the asks age out one at a time.
    let mut rolling = St::default();
    for k in 0..u64::from(upgrade::WORK_END_DAILY) {
        rolling.spend_work_end(now + k * 3_600);
    }
    assert_eq!(rolling.work_end_left(now + 6 * 3_600), 0);
    assert_eq!(
        rolling.work_end_left(now + upgrade::WORK_END_WINDOW_S),
        1,
        "the first ask is a day old, the rest are not"
    );
    // The record keeps it across a save (a restart of the host).
    let again = St::from_json(&st.to_json()).expect("record");
    assert_eq!(again.work_end_at, st.work_end_at);
}

/// The seconds at which an agent that declines every notice is asked over `hours`, by the
/// real reducer and record transitions: after each notice a job runs under it for `job_s`
/// seconds, then ends. Looks are a minute apart.
fn declined_notices(hours: u64, job_s: u64) -> Vec<u64> {
    let t0 = 1_790_000_000;
    let to = Version::parse("2.1.284").expect("v");
    let mut st = St {
        phase: Phase::Pending,
        from: "2.1.283".to_string(),
        to: to.to_string(),
        pending_since: t0,
        salt: t0,
        ..St::default()
    };
    let mut notices: Vec<u64> = Vec::new();
    let mut last_notice = 0;
    for minute in 0..hours * 60 {
        let now = t0 + minute * 60;
        let mut facts = cadence_facts();
        if last_notice != 0 && now < last_notice + job_s {
            facts.background = vec!["zsh".to_string()];
        }
        st.saw_work(&facts);
        st.time_failed(&mut facts, now);
        st.time_work(&mut facts, now);
        match upgrade::next_step(&st.phase, &facts, false, now) {
            Step::Announce => {
                let asks = upgrade::announce_asks(&st.phase, false);
                if facts.work_ended {
                    st.spend_work_end(now);
                }
                let marker =
                    upgrade::ready_marker(SESSION, &to, st.salt.wrapping_add(u64::from(asks)));
                st.announced(marker, now, asks);
                // The visit that typed it sees the agent's work start.
                last_notice = now;
                notices.push(now);
            }
            Step::GiveUp => st.give_up(now),
            Step::Rearm => {
                st.rearm(now);
            }
            _ => {}
        }
    }
    notices
}

/// A session that keeps starting and ending work is asked a bounded number of times a
/// day: the work-end ask is exempt from the ask cap, so in any 24 h the notices stay within
/// three rounds of [`upgrade::MAX_ASKS`] plus the day's work-end budget (one round's worth
/// from the third day). Negative control: without the budget it is asked in every gap.
#[test]
fn a_declining_agent_is_asked_a_bounded_number_of_times_a_day() {
    let day = 86_400;
    let rolling = |notices: &[u64], from: u64| {
        notices
            .iter()
            .filter(|&&t| t >= from)
            .map(|&first| {
                notices
                    .iter()
                    .filter(|&&t| t >= first && t < first + day)
                    .count()
            })
            .max()
            .unwrap_or(0)
    };
    let rounds = usize::try_from(upgrade::MAX_ASKS).expect("small");
    let budget = usize::try_from(upgrade::WORK_END_DAILY).expect("small");
    let first_day = 3 * rounds + budget + 1;
    let steady = rounds + budget + 1;
    for job_s in [120, 300, 660, 1_500] {
        let notices = declined_notices(120, job_s);
        let t0 = notices[0];
        assert!(notices.len() >= 8, "the round does ask: {job_s}");
        let busiest = rolling(&notices, 0);
        assert!(
            busiest <= first_day,
            "a job of {job_s} s: {busiest} notices in one day, more than {first_day}: {notices:?}"
        );
        let later = rolling(&notices, t0 + 3 * day);
        assert!(
            later <= steady,
            "a job of {job_s} s, from the fourth day: {later} in one day, more than {steady}"
        );
    }
}

/// The ask at the end of the agent's own work needs nothing under the agent and Claude's
/// own status idle: of one announced round's four (watcher, status) points, only the
/// watcher gone with status idle is asked (a busy status over an idle screen waits
/// `status-stale`).
#[cfg(unix)]
#[test]
fn tab_ones_recorded_shape_is_not_asked_until_nothing_runs_and_claude_idles() {
    let marker = "ATERM-UPGRADE-READY-00000001";
    let cases = [
        // (watcher under the agent, Claude's status, the step)
        (true, "busy", "wait:awaiting-ready"),
        (false, "busy", "wait:status-stale"),
        (true, "idle", "wait:awaiting-ready"),
        (false, "idle", "announced:3"),
    ];
    for (n, (watcher, status, want)) in cases.into_iter().enumerate() {
        let mut h = Parked::new(&format!("t1s{n}"), Answers::default(), settled(), 3600);
        set_status(&mut h, status);
        let st = St {
            phase: Phase::Announced {
                at_s: now_s() - 900,
                asks: 2,
            },
            noticed_at: now_s() - 900,
            marker: marker.to_string(),
            markers: vec![marker.to_string()],
            notice_pid: h.sf.pid,
            notice_start: squash(&h.sf.proc_start),
            work_seen: true,
            ..settled()
        };
        std::fs::write(state_path(&h.opts, SESSION), st.to_json()).expect("state");
        let table = if watcher {
            shells_under(&h)
        } else {
            Vec::new()
        };
        let word = h.visit_with(dead_pid(), &table).step;
        assert_eq!(word, want, "watcher: {watcher}, status: {status}");
        assert_eq!(
            h.typed().len(),
            usize::from(want.starts_with("announced")),
            "watcher: {watcher}, status: {status}"
        );
    }
}

// ---------------------------------------------------------------- the READY at its turn end

/// An idle Claude Code screen whose last answer is `said`, one row a line
/// (the first under `⏺`, the rest at the message's column), its empty
/// composer holding the caret, read at `seq`.
#[cfg(unix)]
fn said_screen(said: &[&str], seq: u64) -> String {
    let rule = "─".repeat(40);
    let mut rows: Vec<String> = said
        .iter()
        .enumerate()
        .map(|(i, line)| match (i, line.is_empty()) {
            (0, _) => format!("⏺ {line}"),
            (_, true) => String::new(),
            (_, false) => format!("  {line}"),
        })
        .collect();
    rows.extend([String::new(), rule.clone()]);
    let caret = rows.len();
    rows.extend(["❯ ".to_string(), rule]);
    let rows = rows
        .iter()
        .map(|r| aterm_json::to_string(&Value::from(r.as_str())).expect("row"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"rows":[{rows}],"cursor":{{"row":{caret},"col":2}},"seq":{seq},"human_ms":null,"first":0}}"#
    )
}

/// A READY answer on the idle screen before Claude Code appends its transcript row (a
/// batched append, `FLUSH_INTERVAL_MS=100`), through the real visit: held
/// `wait:ready-unrecorded`, then heard and settled; a message taken up since is re-asked
/// as soon as its turn settles. The agent is never signalled.
#[cfg(unix)]
#[test]
fn a_ready_answer_is_heard_at_its_turn_end_and_a_superseded_one_is_asked_again() {
    let marker = "ATERM-UPGRADE-READY-0badf00d";
    let answer = [
        "Step 2 increment 2 passed, and it's merged and pushed.",
        "",
        "Result. The codegen path now keeps THIR alive: 5 of 6 functions.",
        "",
        "Nothing of mine is running now, so this is a good point for the upgrade:",
        "",
        marker,
    ];
    let p = Parked::new(
        "ready-at-turn-end",
        Answers {
            screen: said_screen(&answer, 78),
            ..Answers::default()
        },
        St::default(),
        60,
    );
    let screen_file = p.dir.join("t.screen");
    let grace = p.opts.human_grace_s;
    let now = now_s();
    let st = St {
        phase: Phase::Announced {
            at_s: now - 945,
            asks: 1,
        },
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        marker: marker.to_string(),
        markers: vec![marker.to_string()],
        asked: vec![marker.to_string()],
        tab: TAB.to_string(),
        notice_pid: p.sf.pid,
        notice_start: squash(&p.sf.proc_start),
        // The screen first seen now: its settle has not run.
        last_seq: 78,
        seq_since_s: now,
        ..St::default()
    };
    std::fs::write(state_path(&p.opts, SESSION), st.to_json()).expect("state");
    let json = |text: &str| aterm_json::to_string(&Value::from(text)).expect("json");
    let typed = |text: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},"origin":{{"kind":"human"}},"promptSource":"typed","version":"2.1.284"}}"#,
            json(text)
        )
    };
    let model = |text: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"assistant","message":{{"model":"claude-opus-5-5","role":"assistant","content":[{{"type":"text","text":{}}}],"stop_reason":"end_turn"}},"version":"2.1.284"}}"#,
            json(text)
        )
    };
    let notice = upgrade::prepare_prompt(
        &Version::parse("1.0.0").expect("from"),
        &Version::parse("9.9.9").expect("to"),
        Source::Managed,
        marker,
    );
    // The transcript as the look reads it: the task, the notice wrapped as a
    // paste, the agent's "not yet", the READY row not yet appended.
    write_transcript(
        &p.opts.home,
        &[
            ASKED.to_string(),
            typed(&format!(
                "\n\n<pasted_content id=\"6dd8\">\n{notice}\n</pasted_content id=\"6dd8\">\n"
            )),
            model("Not yet: my build still runs. I will answer once it is done."),
        ],
    );
    let append = |rows: &[String]| {
        let path = transcript(&p.opts.home, SESSION).expect("transcript");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .expect("append");
        for row in rows {
            writeln!(f, "{row}").expect("row");
        }
    };

    // Negative control: an idle point whose last words are no READY waits the
    // notice's window, owning nothing — the loop may continue the agent.
    std::fs::write(&screen_file, said_screen(&["Still building."], 77)).expect("screen");
    let control = p.visit();
    assert_eq!(control.step, "wait:awaiting-ready");
    assert!(!owns_turn_ends(&control.step, 0, grace));
    std::fs::remove_file(&screen_file).expect("screen");

    // (1) The READY on the screen, its row not appended yet: the point held.
    let shown = p.visit();
    assert_eq!(shown.step, "wait:ready-unrecorded", "{shown:?}");
    assert!(
        owns_turn_ends(&shown.step, 0, grace),
        "nothing typed over it"
    );
    assert!(
        !owns_turn_ends(&shown.step, OWNED_SETTLE_LOOKS, grace),
        "bounded"
    );
    assert_eq!(
        load(&p.opts, SESSION).expect("state").wait,
        "ready-unrecorded"
    );

    // (2) Claude Code appends the row: heard, the restart's settle holds the
    // point, and the answer's clock runs.
    append(&[model(&answer.join("\n"))]);
    let heard = p.visit();
    assert_eq!(heard.step, "wait:settling", "{heard:?}");
    assert!(owns_turn_ends(&heard.step, 0, grace));
    assert_ne!(load(&p.opts, SESSION).expect("state").ready_since, 0);

    // (3) A message taken up since that look: the re-ask waits `superseded` while the
    // turn settles, owning it.
    append(&[
        typed("keep going"),
        model("Two pieces of work are running in the background."),
    ]);
    std::fs::write(
        &screen_file,
        said_screen(&["Two pieces of work are running in the background."], 79),
    )
    .expect("screen");
    let moved_on = p.visit();
    assert_eq!(moved_on.step, "wait:superseded", "{moved_on:?}");
    assert!(owns_turn_ends(&moved_on.step, 0, grace));
    assert_eq!(load(&p.opts, SESSION).expect("state").wait, "superseded");
    assert_eq!(turns(&p.asked), 0, "nothing typed yet");

    // (4) The turn settled (a minute quiet): the notice again, at once — a
    // new ask with a new marker, well inside the old one's window.
    let mut st = load(&p.opts, SESSION).expect("state");
    st.seq_since_s = now_s() - 60;
    std::fs::write(state_path(&p.opts, SESSION), st.to_json()).expect("state");
    let asked_again = p.visit();
    assert_eq!(asked_again.step, "announced:2", "{asked_again:?}");
    assert_eq!(turns(&p.asked), 1);
    let st = load(&p.opts, SESSION).expect("state");
    assert!(
        matches!(st.phase, Phase::Announced { asks: 2, .. }),
        "{st:?}"
    );
    assert_ne!(st.marker, marker, "a marker of its own");
    assert!(alive(p.sf.pid), "the agent was never signalled");
}

/// [`named_standing_wall`] reads the quota wall over the same bounded tail as
/// [`named_limit_reset`] ([`TAIL_BYTES`]); a wall past the tail is not read.
/// Negative controls: another conversation and a home with no transcript have none.
#[cfg(unix)]
#[test]
fn the_standing_wall_is_read_from_the_named_transcripts_bounded_tail() {
    let dir = scratch("standing-wall");
    let home = dir.join("home");
    let wall = r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-30T19:02:11.123Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit · resets Oct 1 at 9am (America/Los_Angeles)"}]},"quotaLimits":{"status":"rejected","resetsAt":1790870400,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false},"error":"rate_limit","isApiErrorMessage":true}"#.to_string();
    let read = upgrade::StandingWall {
        at: 1_790_794_931,
        until: Some(1_790_870_400),
        window: Some("seven_day".to_string()),
        credits: Some(upgrade::CreditsWord::Off {
            reason: Some("org_level_disabled".to_string()),
            until: None,
        }),
    };
    // A megabyte of a person's rows before it: only the tail is read, and
    // the wall is in it.
    let filler = format!(
        r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":"{}"}}}}"#,
        "x".repeat(4000)
    );
    let mut rows = vec![filler.clone(); 256];
    rows.push(wall.clone());
    write_transcript(&home, &rows);
    assert_eq!(
        named_standing_wall(&home, &users_agent(SESSION)),
        Some(read)
    );
    assert_eq!(
        named_limit_reset(&home, &users_agent(SESSION)),
        Some(1_790_870_400),
        "the same row"
    );
    // Negative controls.
    assert_eq!(
        named_standing_wall(&home, &users_agent("00000000-0000-4000-8000-000000000000")),
        None
    );
    assert_eq!(
        named_standing_wall(&dir.join("nowhere"), &users_agent(SESSION)),
        None
    );
    // The bound: a wall followed by more than a tail of rows is not read.
    let mut rows = vec![wall];
    rows.extend(vec![filler; 80]);
    assert!(rows[1..].iter().map(String::len).sum::<usize>() as u64 > TAIL_BYTES);
    write_transcript(&home, &rows);
    assert_eq!(named_standing_wall(&home, &users_agent(SESSION)), None);
}

/// [`named_reset_line`]: the line the usage-limit dialog ended with, over the same bounded
/// tail. The reset row is composed from the bundle's text, not captured.
/// Negative controls: asked since a later second, another conversation, no transcript.
#[cfg(unix)]
#[test]
fn the_reset_line_is_read_from_the_named_transcripts_bounded_tail() {
    let dir = scratch("reset-line");
    let home = dir.join("home");
    // 2026-09-30T19:02:11Z.
    let claimed = 1_790_794_931;
    let said = "Reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    let row = |content: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"user","timestamp":"2026-09-30T19:02:40.500Z","message":{{"role":"user","content":"{content}"}}}}"#
        )
    };
    let filler = format!(
        r#"{{"isSidechain":false,"type":"user","message":{{"role":"user","content":"{}"}}}}"#,
        "x".repeat(4000)
    );
    let mut rows = vec![filler.clone(); 256];
    rows.push(row("<command-name>/rate-limit-options</command-name>"));
    rows.push(row(&format!(
        "<local-command-stdout>{said}</local-command-stdout>"
    )));
    write_transcript(&home, &rows);
    assert_eq!(
        named_reset_line(&home, &users_agent(SESSION), claimed).as_deref(),
        Some(said)
    );
    // Negative controls.
    assert_eq!(
        named_reset_line(&home, &users_agent(SESSION), claimed + 60),
        None
    );
    assert_eq!(
        named_reset_line(
            &home,
            &users_agent("00000000-0000-4000-8000-000000000000"),
            claimed
        ),
        None
    );
    assert_eq!(
        named_reset_line(&dir.join("nowhere"), &users_agent(SESSION), claimed),
        None
    );
}

/// The window host's limit and wall readers ([`named_limit_reset`], [`named_standing_wall`],
/// [`named_reset_line`]) read the transcript under the agent's own Claude home
/// ([`Records::of_snapshot`]): absolute, relative, or set and empty.
#[cfg(unix)]
#[test]
fn the_window_hosts_limit_and_wall_readers_read_the_agents_own_claude_home() {
    let dir = scratch("named-own-home");
    let user_home = dir.join("home");
    // The user's own home keeps projects too, none of them this conversation's.
    std::fs::create_dir_all(user_home.join(".claude/projects/p")).expect("the user's projects");
    // 2026-09-30T19:02:11Z.
    let claimed = 1_790_794_931;
    let said = "Reset kept · run /limit-reset at a limit to use it · use by Oct 5 at 3pm";
    let wall = r#"{"isSidechain":false,"type":"assistant","timestamp":"2026-09-30T19:02:11.123Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit · resets Oct 1 at 9am (America/Los_Angeles)"}]},"quotaLimits":{"status":"rejected","resetsAt":1790870400,"unifiedRateLimitFallbackAvailable":false,"rateLimitType":"seven_day","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false},"error":"rate_limit","isApiErrorMessage":true}"#;
    let row = |content: &str| {
        format!(
            r#"{{"isSidechain":false,"type":"user","timestamp":"2026-09-30T19:02:40.500Z","message":{{"role":"user","content":"{content}"}}}}"#
        )
    };
    let rows = [
        wall.to_string(),
        row("<command-name>/rate-limit-options</command-name>"),
        row(&format!(
            "<local-command-stdout>{said}</local-command-stdout>"
        )),
    ];
    let file = |claude_dir: &Path| {
        let project = claude_dir.join("projects/p");
        std::fs::create_dir_all(&project).expect("its project");
        std::fs::write(
            project.join(format!("{SESSION}.jsonl")),
            rows.join("\n") + "\n",
        )
        .expect("its transcript");
    };
    // The agent as the window host keeps it: its relaunch snapshot.
    let work = dir.join("work");
    let snap = |home: Option<&str>, claude_dir: Option<PathBuf>| Snapshot {
        stored_login: false,
        tab: TAB.to_string(),
        pid: 4242,
        start: "Thu Sep 24 01:02:03 2026".to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(SESSION.to_string()),
        cwd: work.to_string_lossy().into_owned(),
        version: None,
        codex: None,
        dialect: None,
        login: None,
        home: home.map(str::to_string),
        claude_dir,
    };
    let read = |snap: &Snapshot| {
        (
            named_limit_reset(&user_home, snap),
            named_standing_wall(&user_home, snap).map(|w| w.until),
            named_reset_line(&user_home, snap, claimed),
        )
    };
    let named = (
        Some(1_790_870_400),
        Some(Some(1_790_870_400)),
        Some(said.to_string()),
    );
    // Under its own home: absolute, where its snapshot read its record.
    let own = dir.join("work home/.claude");
    file(&own);
    let absolute = own.to_string_lossy().into_owned();
    assert_eq!(
        read(&snap(Some(&absolute), Some(own.clone()))),
        named,
        "its own absolute home"
    );
    // Relative, and set and empty: under the directory it runs in (a snapshot from a
    // reopened layout, which names no records directory).
    let relative = work.join("rel/.claude");
    file(&relative);
    assert_eq!(
        read(&snap(Some("rel/.claude"), None)),
        named,
        "a relative home"
    );
    file(&work);
    assert_eq!(read(&snap(Some(""), None)), named, "set and empty");
    // Controls: an agent under the user's home is read there; a relative home whose
    // directory nobody knows, or no conversation, names nothing.
    assert_eq!(
        read(&snap(None, None)),
        (None, None, None),
        "not the user's"
    );
    file(&user_home.join(".claude"));
    assert_eq!(read(&snap(None, None)), named, "the user's own");
    let unknown = Snapshot {
        cwd: String::new(),
        ..snap(Some("nowhere/.claude"), None)
    };
    assert_eq!(read(&unknown), (None, None, None), "no directory known");
    let unnamed = Snapshot {
        session: None,
        ..snap(Some(&absolute), Some(own))
    };
    assert_eq!(read(&unnamed), (None, None, None), "no conversation");
}

/// [`super::super::relaunch::account_restart`] names the account move whose restart
/// signalled this process in this tab, also shortly after it stopped. Negative controls:
/// other causes, another process or tab, no record, and a stop long past its exit.
#[test]
fn the_account_move_that_signalled_an_agent_is_named_by_its_label() {
    use super::super::relaunch::{account_restart, restarted};
    let a = DeadAgent::new("account-restart");
    let opts = Opts {
        dry_run: false,
        ..a.opts.clone()
    };
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    let pid = a.snap.pid;
    let now = now_s();
    let record = |cause: &str, phase: Phase, exited_at: u64| St {
        phase,
        pid,
        shell: a.snap.shell,
        tab: TAB.to_string(),
        from: "2.1.287".to_string(),
        to: "2.1.287".to_string(),
        source: "same".to_string(),
        cause: cause.to_string(),
        exited_at,
        ..St::default()
    };
    assert_eq!(account_restart(&opts, pid), None, "no record at all");
    let exiting = Phase::Exiting { at_s: now };
    save(
        &opts,
        SESSION,
        &record("account:work-2", exiting.clone(), 0),
    );
    assert_eq!(account_restart(&opts, pid).as_deref(), Some("work-2"));
    // The line typed, the new process not yet the loop's: still in flight.
    save(
        &opts,
        SESSION,
        &record("account:work-2", Phase::Relaunched { at_s: now }, now),
    );
    assert_eq!(account_restart(&opts, pid).as_deref(), Some("work-2"));
    // Stopped just after its agent was seen gone: still that exit's.
    let stopped = Phase::Failed("no-resume".to_string());
    save(
        &opts,
        SESSION,
        &record("account:work-2", stopped.clone(), now),
    );
    assert_eq!(account_restart(&opts, pid).as_deref(), Some("work-2"));

    // Negative controls.
    for cause in ["", "memory", "model:claude-sonnet-5-5"] {
        save(&opts, SESSION, &record(cause, exiting.clone(), 0));
        assert!(
            restarted(&opts, upgrade::Agent::Claude, Some(pid)),
            "{cause:?}: the harness did signal it"
        );
        assert_eq!(
            account_restart(&opts, pid),
            None,
            "{cause:?}: for no account move"
        );
    }
    save(&opts, SESSION, &record("account:work-2", exiting, 0));
    assert_eq!(account_restart(&opts, dead_pid()), None, "another process");
    let other_tab = Opts {
        only_sid: Some("s-0ther".to_string()),
        ..opts.clone()
    };
    assert_eq!(account_restart(&other_tab, pid), None, "another tab");
    save(
        &opts,
        SESSION,
        &record("account:work-2", stopped, now - 2 * STALE_S - 1),
    );
    assert_eq!(
        account_restart(&opts, pid),
        None,
        "a stop long past its exit is no longer this exit's"
    );
}

/// [`conversation_under`]: a conversation is under the home whose `projects/` holds its
/// transcript. Negative controls: another conversation, no projects, a name that is no
/// session id (path traversal included), and a directory of the transcript's name.
#[cfg(unix)]
#[test]
fn a_conversation_is_under_the_home_its_transcript_is_filed_in() {
    let dir = scratch("conversation-under");
    let home = dir.join("home");
    write_transcript(&home, &["{}".to_string()]);
    assert!(conversation_under(&home, SESSION));
    assert!(!conversation_under(
        &home,
        "00000000-0000-4000-8000-000000000000"
    ));
    let other = dir.join("other-home");
    assert!(!conversation_under(&other, SESSION), "no such home");
    std::fs::create_dir_all(other.join(".claude/projects")).expect("projects");
    assert!(!conversation_under(&other, SESSION), "no project holds it");
    // No session id: never joined into a path.
    std::fs::write(home.join(".claude/projects/outside.jsonl"), "{}\n").expect("a file");
    for name in ["", "../outside", "p/../../outside", "not a session"] {
        assert!(!conversation_under(&home, name), "{name:?}");
    }
    // A directory of that name is no transcript.
    let dir_named = "0badf00d-1111-2222-3333-44445555aaaa";
    std::fs::create_dir_all(home.join(format!(".claude/projects/p/{dir_named}.jsonl")))
        .expect("a directory");
    assert!(!conversation_under(&home, dir_named));
}

/// [`process_transcript_tail`] finds a process's conversation under its own
/// `CLAUDE_CONFIG_DIR`, through that directory's record of its pid, its project's
/// transcript first. Negative controls: a reused pid, an id filed twice with its own place
/// empty, no transcript and no record name nothing.
#[test]
fn the_transcript_that_confirms_a_wall_is_found_from_the_process() {
    let root = ScratchDir::new("upgrade", "wall-tail");
    let home = root.join("home");
    let alt = root.join("claude-alt");
    let start = "Thu Oct 8 18:04:23 2026";
    let cwd = "/private/tmp/at.0000/proj";
    let slug = "-private-tmp-at-0000-proj";
    let record = |dir: &Path, id: &str, proc_start: &str| {
        std::fs::create_dir_all(dir.join("sessions")).expect("sessions");
        std::fs::write(
            dir.join("sessions/4242.json"),
            format!(
                r#"{{"pid":4242,"sessionId":"{id}","cwd":"{cwd}","procStart":"{proc_start}","version":"2.1.294","kind":"interactive","entrypoint":"cli","status":"idle"}}"#
            ),
        )
        .expect("record");
    };
    let transcript = |dir: &Path, project: &str, id: &str, text: &str| {
        let at = dir.join("projects").join(project);
        std::fs::create_dir_all(&at).expect("project");
        std::fs::write(at.join(format!("{id}.jsonl")), text).expect("transcript");
    };
    let alt_id = "da373640-37c2-443b-b001-4b6cab95a6cf";
    let home_id = "80fb925f-a7d3-401b-9031-8241ff9919ab";
    record(&alt, alt_id, start);
    transcript(&alt, slug, alt_id, "{\"the\":\"alt tail\"}\n");
    record(&home.join(".claude"), home_id, start);
    transcript(
        &home.join(".claude"),
        slug,
        home_id,
        "{\"the\":\"home tail\"}\n",
    );
    let tail = |config: Option<&Path>, at: &str| {
        process_transcript_tail(config.and_then(Path::to_str), home.to_str(), None, 4242, at)
    };
    assert_eq!(
        tail(Some(&alt), start).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "the process's own CLAUDE_CONFIG_DIR"
    );
    assert_eq!(
        tail(None, start).as_deref(),
        Some("{\"the\":\"home tail\"}\n"),
        "no config directory: the home's .claude"
    );
    // A reused pid: its record's procStart is another process's.
    assert_eq!(tail(Some(&alt), "Thu Oct 8 18:04:24 2026"), None);
    assert_eq!(tail(Some(&alt), "not a start"), None);
    // Two transcripts of the id: the project's own place settles it.
    transcript(&alt, "-elsewhere", alt_id, "{\"the\":\"planted\"}\n");
    assert_eq!(
        tail(Some(&alt), start).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "the project's own transcript, not the planted one"
    );
    // The project's own place empty: two elsewhere are none, one is read.
    std::fs::remove_file(
        alt.join("projects")
            .join(slug)
            .join(format!("{alt_id}.jsonl")),
    )
    .expect("its own place emptied");
    transcript(&alt, "-moved", alt_id, "{\"the\":\"moved\"}\n");
    assert_eq!(
        tail(Some(&alt), start),
        None,
        "two, and none in its own place"
    );
    std::fs::remove_file(
        alt.join("projects/-elsewhere")
            .join(format!("{alt_id}.jsonl")),
    )
    .expect("planted");
    assert_eq!(
        tail(Some(&alt), start).as_deref(),
        Some("{\"the\":\"moved\"}\n"),
        "one elsewhere, read"
    );
    std::fs::remove_file(alt.join("projects/-moved").join(format!("{alt_id}.jsonl")))
        .expect("moved");
    assert_eq!(tail(Some(&alt), start), None, "no transcript");
    std::fs::remove_file(alt.join("sessions/4242.json")).expect("record");
    assert_eq!(tail(Some(&alt), start), None, "no record");
}

/// [`snapshot_transcript_tail`] takes the config directory from the live process's exec
/// environment, never the home's `.claude`; a relative or set-and-empty one resolves from
/// the directory the process runs in ([`home_dir_at`]).
#[cfg(unix)]
#[test]
fn the_transcript_is_found_under_the_live_processs_own_config_dir() {
    let root = ScratchDir::new("upgrade", "wall-tail-live");
    let home = root.join("home");
    let alt = root.join("claude-alt");
    let start = "Thu Oct 8 18:04:23 2026";
    let cwd = "/private/tmp/at.0000/proj";
    let slug = "-private-tmp-at-0000-proj";
    let keep = |dir: &Path, pid: u32, id: &str, text: &str| {
        std::fs::create_dir_all(dir.join("sessions")).expect("sessions");
        std::fs::write(
            dir.join(format!("sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"{id}","cwd":"{cwd}","procStart":"{start}","version":"2.1.294","kind":"interactive","entrypoint":"cli","status":"idle"}}"#
            ),
        )
        .expect("record");
        let at = dir.join("projects").join(slug);
        std::fs::create_dir_all(&at).expect("project");
        std::fs::write(at.join(format!("{id}.jsonl")), text).expect("transcript");
    };
    let tail_of = |config: Option<&Path>, runs_in: Option<&Path>| {
        let mut c = parked();
        c.env("HOME", &home);
        if let Some(dir) = runs_in {
            c.current_dir(dir);
        }
        match config {
            Some(dir) => c.env("CLAUDE_CONFIG_DIR", dir),
            None => c.env_remove("CLAUDE_CONFIG_DIR"),
        };
        let mut agent = c.spawn().expect("the stand-in");
        let pid = agent.id();
        wait_exec(pid);
        keep(
            &alt,
            pid,
            "da373640-37c2-443b-b001-4b6cab95a6cf",
            "{\"the\":\"alt tail\"}\n",
        );
        keep(
            &home.join(".claude"),
            pid,
            "80fb925f-a7d3-401b-9031-8241ff9919ab",
            "{\"the\":\"home tail\"}\n",
        );
        let snap = Snapshot {
            stored_login: false,
            tab: TAB.to_string(),
            pid,
            start: start.to_string(),
            shell: 1,
            program: PathBuf::from("/usr/local/bin/claude"),
            argv: vec!["claude".to_string()],
            session: None,
            cwd: cwd.to_string(),
            version: None,
            codex: None,
            dialect: None,
            login: None,
            home: None,
            claude_dir: None,
        };
        let tail = snapshot_transcript_tail(&snap);
        let _ = agent.kill();
        let _ = agent.wait();
        tail
    };
    assert_eq!(
        tail_of(Some(&alt), None).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "the process's own CLAUDE_CONFIG_DIR"
    );
    assert_eq!(
        tail_of(None, None).as_deref(),
        Some("{\"the\":\"home tail\"}\n"),
        "none: its HOME's .claude"
    );
    assert_eq!(
        tail_of(Some(Path::new("claude-alt")), Some(&root)).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "a relative one, under the directory it runs in"
    );
    assert_eq!(
        tail_of(Some(Path::new("")), Some(&alt)).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "set and empty: the directory it runs in"
    );
}

/// The CLI's sweep judges an empty-home agent by its login's profile (`$HOME/.claude.json`,
/// [`profile_apart`]) also after a window recorded it: the record keeps the profile with the
/// home ([`St::home_profile`]), and a home read off a live leader comes before the record's.
#[cfg(unix)]
#[test]
fn the_clis_sweep_judges_an_empty_homes_agent_a_window_recorded_by_its_login() {
    const LEADER: u32 = 4_242;
    let dir = scratch("cli-empty-home-recorded");
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={LEADER}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    // The CLI's unscoped pass, its instance listing the agent's tab, and one
    // whose instance answers nothing (`drive`'s socket does not exist).
    let listed = Opts {
        sock: Some(sock),
        dry_run: true,
        ..drive(&dir)
    };
    let unlisted = Opts {
        dry_run: true,
        ..drive(&dir)
    };
    let work = dir.join("work");
    let both = r#"{"catalog":{"config":{"models":[{"id":"claude-opus-5-5","section":"current"},{"id":"claude-fable-5-1","section":"current"}]}}}"#;
    for claude_dir in [listed.home.join(".claude"), work.clone()] {
        let catalog = claude_dir.join("cache/model-catalog");
        std::fs::create_dir_all(&catalog).expect("the catalog cache");
        std::fs::write(catalog.join("0000-cc.json"), both).expect("served");
    }
    let targets = Targets {
        managed: None,
        native: None,
        unanswered: false,
    };
    let mctx = ModelCtx::read(&listed, &targets);
    std::fs::create_dir_all(work.join("sessions")).expect("sessions");
    std::fs::write(
        work.join(format!("sessions/{LEADER}.json")),
        format!(
            r#"{{"pid":{LEADER},"sessionId":"{SESSION}","cwd":"{}","version":"2.1.296","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#,
            work.display()
        ),
    )
    .expect("the record");
    let proj = work.join("projects/-work");
    std::fs::create_dir_all(&proj).expect("projects");
    let at = i64::try_from(now_s()).expect("now") - 7_200;
    std::fs::write(
        proj.join(format!("{SESSION}.jsonl")),
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-fable-5-1\"}}}}\n",
            crate::harness::usage::rfc3339_utc(at)
        ),
    )
    .expect("transcript");
    // Its login is denied Opus 5.5, in the profile Claude Code keeps for it.
    std::fs::write(
        listed.home.join(".claude.json"),
        r#"{"modelAccessCache":[{"apiName":"claude-opus-5-5","entitled":false}]}"#,
    )
    .expect("the login's profile");
    let args = |_: u32| {
        Some(atpkg::caller_shell::ProcArgs {
            env: vec!["CLAUDE_CONFIG_DIR=".to_string()],
            ..atpkg::caller_shell::ProcArgs::default()
        })
    };
    let record = |opts: &Opts, tabs: Option<&[LiveTab]>| {
        Homes::read(opts, tabs, args, |_| Some(work.clone()))
            .roster(Some(Vec::new()))
            .and_then(|files| files.into_iter().find(|sf| sf.pid == LEADER))
    };
    let verdict = |opts: &Opts| {
        record(opts, None).map(|sf| {
            let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
            model_read_for(opts, &sf, None, &mctx, false, None).verdict
        })
    };
    let keep = Some(ModelVerdict::Keep("model-current"));
    // Control: no record of a visit. The listed tab's leader names the home;
    // with no tab listed, nothing names it.
    assert_eq!(verdict(&listed), keep, "CONTROL: read from its tab");
    assert_eq!(verdict(&unlisted), None, "CONTROL: nothing names its home");
    // A record an older build wrote names the home's directory alone: the
    // listed tab's leader, read first, still names its profile.
    std::fs::create_dir_all(state_dir(&listed)).expect("state");
    let older = St {
        home: Records::at(&listed.home, HomeVars::config(Some("")), || {
            Some(work.clone())
        })
        .home_word(&listed.home),
        ..St::default()
    };
    std::fs::write(state_path(&listed, SESSION), older.to_json()).expect("the older record");
    assert_eq!(
        verdict(&listed),
        keep,
        "a home read off its live leader comes before a record's word"
    );
    std::fs::remove_file(state_path(&listed, SESSION)).expect("rm");
    // The window notes it behind, from the record its own pass read.
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(LEADER)),
    }];
    let sf = record(&listed, Some(&tabs)).expect("the window reads its record");
    assert!(
        mint_behind(
            &Opts {
                dry_run: false,
                ..listed.clone()
            },
            &sf,
            TAB,
            &newer(),
            false,
            now_s(),
        ),
        "noted behind"
    );
    let (after_listed, after_unlisted) = (verdict(&listed), verdict(&unlisted));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        (after_listed, after_unlisted),
        (keep.clone(), keep),
        "judged by its login's denial, from its tab and from the window's record alike"
    );
}

/// Under a relative home, a Claude Code that changed directory keeps its record where it
/// started but writes its conversation under the home its new directory names: the wall's
/// tail ([`process_transcript_tail`]) is read there, and nothing written there is no tail.
/// Control: an agent that never moved reads its own transcript.
#[cfg(unix)]
#[test]
fn the_walls_tail_is_the_transcript_its_agent_writes_now() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, moved: bool, written_now: Option<&str>| {
        let dir = scratch(name);
        let work = dir.join("work");
        std::fs::create_dir_all(work.join("wt")).expect("its directories");
        let started = std::fs::canonicalize(&work).expect("where it started");
        let runs_in = if moved {
            started.join("wt")
        } else {
            started.clone()
        };
        let claude_dir = started.join(RELATIVE);
        let mut agent = tty_agent(
            &[
                (crate::harness::accounts::HOME_ENV, Path::new(RELATIVE)),
                ("PWD", started.as_path()),
            ],
            Some(&runs_in),
        );
        let pid = agent.id();
        let start = kernel_start(pid).expect("lstart");
        register_running_in(&claude_dir, pid, &start, &started);
        // The conversation as it stood when it filed its record.
        file_transcript_in(&claude_dir, &crate::harness::footer::project_slug(&started));
        // What it writes now, under the home its directory now names.
        if let Some(text) = written_now {
            let now_dir = runs_in
                .join(RELATIVE)
                .join("projects")
                .join(crate::harness::footer::project_slug(&runs_in));
            std::fs::create_dir_all(&now_dir).expect("projects");
            std::fs::write(
                now_dir.join(format!("{SESSION}.jsonl")),
                format!(
                    "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}\n"
                ),
            )
            .expect("transcript");
        }
        let tail = snapshot_transcript_tail(&Snapshot {
            stored_login: false,
            tab: TAB.to_string(),
            pid,
            start,
            shell: 1,
            program: PathBuf::from("/opt/claude/bin/claude"),
            argv: vec!["claude".to_string()],
            session: None,
            cwd: "/".to_string(),
            version: None,
            codex: None,
            dialect: None,
            login: None,
            home: Some(RELATIVE.to_string()),
            claude_dir: None,
        });
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        tail
    };
    let unmoved = case("wall-tail-unmoved", false, None);
    let moved = case("wall-tail-moved", true, Some("WRITTEN-NOW limit notice"));
    let unwritten = case("wall-tail-moved-unwritten", true, None);
    assert!(
        unmoved
            .as_deref()
            .is_some_and(|t| t.contains("port the parser")),
        "CONTROL: an agent that never moved, its transcript's: {unmoved:?}"
    );
    assert!(
        moved
            .as_deref()
            .is_some_and(|t| t.contains("WRITTEN-NOW") && !t.contains("port the parser")),
        "the transcript it writes now: {moved:?}"
    );
    assert_eq!(
        unwritten, None,
        "nothing written where it writes now: no tail, never the one it left"
    );
}

/// A tab the CLI's sweep only learned homes from is not its to wait on: an unreadable
/// leader there is no `wait:no-process` row. Control: the window's own roster waits on it.
#[cfg(unix)]
#[test]
fn the_clis_sweep_waits_on_no_tab_it_only_learned_homes_from() {
    let dir = scratch("cli-sweep-unread-leader");
    // A live leader whose exec record is not read.
    let leader = std::process::id();
    let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={leader}");
    let (sock, _asked) = instance_with(
        &dir,
        Answers {
            who: Some(who),
            ..Answers::default()
        },
    );
    let opts = Opts {
        sock: Some(sock),
        dry_run: true,
        ..drive(&dir)
    };
    let tabs = [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(i64::from(leader)),
    }];
    let window = Homes::read(&opts, Some(&tabs), |_| None, |_| None).unnamed(TAB);
    let cli = Homes::read(&opts, None, |_| None, |_| None).unnamed(TAB);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(window, "CONTROL: the window's own tab waits on its leader");
    assert!(
        !cli,
        "the CLI's sweep reports no wait for a tab it only learned homes from"
    );
}

/// An agent under another Claude home is judged by a list grown from that home's
/// announcements ([`ModelCtx::at`]), leaving the pass's shared `models.json` and its journal
/// as they were. Control: the user's own agent, announced nothing, is not moved.
#[test]
fn another_homes_announcement_grows_only_its_own_agents_list() {
    let dir = scratch("model-home-grows-alone");
    let opts = Opts {
        dry_run: false,
        ..drive(&dir)
    };
    let bin = dir.join("build/claude");
    std::fs::create_dir_all(bin.parent().expect("dir")).expect("the build");
    std::fs::write(
        &bin,
        concat!(
            r#"VERSION:"2.1.300",models:[{id:"claude-opus-5",family:"opus",display_name:"Opus 5"},"#,
            r#"{id:"claude-opus-5-5",family:"opus",display_name:"Opus 5.5"},"#,
            r#"{id:"claude-opus-5-6",family:"opus",display_name:"Opus 5.6"},"#,
            r#"{id:"claude-fable-5-1",family:"fable",display_name:"Fable 5.1"}],"#,
            r#"latest_per_family:{fable:"claude-fable-5-1",opus:"claude-opus-5-5"},alias_migration:{}};"#,
        ),
    )
    .expect("the build's catalog");
    let twin = dir.join("agents/claude");
    std::fs::create_dir_all(twin.parent().expect("dir")).expect("the twin");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", bin.display()),
    )
    .expect("the twin");
    let targets = Targets {
        managed: Some(Candidate {
            exe: twin,
            version: Version::parse("2.1.300").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    };
    let answered = |claude_dir: &Path| {
        let proj = claude_dir.join("projects/-work");
        std::fs::create_dir_all(&proj).expect("projects");
        let at = i64::try_from(now_s()).expect("now") - 7_200;
        std::fs::write(
            proj.join(format!("{SESSION}.jsonl")),
            format!(
                "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"model\":\"claude-opus-5-5\"}}}}\n",
                crate::harness::usage::rfc3339_utc(at)
            ),
        )
        .expect("transcript");
    };
    let other = dir.join("work/.claude");
    std::fs::create_dir_all(&other).expect("its home");
    std::fs::write(
        other.join(".claude.json"),
        r#"{"cachedGrowthBookFeatures":{"tengu_startup_announcements":[{"id":"opus-5-6-launch","text":"t","requiresModel":"claude-opus-5-6"}]}}"#,
    )
    .expect("its profile");
    answered(&other);
    let users = opts.home.join(".claude");
    answered(&users);
    let verdict = |sf: &SessionFile, mctx: &ModelCtx| {
        let _ = std::fs::remove_dir_all(opts.state.join("upgrade/models"));
        model_read_for(&opts, sf, None, mctx, false, None).verdict
    };
    // The pass's own read, as it leaves the file and the journal.
    let mctx = ModelCtx::read(&opts, &targets);
    let path = state_dir(&opts).join("models.json");
    let list = std::fs::read_to_string(&path).expect("the list");
    let changed = ledger_details(&opts, "models-changed");
    let theirs = SessionFile {
        claude_dir: Some(other.clone()),
        ..record("2.1.300")
    };
    let second = SessionFile {
        pid: 102,
        ..theirs.clone()
    };
    let moved = ModelVerdict::Due {
        from: "claude-opus-5-5".to_string(),
        to: "claude-opus-5-6".to_string(),
    };
    let judged = [verdict(&theirs, &mctx), verdict(&second, &mctx)];
    let (list_after, changed_after) = (
        std::fs::read_to_string(&path).expect("the list"),
        ledger_details(&opts, "models-changed"),
    );
    let own = SessionFile {
        claude_dir: Some(users.clone()),
        ..record("2.1.300")
    };
    let next = verdict(&own, &ModelCtx::read(&opts, &targets));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        judged,
        [moved.clone(), moved],
        "each moved to the model its home announces"
    );
    assert_eq!(
        (list_after, changed_after),
        (list, changed),
        "the shared list and its journal as the pass's own read left them"
    );
    assert!(
        !matches!(next, ModelVerdict::Due { .. }),
        "CONTROL: the user's agent, announced nothing, is not moved: {next:?}"
    );
}

/// Under a relative home, a Claude Code that changed directory leaves its record (status
/// frozen) where it filed it: its look waits `wait:home-moved` and types nothing.
/// Control: the same agent, never moved, goes on to the gates.
#[cfg(unix)]
#[test]
fn an_agent_whose_records_a_worktree_split_waits_and_is_asked_nothing() {
    const RELATIVE: &str = "work home/.claude";
    let case = |name: &str, moved: bool| {
        let dir = scratch(name);
        let work = dir.join("work");
        std::fs::create_dir_all(work.join("wt")).expect("its directories");
        let started = std::fs::canonicalize(&work).expect("where it started");
        let runs_in = if moved {
            started.join("wt")
        } else {
            started.clone()
        };
        let claude_dir = started.join(RELATIVE);
        let mut agent = tty_agent(
            &[
                (crate::harness::accounts::HOME_ENV, Path::new(RELATIVE)),
                ("PWD", started.as_path()),
            ],
            Some(&runs_in),
        );
        let pid = agent.id();
        let who = format!("OK 1\n0 {TAB} driving=- watchers=0 turns=0 alive fgpgid={pid}");
        let (sock, asked) = instance_with(
            &dir,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        // The ask flow, its window's step for the tab.
        let opts = Opts {
            sock: Some(sock),
            only_sid: Some(TAB.to_string()),
            ..drive(&dir)
        };
        assert!(opts.ask, "the ask flow");
        let sf = register_as(&claude_dir, pid, SESSION, "1.0.0");
        file_transcript_in(&claude_dir, "-work");
        let shell = dead_pid();
        let r = visit(
            &opts,
            &sf,
            &[(shell, 1, "zsh".to_string())],
            &newer(),
            &Script::new(shell, usize::MAX, None),
        );
        let typed: Vec<String> = asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
            .unwrap_or_default();
        let _ = agent.kill();
        let _ = agent.wait();
        let _ = std::fs::remove_dir_all(&dir);
        (r.step, typed)
    };
    let (moved, typed) = case("split-records-moved", true);
    let (unmoved, _) = case("split-records-unmoved", false);
    assert_eq!(
        (moved.as_str(), typed),
        ("wait:home-moved", Vec::<String>::new()),
        "its look waits by name, typing nothing"
    );
    assert_ne!(unmoved, "wait:home-moved", "CONTROL: never moved");
}

/// With no roster of its own, the CLI's sweep learns Claude homes from the `who` roster
/// of every instance [`Opts::instances`] names. Control: an instance not given is not read.
#[cfg(unix)]
#[test]
fn the_clis_sweep_reads_the_tabs_of_every_instance_it_is_given() {
    const OTHER_TAB: &str = "s-0123456789abcdef0123";
    let dir = scratch("cli-sweep-two-instances");
    // Two instances, one tab each, each tab led by an agent under its own home.
    let mut socks = Vec::new();
    let mut instances = Vec::new();
    for (name, tab, leader) in [("a", TAB, 4_242_u32), ("b", OTHER_TAB, 4_343)] {
        let at = dir.join(name);
        std::fs::create_dir_all(&at).expect("the instance's directory");
        let who = format!("OK 1\n0 {tab} driving=- watchers=0 turns=0 alive fgpgid={leader}");
        let (sock, asked) = instance_for(
            &at,
            tab,
            Answers {
                who: Some(who),
                ..Answers::default()
            },
        );
        socks.push(sock);
        // Both servers must outlive the roster reads after this loop.
        instances.push(asked);
        let home = dir.join(format!("{name}/.claude"));
        std::fs::create_dir_all(home.join("sessions")).expect("sessions");
        std::fs::write(
            home.join(format!("sessions/{leader}.json")),
            format!(
                r#"{{"pid":{leader},"sessionId":"{SESSION}","cwd":"/","version":"2.1.296","status":"idle","statusUpdatedAt":1,"procStart":"start","kind":"interactive","entrypoint":"cli"}}"#
            ),
        )
        .expect("the record");
    }
    let args = |pid: u32| {
        let name = if pid == 4_242 { "a" } else { "b" };
        Some(atpkg::caller_shell::ProcArgs {
            env: vec![format!(
                "CLAUDE_CONFIG_DIR={}",
                dir.join(format!("{name}/.claude")).display()
            )],
            ..atpkg::caller_shell::ProcArgs::default()
        })
    };
    // The CLI's unscoped pass: no `--sock`, the instances it is given.
    let read = |instances: Vec<String>| {
        let opts = Opts {
            sock: None,
            dry_run: true,
            instances: Instances::Listed(instances),
            ..drive(&dir)
        };
        let mut pids: Vec<u32> = Homes::read(&opts, None, args, |_| None)
            .roster(Some(Vec::new()))
            .expect("the records")
            .iter()
            .map(|sf| sf.pid)
            .collect();
        pids.sort_unstable();
        pids
    };
    let both = read(socks.clone());
    let one = read(socks[..1].to_vec());
    let none = read(Vec::new());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        both,
        [4_242, 4_343],
        "every instance's tabs teach their homes"
    );
    assert_eq!(one, [4_242], "CONTROL: the instance not given is not read");
    assert_eq!(none, Vec::<u32>::new(), "CONTROL: none given, none read");
}

/// THE MANAGED TARGET IS NEVER A DEV LINK'S: atpkg's twin of `claude` is the
/// build a session may be moved onto, except while `claude` is dev-linked
/// (`aterm pkg link`) and its twin names the checkout rather than a store
/// build. CONTROLS: an unlinked program's twin is managed whatever it holds
/// (a test's stand-in script included); a linked program's twin that still
/// names a store build is managed.
#[test]
fn a_dev_links_twin_is_no_managed_build() {
    let dir = scratch("managed-twin");
    let layout = atpkg::store::Layout {
        prefix: dir.to_path_buf(),
    };
    std::fs::create_dir_all(dir.join("agents")).expect("agents");
    let twin = dir.join("agents/claude");
    std::fs::write(&twin, "#!/bin/sh\necho stand-in\n").expect("stand-in");
    assert_eq!(
        managed_twin(&layout, "claude"),
        Some(twin.clone()),
        "CONTROL: not linked, the twin is managed"
    );
    let checkout = dir.join("claude-code");
    std::fs::create_dir_all(checkout.join("bin")).expect("checkout");
    std::fs::write(checkout.join("bin/claude"), "#!/bin/sh\n").expect("dev build");
    atpkg::linkmode::link(&layout, "claude", &checkout, &[PathBuf::from("bin/claude")])
        .expect("dev link");
    assert!(atpkg::linkmode::is_linked(&layout, "claude"));
    std::fs::write(
        &twin,
        format!(
            "#!/bin/sh\nexec '{}' \"$@\"\n",
            checkout.join("bin/claude").display()
        ),
    )
    .expect("dev twin");
    assert_eq!(managed_twin(&layout, "claude"), None, "a dev link's");
    let store = dir.join("store/claude/7/bin/claude");
    std::fs::write(
        &twin,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", store.display()),
    )
    .expect("twin");
    assert_eq!(
        managed_twin(&layout, "claude"),
        Some(twin),
        "CONTROL: linked, but the twin names a store build"
    );
}

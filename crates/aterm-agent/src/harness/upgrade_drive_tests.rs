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
// Every child these tests start is the product's kind: its stdio and nothing
// else of this process. A `std::process::Command` child under the seal's std
// holds every descriptor open here without close-on-exec at its spawn, and on
// Darwin that is any other test thread's pipe between its `pipe()` and its
// `F_SETFD`: a stand-in that lives (a bash in its `read`, a parked agent) held
// a `ps` child's stdout that way, and `ps()`'s read for its end waited as long
// as the stand-in lived — one test's stand-in, another test's wait.
use aterm_uds::spawnfd::Command;
use std::time::{Duration, Instant, UNIX_EPOCH};

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
    // A cursor ABOVE the reply's first row is no cursor, not row 0.
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

/// The busy footer the upgrade's gate waits on is the live zone's
/// (`aterm_phase::busy_zone_start`): a dead process's `esc to interrupt`
/// footer above a new launch card (after `/exit` and a fresh `claude` with no
/// `clear`) does not hold the gate at `busy`. The new session's own footer,
/// with the dead one above it, still counts; and a read too short to hold the
/// frame (a `tail=3` of a busy tab) is read whole.
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

/// The harness's own hand on a tab whose agent it restarts ([`Hand`]:
/// `hand=lease:aterm-harness@<this pid>`) holds nothing against itself (its
/// last looks, the relaunch line and its supervisor loops read on), while
/// another holder's lease, a named driver's turn, and input its program has
/// not read yet (a first prompt on its way, which a signal would drop; a
/// shell's queued lines) hold the tab. Negative controls: another process's
/// harness hand is someone else's, and `input=clear`/`-` holds nothing.
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

/// A Codex worker whose managed build is already current still has a
/// foreground Codex: Claude's session directory has no bearing on its answer.
/// The unknown/Claude arm is the negative control: it must keep the roster
/// read, including when a Codex process could not be identified reliably.
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

/// A build that did not answer decides nothing. The tab's Claude Code runs
/// 1.0.0, and the managed twin's `--version` timed out: the version cache
/// ([`cached_version_with`]) holds it unanswered until its back-off,
/// [`Targets`] carries that, and the look is [`UNREAD_VERSION`] (the host
/// looks again on its unread ladder), never "not due". Asked again before the
/// back-off, it says the same and probes nothing. Negative controls: once the
/// build answers, it decides (`No` for a session on it, `Yes` for one behind
/// it); and a record of another tab's agent is no record of this one's
/// ([`UNREAD_RECORD`]), whatever the cache holds.
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
            // The move's instant, kept through Done for the finished move's
            // record (audit 2026-10-09).
            relaunched_at: 1_790_000_090,
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
    // An upgrade's record an older build wrote, with no cause at all, reads
    // as the upgrade's.
    let old = r#"{"phase":"relaunched","at":7}"#;
    assert_eq!(St::from_json(old).map(|s| s.cause), Some(String::new()));
    // Nor any `relaunched_at` (review of 2026-10-09): a move in flight across
    // the window's update takes its phase's stamp as the move's instant, so
    // its finished record still says when it moved. NEGATIVE CONTROL: a Done
    // record of that build lost it with the phase and keeps none, and a
    // written `0` is read as written.
    assert_eq!(St::from_json(old).map(|s| s.relaunched_at), Some(7));
    let done = r#"{"phase":"done","at":0}"#;
    assert_eq!(St::from_json(done).map(|s| s.relaunched_at), Some(0));
    let written = r#"{"phase":"relaunched","at":7,"relaunched_at":0}"#;
    assert_eq!(St::from_json(written).map(|s| s.relaunched_at), Some(0));
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

/// Claude Code's own keep-awake is not work. Claude Code starts `caffeinate -i
/// -t 300` as its own child at every turn and stops it some 30 s after the turn
/// ends; counted as background work, it would hold every restart well past the
/// READY answer. The tree is one `ps` read at a READY answer (Claude 4163 under
/// the tab's zsh, its caffeinate 8931): nothing there is work, and the restart
/// goes.
///
/// Negative controls, each still holding the restart: a `caffeinate` a Bash
/// tool started (under the tool's shell); the python a Bash tool left running
/// under the agent beside Claude's own keep-awake (only the keep-awake is let
/// go, not every direct child); a shell Claude's keep-awake itself runs (the
/// walk goes on below it); and a caffeinate some helper of the agent's
/// started, which is not the agent's own.
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

/// The processes the notice names: two background poll loops under a Claude
/// Code, as `ps` prints them. The notice quotes the `eval` payload and drops
/// the Bash tool's boilerplate. Their pids come from the same walk as the
/// gate's.
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

    // The give-up's ledger row quotes the same list (a closer in the Bash
    // tool's wrapper above). Its `grep -m1` is what makes the `tail -f` a wait
    // that can never end, and the row keeps it past the 119-character path
    // ahead of it.
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
    // The row says the release is OWED, never that it was typed — the
    // `released:` row that follows says that. The Codex lane types none,
    // and its row promises none.
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

/// The restart's last look fails closed: a process table `ps` could not give
/// is empty, and over an empty table the walk answers "nothing runs", which
/// would let the irreversible act pass on no evidence. An empty table reads
/// as work.
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

/// The one-minute host tick should pay no `ps` or argv cost for a current
/// Claude, yet a newer native/managed build or an interrupted restart must
/// still reach the ownership proofs before any possible act.
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
    // CONTROL: this test's own live process holding the conversation.
    let me = std::process::id();
    let start = kernel_start(me).expect("our own start");
    assert!(steps(&[file(me, &start)]).is_empty(), "`visit` has it");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_restart_left_between_exit_and_relaunch_is_resumed_by_the_next_sweep() {
    let dir = scratch("orphan");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    // A pid that WAS a process and is not one now.
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

fn scratch(name: &str) -> ScratchDir {
    ScratchDir::new("upgrade", name)
}

/// THE HOME'S MODEL CACHES ARE NOT READ WHILE TWO OR MORE ACCOUNTS ARE LISTED
/// ([`Opts::several_logins`], [`model_evidence`]; the accounts design §9 row
/// 2): the home keeps one profile, and its served catalog, its access cache
/// and its announcements are whichever login started last — so under several
/// logins the evidence is the UNKNOWN, and a model that cache denies is
/// denied by nothing the upgrade reads. NEGATIVE CONTROL: with one login the
/// same files are read as ever — the denial, the catalog, the announcement.
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
    let read = model_evidence(&one);
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
        ..one.clone()
    };
    let unknown = model_evidence(&several);
    assert_eq!(
        unknown,
        models::Evidence::default(),
        "nothing of the home's"
    );
    // What the sweeps judge by is that evidence ([`models_report`] reads it
    // as every sweep does; what it says of the BUILD's own catalog depends
    // on the Claude Code this machine has installed, and is not asserted):
    // with one login a model's verdict is the cache's denial or the served
    // catalog's; under several, neither is read, so neither is said.
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
    // Released by `LOCK_UN` (the guard's drop), so the very next sweep runs. A
    // child another test thread spawns at this instant shares the lock's open
    // file description until its exec, so a lock released by close alone
    // could read held after the drop; `LOCK_UN` strips it from the description
    // itself, copies and all.
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

    // Negative control: an everyday launch IS announced, once, through the same
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

/// The drain's bound, end to end over the real state: an answered
/// announcement whose restart a box nobody answers has held past
/// [`upgrade::DRAIN_S`] is voided by the sweep's own arm. The READY answer
/// still in the transcript no longer counts ([`answered`], whose pre-void
/// answer is the negative control), the ledger says why, the phase keeps its
/// asks, and the reducer asks again at the next idle point instead of
/// restarting on the stale answer. A dry run says it and changes nothing.
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
    // The person clears the box: the stale answer does not end the agent, and
    // no notice follows on the heels of the void: the release goes first
    // ([`upgrade::gate_release`]), and the next notice `REASK_S` after the
    // void (the box, while it stands, is only a wait for either).
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

/// A sweep lock is free the moment its guard drops, whatever copies of its
/// descriptor live on. `copy` stands in for a child another thread is
/// mid-spawning, which holds every descriptor until its exec: a lock released
/// by close alone would stay held and the next sweep would read
/// `busy:another-sweep`. A regression of [`SweepLock`] to a bare `File` fails
/// the assertion after the release. Negative control first: a lock a sweep
/// holds refuses.
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

/// The stand-in agent's argv: THIS test binary, running only [`park_when_asked`].
const PARK: [&str; 2] = ["harness::upgrade_drive::tests::park_when_asked", "--exact"];

/// What tells [`park_when_asked`] to park.
const PARK_ENV: &str = "UPGRADE_DRIVE_TEST_PARK";

/// The stand-in agents' body: idle unless asked to park — then parked for as
/// long as the test process that started it lives (its parent changes when that
/// process ends), bounded only as a hang detector. A fixed 30 s park was a
/// latency budget in disguise: a test that ran past it on a loaded machine read
/// the agent's argv after it had exited (`wait:argv-unreadable`).
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
    let start = kernel_start(pid).expect("lstart");
    std::fs::create_dir_all(claude_dir.join("sessions")).expect("sessions");
    std::fs::write(
        claude_dir.join(format!("sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("session file");
    session_file_in(claude_dir, pid).expect("the file parses")
}

/// The complete roster was already read to find candidates. A quiet sweep
/// with multiple live conversations must not re-scan it once per candidate.
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

/// A notice whose process is gone is asked afresh of the live holder. Agent A
/// in tab `s-a` was announced to, answered READY, and is owed a release; B
/// holds the conversation (in A's tab, in another, and under A's pid
/// recycled). While A lives (holding another conversation now) B waits. Once
/// A is gone a dry run says it would reopen and changes nothing, and one visit
/// puts the upgrade back to pending in a new round (a fresh salt, A's markers
/// and fence forgotten, the release owed to A dropped, said once in the
/// ledger) and asks B afresh with a marker of its own. A's READY, still the
/// transcript's last word, never restarts B. Tier-1 of
/// `harness_upgrade_notice_owner_model` (`Reask`, `Terminate`).
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

        // THE VISIT: a new round, and B asked afresh in it.
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
        // AN AGE BELONGS TO ONE PROCESS: B was never asked and is behind since
        // it began, never since A's round (which ended with A).
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

/// A gone notice's row promises no ask the visit never makes: the upgrade is
/// reopened when the process its notice reached is gone, before the visit
/// knows whether the conversation still owes a move. Here the notified agent
/// A exited and the person resumed its conversation by hand on the current
/// build, so B is current and nothing is asked. The ledger says the upgrade
/// is pending again (`reopened:`), once, and that the holder is asked only as
/// a pending upgrade asks: while the conversation is still behind.
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

/// The record a gone notice leaves ([`St::notice_gone`]): pending in a new
/// round in the holder's tab (the salt past every marker the old round could
/// make; the markers, the round's `asked` and the notice's fence forgotten;
/// how long the session has been behind kept), and the release owed to the
/// gone process answered. A `--now` hurried the round that is over and is
/// spent with it, whatever tab it named (as at a give-up and a re-arm), so it
/// waives nothing for a process a person just resumed by hand; the
/// person-hold clocks start again. A `--skip` or `--defer` keeps its own
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

/// A second session JSON can be half-written exactly when a sweep reads it.
/// The valid owner's file does not make a partial directory an exhaustive
/// conversation roster, so even an announced READY cannot be used to act.
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
    // A LIVE sibling (round six, F20): only a running process can hold a
    // conversation, so only its unreadable record makes the roster partial.
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
    // Negative control: a dead pid's file name over a LIVE process's record.
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
    }
}

/// THE MACHINE'S CLAUDE CODE BUILD, SCANNED ONCE PER PROCESS. A model read
/// ([`ModelCtx::read`], behind every restart's, relaunch's and look's
/// [`Targets::read`]) finds the managed build through the developer's own
/// store, as a sweep does, and keeps the build's baked catalog in
/// `<state>/upgrade/builds`, keyed by the binary's path, size and mtime
/// ([`catalog::baked_cached`]) — so every test's fresh state scanned the
/// whole build anew: 230 MB through a debug build's byte loop, 0.7 s of CPU a
/// scan, once per case of a test that restarts several. Measured 2026-10-05
/// (`aterm pkg` claude 2.1.289 installed): 18 tests, 21 s of a serial run
/// alone, and the five slowest of `upgrade_drive::tests` in the gate (41-47 s
/// each at load 10-15).
///
/// `opts`'s state is handed the cache file the PRODUCT wrote for this build —
/// its first `baked_cached`, here once per process (and again whenever the
/// build's key moves) — so the test's read is the product's second read of
/// the same build: the catalog off the file the scan wrote, asserted here to
/// equal the scan's own. Every other file of the state is untouched; the
/// state's listings read `*.json` only, never the `builds` directory. A
/// machine with no managed build seeds nothing, and reads nothing either.
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
    /// This test thread's visits run under the DEFAULT `upgrade_ask = false`
    /// ([`quietly`]).
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

/// A `/bin/sh` SCRIPT AT `exe` RUNNING `body`, the executable a hard link of
/// ONE stand-in per process. macOS assesses every NEW executable at its first
/// exec, one at a time machine-wide, and a script is one: measured
/// 2026-10-05, a fresh `#!/bin/sh` file's first run took 0.46-2.3 s at load
/// 26-32 (its second 3.5 ms), a hard link of a file already run 9 ms. So
/// `exe` is a link of `#!/bin/sh` + `. "$0.sh"`, and `body` is written beside
/// it as `<exe>.sh`, a plain file the shell reads: the same `/bin/sh` runs
/// the same commands with the same `$0`, arguments, environment and
/// descriptors, in the same one process, as it did with `body` under the
/// shebang. The stand-in's directory goes as the process exits. Where no link
/// can be made (another filesystem), `exe` is a fresh copy of the stand-in,
/// slower and as correct.
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

/// INDEPENDENT CASES SIDE BY SIDE: each case on a thread of its own, every
/// one joined, and the first in the order given that failed fails the test
/// with its own panic. For cases that each wait out the product's own fixed
/// waits — a relaunch's 600 ms for the shell to draw its prompt, its 250 ms
/// polls, a lock's bounded ladder — so their waits overlap rather than add.
/// A case shares nothing with another (its own scratch directory, socket,
/// stand-ins and stand-in kernel) and sets any thread-local wait it needs on
/// its own thread.
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

/// A pid that WAS a process and is not one now.
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
    // The file had eight bytes when metadata was read; by the time it was
    // read it had grown to sixteen. No timing race or writer thread needed.
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
    // cell's grapheme is an EMPTY token that must not shift the fields.
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

/// Another supervisor's claim is read off one `sessions` read, never the
/// window's own, so `--status` shows a tab an `aterm drive watch` loop holds.
/// Kept: a tab whose `supervisor=` names another holder, verbatim or encoded.
/// Negative controls: a window's own host's claim (any pid: a predecessor's
/// carried one lapses on its own; encoded, it is still the host's), no claim,
/// and a title that reads like one are no other supervisor's; a refused read
/// is no roster; and an instance that never answers is given up on, never
/// waited on.
#[cfg(unix)]
#[test]
fn foreign_claims_name_another_supervisors_tabs_never_the_windows_own() {
    const ROSTER: &str = concat!(
        "OK 6\n",
        "1 s-a - live claude meta=1 window=1 supervisor=aterm-supervise:42 upgrade=-\n",
        "2 s-b - live claude meta=1 window=1 supervisor=aterm-harness@777 upgrade=-\n",
        "3 s-c - live zsh meta=0 window=1 supervisor=- upgrade=-\n",
        "4 s-d s-a live supervisor=aterm-supervise:9 meta=0 window=1 supervisor=- upgrade=-\n",
        "5 s-e - live claude meta=1 window=1 supervisor=aterm-harness%40778 upgrade=-\n",
        "6 s-f - live claude meta=1 window=1 supervisor=a%20script upgrade=-",
    );
    let (mut c, server) = fake_tab(vec![ROSTER]);
    let claims = foreign_claims(&mut c);
    drop(c);
    assert_eq!(
        claims,
        Some(["s-a", "s-f"].map(str::to_string).into()),
        "another's claim, never the window's own"
    );
    assert_eq!(server.join().expect("server"), vec!["sessions"]);
    let (mut c, _) = fake_tab(vec!["ERR denied"]);
    assert_eq!(foreign_claims(&mut c), None, "no roster");
    // An instance that takes the request and never answers.
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(foreign_claims(&mut RelayClient::new(ours)));
    });
    let given_up = rx.recv_timeout(Duration::from_secs(60));
    drop(theirs);
    assert_eq!(given_up, Ok(None), "a wedged instance is no roster");
}

/// EVERY INSTANCE'S CLAIMS, AS ONE SET: `--status` asked only
/// the instance its socket resolved to, so a tab another supervisor claims in
/// a second aterm read `unwatched`. Two instances' claims are unioned; one
/// that refuses its roster adds nothing and holds no other back.
#[cfg(unix)]
#[test]
fn the_claims_of_every_instance_are_one_set() {
    let (a, _) = fake_tab(vec![
        "OK 1\n1 s-a - live claude meta=1 window=1 supervisor=aterm-supervise:42 upgrade=-",
    ]);
    let (b, _) = fake_tab(vec![
        "OK 1\n1 s-b - live claude meta=1 window=1 supervisor=a%20script upgrade=-",
    ]);
    let (refused, _) = fake_tab(vec!["ERR denied"]);
    assert_eq!(
        claims_of([a, refused, b]),
        ["s-a", "s-b"].map(str::to_string).into()
    );
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

/// A GRID DISPLACED BY A NET-ZERO RE-GRID NEVER MAKES A PLACEHOLDER A DRAFT
/// (the owner's tab of 2026-09-28, 17:08:36 to 17:59:39: the window re-gridded
/// 61>60>61 rows 0.75 ms apart, Claude read 61 == 61 and never repainted, and
/// every row sat one higher than Claude drew it — the cursor one row BELOW the
/// composer, on the rule. The old rule asked the dim cell at the CURSOR's row,
/// read the rule's glyph, and called Claude's own dim suggestion a person's
/// draft for 51 minutes, with nobody at the tab; the upgrade waited `draft`
/// and posted a warn row for a draft nobody typed). The dim cell is read at the
/// CARET row, wherever the cursor is. NEGATIVE CONTROLS: the same displaced
/// grid with TYPED text (the cell reads `none`) and with an unreadable cell is
/// a draft; a cursor inside the text is a draft and asks nothing.
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
    // GNU screen started IN a tab: the window's agent is on the pty screen's
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
    // In THIS process's group: what a launcher script gives its child.
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
    // Said ONCE: the next sweep finds the upgrade failed and asks nothing.
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

/// End to end: a visit's own retarget is said once in the ledger. A visit
/// that finds a newer build than a waiting record's moves the record to it
/// ([`St::for_target`]) and writes `retargeted:<from>-><to>`. The stand-in is a
/// job of its own on no terminal, so the real visit retargets and then waits at
/// the foreground proof (`wait:not-foreground`, no act): the row is the
/// retarget's alone, written once, and the next visit (the record already on
/// the build) writes none. Negative controls: a record already on the target,
/// and a finished one (a new move, no retarget), write no row; a dry run
/// writes neither the row nor the record.
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
    // HOW IT LEFT THE ROUND, SAID TRULY: an
    // announced round the visit retargets IN PLACE — its notice this agent's,
    // for no model, on the build the session runs — keeps its round, marker
    // and READY, and its row says so; it said "pending again" of every
    // retarget. NEGATIVE CONTROL: a stopped round's retarget starts a new
    // pending round, and says "started over".
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
        // A PENDING round the abandoning arm REBUILDS — its session moved off
        // the build the round began on (0.9.0; it runs 1.0.0): pending before
        // and after, no marker either side, and yet a new round (its salt,
        // its recorded wait, its last stop dropped). Told by phase and
        // marker, it was said "in place: its round … kept"; told by the
        // arm, "started over".
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
            // The reading that said "in place" of it: phase and marker as
            // they were.
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

/// A RETARGET IS TOLD BY THE ARM THAT MADE IT: a
/// pending record for 2.1.290 whose visit finds a model move due and 2.1.291
/// installed fails the in-place arm (its promised model, none, is not the
/// move's) and is REBUILT — its recorded wait and last stop dropped, a new
/// salt — pending before and after, no marker either side. Phase and marker
/// read it "in place: its round … kept" (now "still pending");
/// [`St::retarget`] names the arm, and the row says "started over". A
/// release the old notice owes rides it, and so do aterm's own words left in
/// the prompt ([`St::left_typed`]), which are no build's. NEGATIVE CONTROL:
/// the same record with no model move is moved in place, and said so.
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
    // NEGATIVE CONTROL: no model move, in place.
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
    /// A CLAUDE CODE PROMPT the stand-in draws itself ([`Prompt`]), in place
    /// of `screen`: what a `turn` it answers with a skip that typed part of
    /// its text (`typed=<k>`) or all of it (`reason=guard`) left there, and a
    /// person's key after it where `person_key` says; `key … ctrl+u` kills the
    /// line when its `if-gen=` names the screen drawn. `None`: `screen`.
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

/// A control socket at `<dir>/t.sock`, its token beside it, standing in for
/// the aterm instance of [`TAB`] with an idle Claude in it: `sessions` names the
/// tab, `text --json` is [`idle_screen`] (or what `<dir>/t.screen` says, when a
/// test writes it: the screen a later look finds), a `cell` is a plain blank,
/// `status` holds nothing (or answers what `<dir>/t.status` says, when a test
/// writes it), and a `turn` is answered OK — recorded here, typed nowhere.
/// Returns the socket and every request it read, in order. The accept loop
/// ends with the test process.
#[cfg(unix)]
fn instance(dir: &Path) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    instance_of(dir, TAB)
}

/// [`instance`] for the tab `tab`.
#[cfg(unix)]
fn instance_of(
    dir: &Path,
    tab: &'static str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    instance_for(dir, tab, Answers::default())
}

/// [`instance`], answering as `answers` says.
#[cfg(unix)]
fn instance_with(
    dir: &Path,
    answers: Answers,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    instance_for(dir, TAB, answers)
}

/// [`instance`] for the tab `tab`, answering as `answers` says.
#[cfg(unix)]
fn instance_for(
    dir: &Path,
    tab: &'static str,
    answers: Answers,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::io::BufRead as _;
    let sock = dir.join("t.sock");
    std::fs::write(dir.join("t.sock.token"), format!("{TOKEN}\n")).expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let status_file = dir.join("t.status");
    let screen_file = dir.join("t.screen");
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&asked);
    std::thread::spawn(move || {
        let mut turn_replies = answers.turn.clone();
        let mut texts = 0;
        let mut prompt = answers.prompt.clone();
        let mut generation = 77;
        // Status reads since a `lease acquire` (`None`: none acquired yet).
        let mut looks: Option<usize> = None;
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let Ok(mut out) = conn.try_clone() else {
                continue;
            };
            let mut lines = std::io::BufReader::new(conn).lines();
            // `AUTH <token>` is acknowledged with nothing, as the server does.
            let auth = format!("AUTH {TOKEN}");
            if !lines.next().is_some_and(|l| l.is_ok_and(|l| l == auth)) {
                continue;
            }
            for line in lines {
                let Ok(line) = line else { break };
                let verb = line
                    .split_whitespace()
                    .find(|w| !w.starts_with('@'))
                    .unwrap_or("");
                let reply = match verb {
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
                    "blocks" => answers.blocks.to_string(),
                    "line" => answers.line.to_string(),
                    "rekey" if line.ends_with(" rekey withdraw") => {
                        "OK rekey withdrawn".to_string()
                    }
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
                };
                if let Ok(mut log) = log.lock() {
                    log.push(line);
                }
                if writeln!(out, "{reply}").is_err() {
                    break;
                }
            }
        }
    });
    (sock.to_string_lossy().into_owned(), asked)
}

/// The requests of `asked` that TYPE into the tab.
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

/// The continuation a relaunched agent is owed looks for a person first: a
/// person's keystroke within the grace (`human_ms=`) holds it (`wait:held`),
/// nothing typed and the restart still in flight, so the next idle point
/// types it. Negative control: with nobody at the keys it is typed, with
/// `yield=0.2`.
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

/// THE CONTINUATION IS TYPED UNDER THE HARNESS'S HAND, whoever holds it: a
/// later visit that finds the relaunched agent owed one (no caller holds a
/// hand) takes one for the typing alone and gives it back at once; the
/// relaunch's own wait passes the hand it has kept and the typing neither
/// takes nor gives back a second. NEGATIVE CONTROL: a tab another driver
/// holds is `wait:held`, nothing typed, and this process gives back nothing.
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
    // NEGATIVE CONTROL: the tab is another driver's.
    let (step, asked) = run(
        "continuation-theirs",
        "ERR lease held holder=orchestrator expires_in_ms=5000",
        false,
    );
    assert_eq!(step, "wait:held", "{asked:#?}");
    assert_eq!(count(&asked, " turn "), 0, "{asked:#?}");
    assert_eq!(count(&asked, " lease release "), 0, "{asked:#?}");
}

/// THE LINE IS NEVER TYPED OVER A LIVE OWNER UNDER THE AGENT'S OWN HOME:
/// [`type_relaunch_line`]'s check that no live process holds the
/// conversation reads the records it is given — the relaunched agent's
/// ([`St::records`], review of `fix/cold-restore-login-home`, 2026-10-09). A
/// Claude Code holding the conversation under `<dir>` keeps the line waiting
/// (`conversation-owner-ambiguous`), nothing typed. NEGATIVE CONTROL: the
/// user's `~/.claude`, which names no process of `<dir>`'s, lets it type —
/// the read every relaunch made before.
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

/// The relaunch line is typed under the harness's hand, never let go for it,
/// so a retrying orchestrator's `turn` cannot slip into the bare shell before
/// the line or the booting agent after it. The hand is the server's hard
/// lease ([`Hand`]: `lease acquire … hard`), which refuses every other
/// connection's writes and lets its own through, its `turn` included: between
/// the hand taken and the line typed nothing is released or re-taken, for
/// bash's raw `send` and zsh's fenced `turn` alike. A turn refused `ERR busy
/// …` (a hand that could not be taken, or lapsed) is a wait, never the refusal
/// that fails a relaunch whose agent is already gone. Negative control: from a
/// server that takes no hard lease (an older aterm), the hand holds nothing
/// and is given back by nothing.
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

/// The last look before the relaunch line is a `status` read at the moment it
/// is typed, after every wait before it: a person who typed at the returned
/// prompt within the grace, a halt and anyone's hand each hold the tab, and
/// nothing is typed, so the line is never joined to a person's half-typed
/// command. Negative control: a keystroke older than the grace, no hand, no
/// halt: the line is typed.
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

/// At a bash prompt the relaunch line is typed, not pasted: a crashed agent
/// leaves bracketed-paste mode on, and macOS's bash 3.2 reads the paste's
/// frame as text (`--resume <id>01~`). Raw bytes (`send --`), then Enter.
/// Negative control: a shell that is not bash (this test binary standing in)
/// is pasted to with a `turn`, as zsh and fish are.
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

/// Every look at the job before an act is the one standing between a ctrl-z
/// and that act. The agent is its shell's foreground job at the first look and
/// a background job from the Nth read on; each N stops the sweep at exactly
/// one look, and nothing is typed or signalled: the re-read before the
/// announce (N = 1, on a pending upgrade), the same re-read before the SIGTERM
/// (N = 1), restart's own gate (N = 2), and the last look before the signal
/// (N = 3). Dropped, each lets the act through. Control: with every read
/// foreground, the announce is typed.
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

/// Only the relaunch itself — the SAME conversation, a child of the shell the
/// line was typed at — is adopted and told it was upgraded.
#[cfg(unix)]
#[test]
fn only_the_relaunch_itself_is_adopted_and_told_it_was_upgraded() {
    let dir = scratch("adopt");
    let opts = drive(&dir);
    // Its parent is THIS process, standing in for the shell.
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
    // END TO END, the in-flight branch of a sweep: a process the recorded
    // shell did not start holds the conversation.
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
    // Control: the recorded shell IS its parent — adopted, on to the tab.
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

/// A relaunch is judged from a parent READ: a parent that cannot be read (the
/// process ended as it was asked, or `ps` could not run) is no verdict at all.
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

/// End to end, the in-flight branch: the new process holding the conversation
/// fails the upgrade for good only from a parent read as another than the
/// recorded shell. Unread, it is a wait (the phase kept, nothing in the
/// ledger, the next sweep asks again), so the real relaunch is still told to
/// carry on.
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

/// The stale-exit bound counts from the exit a look saw, not the signal: a
/// restart whose agent took longer than [`STALE_S`] to shut down, seen gone by
/// the host's carry a moment ago, is carried by the orphan pass too, never
/// failed `stale-exit` from under the carry. Negative controls: seen gone past
/// the bound, it is stale; and the pass's own first sighting of an exit (no
/// stamp: the exit may be long past) decides on the signal, the one time known
/// to come before it.
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

/// A Claude Code restart records when its agent was seen gone
/// ([`St::exited_at`]), as the Codex lane does: a failure after it has no
/// process left to vet it by, and is the tab's record in the status view.
/// Seen by the orphan pass, over an agent no longer alive, before its expiry
/// is decided and before a socket is dialled. Negative controls: an agent
/// still alive (exiting) is not gone and its record says nothing of an exit,
/// and a dry run writes nothing.
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

/// A restart under way that has not moved is said once on the ledger. A Claude
/// Code the upgrade SIGTERMed that hangs in its own shutdown (a hook, an MCP
/// teardown) keeps its session file and pid, and every visit answers
/// `wait:exiting`. Past [`STALE_S`] the visit says it once (`stuck:exiting`,
/// keyed on [`St::noted`] as a held-back wait is), in the status view's words;
/// a later visit writes nothing more. It stays a note: the visit still waits,
/// the record stays in flight (a failure would leave an agent that exits later
/// un-relaunched), and nothing is signalled or typed. The ledger and
/// `--status` agree. Negative control: within the bound nothing is said.
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

/// The orphan pass: a hung agent that has already removed its session file
/// (Claude's graceful shutdown removes it first) is no conversation a visit
/// sees, and the orphan pass skips it while its pid lives. Past [`STALE_S`]
/// that skip says it once too; nothing is acted on and its record stays in
/// flight. Negative controls: within the bound nothing is said, and a dry run
/// writes nothing.
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

/// A relaunched agent that never carries on: the line that resumes it was
/// typed and the new process holds the conversation, but its carry-on waits
/// (never idle, or a tab whose ownership moved). Past [`STALE_S`] from the
/// relaunch, the carry-on's wait says it once (`stuck:relaunched`); nothing is
/// typed and the record stays relaunched. Negative controls: within the bound
/// nothing is said, and a relaunch on exit's record (no upgrade, in no
/// `--status`) says nothing either.
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
    let _quick = QuickWaits::new();
    let dir = scratch("seen-gone-relaunch");
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
        pid: dead_pid(),
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
    assert_ne!(st.exited_at, 0, "seen gone");
    assert_eq!(turns(&asked), 0, "nothing typed");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every step word is taught, and an untaught one is no last word: the last
/// words are named in [`after`]; every word the lanes say is taught
/// ([`unknown_step`] false); a word nobody taught is looked at again at the
/// ladder's longest pause, and the host warns of it. Negative control: a
/// catch-all `_ => After::Finished`, replayed, reads the untaught word as the
/// last one.
#[test]
fn every_step_word_is_taught_and_an_untaught_one_is_looked_at_again() {
    for last in [
        "current",
        "done",
        "done:fresh",
        "done:taskless",
        "done:no-continue",
        "done:unconfirmed",
        "ended:graceful-exit",
        "ended:one-shot",
        "skip:not-selected",
        "skip:another-home",
        "skip:not-a-tui",
        "skip:no-managed-codex",
        "daemon-updated",
        "would-announce",
        "would-restart:fresh",
        "would-release:gave-up",
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
        "wait:announce-refused:changed",
        "wait:not-idle:busy",
        "wait:failed",
        "wait:box",
        "reopened:notice-process-gone",
        // The Codex ladder's and the goal pause's words: each is looked at
        // again, none of them the last word.
        "goal-paused",
        "goal-esc",
        "goal-paused-seen",
        "goal-resumed",
        "goal-resumed:moved",
        "goal-released:by-hand",
        "goal-pause-refused",
        "left-typed:goal-pause",
        "wait:goal",
        "wait:goal:record",
        "wait:goal:switch",
        "wait:goal-held",
        "wait:goal-pausing",
        "wait:goal-pausing:no-fence",
        "wait:goal-pause-refused:held",
        "wait:goal-resume",
        "wait:goal-resuming",
        "wait:goal-left-paused",
        "wait:goal-sandboxed",
        "wait:switch",
        "wait:daemon-turn",
        "wait:daemon-busy",
        "wait:pin:busy-thread",
        "wait:changed",
        "wait:in-flight",
        "wait:no-shell-integration",
        "wait:screen-unreadable",
        "wait:failed:no-resume",
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
    // Negative control: the match as it ended until this day.
    let old = |step: &str| {
        if unknown_step(step) {
            After::Finished
        } else {
            after(step, 0)
        }
    };
    assert_eq!(old("brand-new"), After::Finished);
    assert_ne!(after("brand-new", 0), old("brand-new"));
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
    // Negative control: the rule as it stood.
    let any_row = |rows: &[String]| rows.iter().any(|r| r.contains("esc to interrupt"));
    assert!(any_row(&quoted));
}

/// The Claude notice fences on its input box: where the host's `help turn`
/// names `if-rows=`, the notice's `turn` carries the box's three rows (the
/// rule, the `❯` caret row, the rule), hashed by the definition the host
/// shares, and no `if-gen=`, so a spinner row repainting above the box does
/// not refuse it. Negative controls: a host whose `help turn` has no
/// `if-rows=` is fenced on the generation, and so is a screen whose caret row
/// is not framed by rules (another program's composer).
#[cfg(unix)]
#[test]
fn the_claude_notice_fences_on_its_input_box_where_the_host_takes_if_rows() {
    use aterm_types::control_verbs::RowsFence;
    const ROWS_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] \
                             [if-rows=<first>-<last>:<hex16>] ...";
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

/// The fenced notice asks nothing between its read and its `turn`: the server
/// types nothing when output came after the generation it is handed, so the
/// `help turn` probe is asked first and kept for the connection, and a read
/// that needs the composer `cell` is read once more after it: the last `text`
/// read is followed by the `turn` itself. A composer that moved between the
/// two reads is `changed`, nothing typed. Negative control: the other order,
/// replayed on a fresh connection, sends the `cell` and the probe after the
/// read the turn is fenced on.
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
    // Negative control: the order as it stood until this day.
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

/// THE FENCE JUDGES THE COMPOSER WHERE IT IS DRAWN (the owner's tab of
/// 2026-09-28: the grid one row displaced, the cursor on the rule BELOW the
/// composer). The `cell` is asked at the composer's caret row, whatever the
/// cursor's — Claude's own dim suggestion is an empty composer there, typed
/// text is a draft — and the two reads a `cell` sits between are the same
/// composer when the CARET row's text is the same, not the cursor row's (a
/// person typing changes the composer row, which the cursor row under a
/// displaced grid would not show). NEGATIVE CONTROLS: typed text on the
/// displaced grid is a draft; a composer that changed between the reads is
/// `changed`, nothing typed.
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
    // The same rows with the text TYPED: a draft.
    let (dir, mut c, _) = rig(
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

/// THE HOST'S NEXT MOVE after one step: park again at the next idle point
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
    // A stopped round says so as it stops (`failed:<why>`, `refused:<what>`)
    // and, resting with nothing owed, `wait:failed` (an owed release reads
    // `wait:release:<why>`, a Codex record's stop `wait:failed:<why>`); none
    // is a last word, since upgrades must never stall: a new round starts
    // once it has rested `upgrade::RETRY_S`, so each is looked at again, up to
    // the ten-minute look; so is the new round's `rearmed:<why>`, whose first
    // notice the next look types.
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

/// Whether a step moved the session, and whether it typed a turn: a step that
/// typed into the agent or ended it leaves no point for the loop to decide
/// again, and one that typed a turn (a notice, a carry-on, the release line)
/// is awaited as the harness's own, whose answer is no work of the worker's.
/// Negative controls: a step that ended the agent typed no turn into it; and
/// every word that typed and ended nothing (a carry-on's model read later,
/// `done`, `done:model-unconfirmed`, a daemon-mode Codex carried on with
/// nothing typed, a release still owed or dropped) leaves the point standing.
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

/// The upgrade owns a session's turn ends by its own step's word
/// ([`owns_turn_ends`], what the window's host answers its loop with): after
/// the announcement, and while READY is given and the restart's gate only
/// settles or drains background work (bounded); never after an answer without
/// READY, a hold, a release still owed, or a last word. Negative controls:
/// past the bounded looks a drain or a settle owns nothing, and a release owed
/// owns nothing whatever holds it.
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

/// UNDER THE QUIET FLOW A WAIT ON THE AGENT'S OWN WORK OWNS NO TURN END AND
/// BOUNDS NONE (round eight, #25; [`owns_turn_ends_as`], [`bounded_wait_as`]):
/// its drain ownership is for an agent that answered READY, and the quiet
/// flow asks for none — a pending round with a shell under its agent waited
/// `background` at every idle point and held the loop's continuation for
/// [`OWNED_BACKGROUND_LOOKS`] looks for a restart that could not come before
/// the work ended, and spent the settle's looks the restart is due at once it
/// does. NEGATIVE CONTROLS: the ask flow's drain owns and bounds as ever, and
/// the quiet restart's settle, a notice and a person's hold own in both.
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

/// A PERSON AT THE TAB OWNS THE TURN END FOR THE PERSON'S GRACE (the
/// 2026-09-27 20:27 point, s-d3346, live records): the agent answered READY at
/// 20:27:20, the owner had typed "ok" at 20:26:42, and the upgrade waited
/// `held-back:attended` (20:27:27), then `wait:attended` (20:27:49, 20:28:10)
/// OWNING NOTHING, its next look climbing to 60 s — so at the grace's lapse
/// (20:28:42) the loop typed `keep going` over the READY, and the upgrade then
/// waited 5 h 50 min for an idle point that never came. Now the attended wait
/// owns the point while the person's grace runs — every look reads it afresh,
/// and a person scrolling through the answer renews it — and is looked at again
/// at the first rung while the next look owns it ([`after_attended`]), so the
/// look that finds the grace lapsed lands within 20 s of it and takes the
/// restart. Negative controls: past the backstop ([`ATTENDED_OWNED_S`] beyond
/// the grace) it owns nothing, so a stamp that fails closed never holds the loop
/// for good, and its looks go back on the ladder; `awaiting-ready`, a
/// person's hold, a give-up and an adoption still own nothing; a break still
/// gives an attended wait's claim back; and every other wait keeps its
/// ladder.
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

/// The release is typed only where it is the upgrade's next act
/// ([`release_is_next`]), and a step that leaves one owed never reads as the
/// host's last word ([`owed_word`]). The upgrade's own give-up, void and stop,
/// and its waits on nothing of its own, leave the release next; every wait of
/// the notice's or the restart's gate keeps its word (what it waits for goes
/// first). A stop's own word, `failed:…` / `refused:…`, is the host's last
/// word ([`After::Finished`]), so one that leaves a release owed says
/// `wait:release:<why>` instead. Negative controls: an act the host looks on
/// after (`gave-up`, `drain-expired:…`) keeps its word.
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

/// [`due`] parks a session only when it has an upgrade to take (a restart of
/// this tab in flight is one) and says what it could not read rather than
/// "not due", so an unanswered socket never lets the upgrade go for good.
/// Negative controls: another tab's restart, and a finished one, are not
/// `Yes`; with no socket to read the tab by, they are `Unread(no-socket)`,
/// never `No`.
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
    }
}

/// The instance's one tab, [`TAB`], its foreground group 101.
fn the_tab() -> [LiveTab; 1] {
    [LiveTab {
        sid: TAB.to_string(),
        fgpgid: Some(101),
    }]
}

/// AN ANNOUNCED ROUND WHOSE NOTICE CARRIED A MODEL IS THE HOST'S TO VISIT:
/// with the build current and the rule keeping the
/// model now (here nothing is offered at all), the host's pre-filter — the
/// one [`due`] and the step's pass ask — skipped it, so a model-only round
/// was never visited again: its READY never acted on, a barred ride never
/// dropped, the agent the notice asked to wind down never released.
/// NEGATIVE CONTROLS: the same round carrying no model, and a gave-up round
/// whose release was typed (visited at an attach, as every give-up is), are
/// not looked at.
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
    // NEGATIVE CONTROLS.
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

/// A RECORD IS READ ONLY WHERE IT DECIDES (the review of the silent-drop
/// fix): a record whose agent leads the tab and is behind, but whose PROCESS
/// could not be read whole — its ids (a `ps` that could not spawn: under
/// fork pressure), its argv, or the tab's foreground caught moving between
/// the roster and the ids — decided nothing, and the look is
/// [`UNREAD_PROCESS`]. It was counted as read before its process was, and
/// read "not due": a READY'd session was let go without a word. NEGATIVE
/// CONTROLS: every read whole is `Yes`; a readable environment naming
/// another tab is read — none of this tab's — and `No`; a current record
/// needs no process read at all.
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

/// A launch's record is waited on for its launch, not for good ([`no_record`]):
/// no record of the tab's agent is a wait ([`UNREAD_RECORD`]) while the agent
/// (the tab's foreground group's leader) is younger than [`RECORD_WINDOW_S`],
/// and nothing to take once older: a Claude Code that keeps its records
/// elsewhere (`CLAUDE_CONFIG_DIR`), or whose session is no interactive CLI
/// one, never writes one this look can find. A leader whose start cannot be
/// read is a process not read while it lives, and nothing once gone; a tab the
/// instance no longer has is nothing. A machine with no `.claude/sessions`
/// holds no records (the same window decides), while one whose records cannot
/// be read whole is unread.
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
    // Half-written by a LIVE process (this one): only a running process's
    // record can be mid-write (round six, F20).
    let me = std::process::id();
    std::fs::write(
        dir.join(format!(".claude/sessions/{me}.json")),
        "{\"pid\": 1",
    )
    .expect("half");
    assert_eq!(claude_records(&dir), None, "caught half-written: unread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tier-1 of `HarnessUpgradeLook`, its classifier half (the host's bind in
/// aterm-gui, `the_real_look_conforms_to_the_upgrade_look_model`, fakes `due`
/// from the model's `readable` bit, so it binds only the host's reaction).
/// Here the real [`due_among`] and [`no_record`] are driven over every
/// combination of what a look reads of one tab (its agent's record there or
/// not, behind or current, its group, its ids and argv each read or not, the
/// foreground caught moving, its environment naming no tab, its own or
/// another, the agent a second old or past [`RECORD_WINDOW_S`]), and each
/// verdict is taken as the model's action (`Yes` is `Look`, `No` is
/// `NotDue`, `Unread` is `Unread`) at the state the combination projects to,
/// a look owed: `due` an upgrade the step could take (the worst case where
/// nothing says), `readable` every read the verdict needed answered. The
/// model must enable each. Negative control: a classifier that counts the
/// record as read before its process answers `No` where a process read
/// failed: `NotDue` where the model forbids it (`NeverDropped`'s cause).
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

/// A session is behind from its worker's attach ([`note_behind`]): its state
/// is minted `Pending` there (the target, the tab, its age from now), and the
/// first step, at the first idle point after a busy first turn, reuses it and
/// keeps that age. Negative controls: a state already there is left as it is,
/// and a session on the newest build gets none.
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

/// A fresh launch is behind from its worker's attach, though the note can only
/// be made once Claude Code has written its record. The host asks again until
/// it can be read, with the second it first saw the session: the state is
/// minted with its age from then ([`mint_behind`]'s `since`), not from the ask
/// that finally read it. A look that cannot read the session notes nothing and
/// says why ([`Behind::Unread`]), never "nothing to note". Negative control: a
/// `since` in the future never makes a session younger than now.
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

/// A session upgraded before: the state an earlier upgrade left (done, or
/// failed on another build) no longer says where the session stands once a
/// newer build lands, so it is minted behind anew ([`behind_anew`]), its age
/// from now, not left to the first step. Negative controls: a state still in
/// flight, a carry-on whose model is still waited for, a failure on this very
/// build (final for it), and a pending one (the step's, its age kept) are left
/// as they are.
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

/// A conversation resumed in another tab is behind there from the host's
/// attach ([`mint_behind`]): the watch, the window's view and `--status` find a
/// record by its tab, so at the attach that proves the conversation is held
/// here the record is moved into this tab and onto the build the visit would
/// choose (`retargeted:9.9.8->9.9.9`, said once in the ledger), its round and
/// age kept, and this tab's rows name it; a stopped round resting is moved
/// into this tab too. Negative controls: a record already in this tab is left
/// as it is, and so is an announced round in another tab (its tab the
/// notice's fence).
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

/// A shell whose dialect the relaunch line is written for, alive until the
/// test lets it go: bash blocks in its own `read` of a pipe the handle holds,
/// so it stays the process and never ends on a clock a slow test outlives
/// (a bash gone before its test ends is a zombie `ps` names `<defunct>`: a
/// shell with no dialect). It holds that pipe and its stdio, nothing else: a
/// bash that lives as long as its test is exactly the holder this file's
/// `Command` import keeps out of another test's `ps`.
fn stand_in_shell() -> aterm_uds::spawnfd::Child {
    Command::new("/bin/bash")
        .args(["-c", "read -r _"])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("bash")
}

/// THE OWNER'S `Skip` AND `Not today` HOLD A CRASH'S RELAUNCH (round eight,
/// #24): the relaunch on exit chose the newest build it could move to and
/// never read the owner's word on the tab, so an agent that crashed came
/// back on the very build its owner had skipped (or deferred). Now a build
/// the conversation's upgrade record holds the session off for this tab is
/// no relaunch target: it comes back on the program it ran. NEGATIVE
/// CONTROLS: no word, a deferral run out, a word on another tab and `--now`
/// move it as before; a build NEWER than the skipped one is moved to.
#[cfg(unix)]
#[test]
fn the_owners_hold_keeps_a_crashs_relaunch_off_the_build_it_names() {
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
    // NEGATIVE CONTROLS.
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
    let _ = shell.kill();
    let _ = shell.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RESTART OR A RELAUNCH ON ITS OWN PROGRAM KEEPS THE OWNER'S HOLD AND THE
/// MOVE CLOCK, AND THE ROUND AFTER IT KEEPS THEM (round eight, #24): a memory
/// banner's restart in place, a bucket's fallback, a crash relaunched on the
/// program it ran each wrote a fresh record over the upgrade's, and the next
/// visit's retarget minted a round with no word and its clock at zero — under
/// the quiet flow moving the session onto the very build the owner skipped
/// at its next idle point. Now the restart's record keeps the hold and how
/// long the session has been behind ([`St::kept_by_restart`]), and the
/// retarget off it keeps the hold that still names its target and that clock
/// ([`St::for_target`]). NEGATIVE CONTROLS: a skip does not follow onto a
/// NEWER build; a deferral run out, `--now` and a word on another tab are not
/// kept; a finished MOVE keeps no clock (the session is behind anew).
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
    // NEGATIVE CONTROL: a skip names ONE build.
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
    // NEGATIVE CONTROL: a finished MOVE onto its build is behind anew.
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

/// A RESTART THAT STOPS KEEPS THE OWNER'S SKIP OF THE BUILD IT HELD OFF
/// (round eight, #24): a restart or relaunch on the session's own program
/// carries the owner's `Skip` of the build it did not move to
/// ([`St::kept_by_restart`]); when that restart STOPS — a relaunch overtaken
/// or out of time, the conversation resumed elsewhere, a refused relaunch —
/// its record is a stopped one on the build the session still runs, and the
/// next retarget's stopped arm dropped every skip unasked: a new round with
/// no word, which the quiet flow moved onto the skipped build at its next
/// idle point. Now a skip that still names the new target is kept
/// ([`St::word_onto`]), with the move clock. NEGATIVE CONTROLS: an upgrade
/// round's own skip of the build IT moved to does not follow onto a newer
/// one, and neither does the restart's onto a build newer than the one it
/// names.
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
        // NEGATIVE CONTROL: a skip names ONE build.
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
    // NEGATIVE CONTROL: the upgrade round's own skip, its round stopped, does
    // not follow onto the newer build (it names the build that round moved to).
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

/// A RESTART KEEPS THE MOVE CLOCK ONLY OF A RECORD ABOUT THE BUILD IT KEEPS
/// THE SESSION ON (round eight, #24's review): [`St::kept_by_restart`] copied
/// the prior record's clock whatever build that record was about. A round for
/// 2.1.290 stopped `resumed-elsewhere` because the person resumed the
/// conversation by hand on 2.1.290; a crash or a memory restart of it, kept on
/// that program, carried the old round's clock onto its own record, and when
/// 2.1.300 landed days later its round began at the ladder's last rung — no
/// settle, a person's grace cut to a keystroke's, the quiet restart at the
/// first idle point, its row overdue at once. Without the restart in between
/// the retarget's stopped arm starts that clock afresh, and so does the
/// restart now — through an older same-program record too. And a restart of
/// the same program that STOPPED (a relaunch overtaken or out of time) is
/// still behind, as the retarget's stopped arm keeps it: its clock was
/// dropped. NEGATIVE CONTROLS: a round about the build the restart keeps,
/// and a stopped restart of it, keep the clock; the owner's skip of a build
/// is kept whatever build its record is about, as the retarget without the
/// restart keeps it.
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
    // NEGATIVE CONTROL: a round about the build the restart keeps.
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
    // A STOPPED restart of the same program is still behind (#24's review):
    // its clock is kept by the restart after it, as by the retarget.
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
    // NEGATIVE CONTROL: a stopped restart of ANOTHER build keeps no clock.
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

/// A STOPPED ROUND'S RETARGET KEEPS WHAT TELLS THE CODEX LANE ITS TUI WAS TOLD
/// TO WIND DOWN (round eight, #23): the Codex lane types no release, and its
/// quiet move carries a TUI on after its relaunch only where the record says
/// a notice once reached it ([`St::noticed_at`]) or its round before gave up
/// ([`St::last_stop`]). The stopped and abandoning arms of [`St::retarget`]
/// built a fresh round with neither, so a newer Codex landing during a
/// give-up's rest made the TUI's next move a quiet one, nothing typed after
/// it. Now both arms carry the notice's stamp, and a Codex record the give-up
/// (as the rest's re-arm records it). NEGATIVE CONTROLS: a Codex round
/// stopped for another reason with no notice carries nothing; a Claude Code
/// record's owner's view keeps no stop (its release carries the wind-down).
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
    // THE ABANDONING ARM (a round for another model): the stamp, and a
    // re-armed give-up's stop.
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
    // NEGATIVE CONTROLS: nothing asked, nothing carried; a Claude Code
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

/// RELAUNCH ON EXIT relaunches only an agent that CRASHED — exited leaving
/// Claude's own record behind (a crash cannot remove it) — on the
/// conversation that record names, and carries on a relaunch in flight
/// rather than typing a second one. Dry runs: the steps are decided against
/// real processes and files, and nothing is typed or written. NEGATIVE
/// CONTROLS: an agent that still runs; a launch no record names a
/// conversation of, and a one-shot run — each the launch's own end, never
/// relaunched and never refused; and a GRACEFUL exit (its record removed:
/// a person's or an orchestrator's `/exit`, a `kill`) — someone's decision,
/// never relaunched from the snapshot.
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
    let mut shell = stand_in_shell();
    let shell_pid = shell.id();
    let snap = |pid: u32, start: &str, session: Option<&str>| Snapshot {
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
    // be resumed: the agent is started afresh. One whose transcripts cannot
    // be read at all is resumed (Negative control above: no projects dir).
    let project = opts.home.join(".claude/projects/-w");
    std::fs::create_dir_all(&project).expect("projects");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch:fresh");
    std::fs::write(project.join(format!("{SESSION}.jsonl")), "{}\n").expect("transcript");
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(r.step, "would-relaunch");
    std::fs::remove_dir_all(opts.home.join(".claude/projects")).expect("rm");
    // ...and the crash's surviving record wins over the snapshot: it names
    // the conversation as the agent last held it.
    let later = "0badf00d-9999-2222-3333-444455556666";
    crash_record(dead, later);
    let r = at_attempt(&snap(dead, start, Some(SESSION)), false);
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("would-relaunch", later)
    );
    // A record of ANOTHER process's start is not this agent's: its own is
    // gone, a graceful exit.
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
        let shell = stand_in_shell();
        let dead = dead_pid();
        let snap = Snapshot {
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

/// What the exit left is read as the exit is seen, and kept. A crashed agent's
/// record that another Claude Code removes during the back-off (starting in
/// another tab) is still relaunched, on the conversation the look kept.
/// Negative controls: a graceful exit whose record goes a moment after the
/// exit is seen (inside the settle; `/exit` removes its own in its last
/// moments) is read as removed and left, though the first look saw the
/// record; and a record read at the attempt, replayed, reads the crash as a
/// graceful exit.
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
    // THE CRASH: its record survives the settle, and the look keeps it.
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
    // The defect, replayed: the record read at the attempt says graceful.
    assert_eq!(a.attempt(&ExitRecord::Unread), "ended:graceful-exit");
    // …unless an attempt of this exit read its record and relaunched it, and
    // the agent that relaunch brought up was gone again before anything read
    // it (the window's host tries the exit again): the record that relaunch
    // filed names this agent, and the crash it read stands — the relaunched Claude Code's start swept
    // the record it read.
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
    // …unless the stall's remedy ended it (U1): relaunched all the same.
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

/// An exit the harness's own restart made is never read as someone's. The
/// upgrade's SIGTERM (and the restart in place's) is a graceful end, so Claude
/// removes its own record ([`ExitRecord::Removed`]). A restart its step left in
/// flight past [`STALE_S`] (the relaunch lane's back-off can pass the bound)
/// is stopped with the moment its agent was seen gone (the status view shows
/// it) and said, `refused:<why>` (the one case a person is asked about), for
/// the upgrade's causes and the restart in place's alike, and again for a
/// restart already stopped after its exit; it never falls through to
/// `ended:graceful-exit` unsaid. A relaunched one that never registered is
/// never relaunched afresh: its line was typed once. Negative controls: a
/// relaunch on exit's own record (its agent crashed), a record of another
/// process than the one that left (closed with no failure stamp, where that
/// process is the relaunch's own agent and the one that left the relaunched
/// one), and a restart whose signal the kernel refused (its agent never seen
/// gone) each fall through.
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
    for cause in [
        "",
        CAUSE_UPGRADE_FRESH,
        CAUSE_MEMORY,
        "model:claude-fable-5-1",
        "model-back:",
    ] {
        save(
            &opts,
            SESSION,
            &record(Phase::Exiting { at_s: old }, cause, a.snap.pid),
        );
        assert_eq!(attempt(), "refused:stale-exit", "{cause:?}");
        let st = load(&opts, SESSION).expect("kept");
        assert_eq!(
            st.phase,
            Phase::Failed("stale-exit".to_string()),
            "{cause:?}"
        );
        assert_ne!(st.exited_at, 0, "{cause:?}: its agent was seen gone");
        // Stopped after its exit: the next attempt says so again.
        assert_eq!(attempt(), "refused:stale-exit", "{cause:?}");
    }
    // Relaunched and never registered: said, never typed a second time.
    save(
        &opts,
        SESSION,
        &record(Phase::Relaunched { at_s: old }, "", a.snap.pid),
    );
    assert_eq!(attempt(), "refused:no-resume");
    // Negative controls. A relaunch on exit's record: its agent crashed, and
    // its graceful end is read as before; the record too old to act on is
    // closed as a stop ([`St::stop`]), with the moment its agent was seen gone.
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
    // The relaunched agent left before its continuation: the record names the
    // agent its restart ended (seen gone as the relaunch was typed, or not
    // stamped yet), and the one that left now is the relaunched one,
    // gracefully: someone's decision. The record closes, and the exit is no
    // failure of the move: no stamp, so the status view vets the stop against
    // a holder and shows no stall for a relaunch that landed.
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
    // The same, the exit tried again on the agent before it (the window's
    // host): a relaunch on exit
    // that landed on a process gone already is closed as one overtaken —
    // carried, it held the tab for a dead process until it went stale.
    // NEGATIVE CONTROL: one whose adopted process lives is carried on.
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
    // A relaunch after aterm's own end (`host`): its record ended no agent
    // of its own, so it is closed and read as before.
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

/// The host finds the restart that ended the agent that left ([`restarted`]),
/// before whose exit it was — ONE rule for both lanes: the record of a restart
/// that ended its agent itself, filed for the process that left (a Claude
/// Code's SIGTERM, a Codex TUI's `/exit`) in its tab — and carries it
/// ([`carry_restart`]): on to the tab while it may act ([`STALE_S`] from the
/// exit the carry saw, never from the signal), `refused:<why>` once it cannot
/// (said again while it is still that restart's), never typed afresh.
/// Negative controls: another process, no snapshot, another tab and the other
/// agent's lane find nothing; a stop long past its exit is no longer this
/// exit's; and in the Codex lane, whose records are filed per tab, a Codex a
/// person started at the prompt, and a record the relaunch on exit filed
/// there (a crash, a stall, aterm's own end), are no restart of the
/// harness's: a person's `/exit` of that Codex is theirs.
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
    assert!(restarted(&opts, false, Some(pid)));
    assert!(
        !restarted(&opts, false, Some(dead_pid())),
        "another process"
    );
    assert!(!restarted(&opts, false, None), "no snapshot proves nothing");
    assert!(!restarted(&opts, true, None), "no Codex record");
    let other_tab = Opts {
        only_sid: Some("s-0ther".to_string()),
        ..opts.clone()
    };
    assert!(!restarted(&other_tab, false, Some(pid)), "another tab");
    // Carried to the tab (no socket here).
    assert_eq!(
        carry_restart(&opts, false, Some(pid)).step,
        "wait:no-socket"
    );
    // A shutdown that outlived the bound (a hung SessionEnd hook, an MCP
    // teardown): signalled more than STALE_S ago, its agent seen gone only by
    // this carry. The bound counts from that exit, never from the signal:
    // carried, and when it was seen gone kept for the next try.
    let signal = now - STALE_S - 60;
    save(&opts, SESSION, &claude(Phase::Exiting { at_s: signal }, 0));
    assert_eq!(
        carry_restart(&opts, false, Some(pid)).step,
        "wait:no-socket",
        "carried from its exit"
    );
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Exiting { at_s: signal });
    assert!(st.exited_at >= now, "seen gone at the carry: {st:?}");
    assert_eq!(
        carry_restart(&opts, false, Some(pid)).step,
        "wait:no-socket",
        "and again at the next try"
    );
    // Past its bound from the exit: stopped, said, and still this exit's.
    save(
        &opts,
        SESSION,
        &claude(Phase::Exiting { at_s: signal }, now - STALE_S - 1),
    );
    let r = carry_restart(&opts, false, Some(pid));
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("refused:stale-exit", SESSION)
    );
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(st.phase, Phase::Failed("stale-exit".to_string()));
    assert_ne!(st.exited_at, 0);
    assert!(restarted(&opts, false, Some(pid)));
    assert_eq!(
        carry_restart(&opts, false, Some(pid)).step,
        "refused:stale-exit"
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
    assert!(!restarted(&opts, false, Some(pid)));
    assert_eq!(
        carry_restart(&opts, false, Some(pid)).step,
        "refused:no-restart-in-flight"
    );
    std::fs::remove_file(state_path(&opts, SESSION)).expect("rm");
    // A Codex restart in flight, found by its TUI and carried by its lane:
    // past its bound, said. Its five minutes count from its `/exit`, not from
    // the TUI's exit: a TUI still alive the Codex lane's `EXIT_TAKEN_S` after
    // its `/exit` is one the `/exit` did not take, so one that left only now
    // is past the bound all the same.
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
    assert!(restarted(&opts, true, Some(tui)));
    assert!(!restarted(&opts, false, Some(pid)), "not Claude Code's");
    let r = carry_restart(&opts, true, Some(tui));
    assert_eq!(
        (r.step.as_str(), r.session.as_str()),
        ("refused:stale-exit", codex.as_str())
    );
    assert!(
        !restarted(&opts, true, Some(tui)),
        "stopped: no longer in flight"
    );
    // Relaunched: the old TUI's exit may still be read after the line was
    // typed, so a relaunch no TUI was adopted from is still the restart's.
    // Negative control: once its supervisor adopted the new TUI
    // (`resumed_pid`), that TUI leaving (an `/exit`, a crash) is its own exit,
    // never the restart's: no hand held on the tab, no badge saying the
    // harness ended it.
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
        restarted(&opts, true, Some(tui)),
        "relaunched, none found yet"
    );
    // WHICH TUI LEFT, AND WHAT FILED THE RECORD — Claude Code's rule
    // ([`ended_by_restart`]). No snapshot names none, and a Codex a person
    // started at the prompt the relaunch waited on is not the one the
    // restart ended. The relaunch on exit files its own record in the same
    // place (a crash, a stall, a crash's relaunch afresh, a tab aterm's own
    // end reopened), held at its prompt (`wait:typing`, a hold) or relaunched
    // and not found yet: no restart of the harness's, even for the TUI it was
    // filed for. Read as one, a person's `/exit` of their own Codex there
    // outranked their word, a hold and `[harness] relaunch = false`
    // (`OnExit::Restarted`), and the crashed TUI's line was typed back.
    assert!(!restarted(&opts, true, None), "no snapshot proves nothing");
    let person = tui.wrapping_add(1);
    assert!(!restarted(&opts, true, Some(person)), "a person's Codex");
    assert_eq!(
        carry_restart(&opts, true, Some(person)).step,
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
                assert!(!restarted(&opts, true, Some(tui)), "{cause} {phase:?}");
                assert_eq!(
                    carry_restart(&opts, true, Some(tui)).step,
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
        restarted(&opts, true, Some(tui)),
        "the relaunched TUI lives: the old one's exit, read late"
    );
    // Gone, a carry of the old TUI's exit made after it (the host's carry
    // tried again, or one another actor's adoption overtook) finds a restart
    // that LANDED: closed as the Codex lane's relaunch on exit closes it,
    // never `refused:no-restart-in-flight`, which the host says as a restart
    // that cannot land.
    let landed_gone = |st: &St| {
        save(&opts, &codex, st);
        let lines = ledger_lines(&opts);
        let r = carry_restart(&opts, true, Some(tui));
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
        !restarted(&opts, true, Some(tui)),
        "found and gone: the TUI that left was the relaunched one"
    );
    landed_gone(&relaunched(0, dead_pid()));
    save(&opts, &codex, &relaunched(4_000_002, 4_000_002));
    assert!(
        !restarted(&opts, true, Some(tui)),
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
        !restarted(&opts, true, Some(tui)),
        "adopted and suspended: theirs"
    );
    assert_eq!(
        carry_restart(&opts, true, Some(tui)).step,
        "refused:no-restart-in-flight"
    );
    // A new round forgets the old restart's TUI ([`St::rearm`]): a round whose
    // relaunched TUI was adopted, then stopped, is re-armed on the same record
    // once it has rested, and its next restart's relaunch is that restart's
    // again; otherwise the old adoption would read the new restart's exit as
    // the adopted TUI's own.
    let mut again = St {
        phase: Phase::Failed("no-resume".to_string()),
        ..relaunched(4_000_002, 4_000_002)
    };
    let _ = again.rearm(now);
    assert_eq!(
        (again.resumed_pid, again.relaunched_pid, again.exited_at),
        (0, 0, 0)
    );
    again.phase = Phase::Relaunched { at_s: now };
    save(&opts, &codex, &again);
    assert!(restarted(&opts, true, Some(tui)), "re-armed: {again:?}");
}

/// THE CARRY OF AN EXIT WHOSE MOVE LANDED MEANWHILE SAYS NOTHING: the host
/// read the agent's exit as the
/// harness's own restart's while that restart was in flight, and another
/// actor — a hand-run `aterm harness upgrade`, which carries its relaunch on
/// itself under the lock the host's carry waits on — finished the move
/// before the carry's next try. The carry found nothing in flight and said
/// `refused:no-restart-in-flight`, which the host badges "aterm ended the
/// agent … cannot bring it back; resume it by hand" over an agent that is
/// back. A record DONE for that very process, its conversation held by a
/// live process now, answers `landed`: the host's `Relaunched`, nothing
/// said, nothing written. NEGATIVE CONTROLS, today's word: the exit of
/// another process, and the same record with nothing holding its
/// conversation.
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
    let r = carry_restart(&opts, false, Some(old));
    assert_eq!((r.step.as_str(), r.session.as_str()), ("landed", SESSION));
    assert_eq!(outcome(&r.step), Outcome::Relaunched, "nothing said");
    assert_eq!(load(&opts, SESSION), Some(saved), "nothing written");
    assert_eq!(ledger_lines(&opts), 0);
    // NEGATIVE CONTROLS: an exit this move did not end, and the move whose
    // conversation nothing holds now.
    assert_eq!(
        carry_restart(&opts, false, Some(dead_pid())).step,
        "refused:no-restart-in-flight"
    );
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{me}.json")))
        .expect("remove the holder's session file");
    assert_eq!(
        carry_restart(&opts, false, Some(old)).step,
        "refused:no-restart-in-flight"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE CARRY OF A RESTART ALREADY ADOPTED ANSWERS AT ONCE: the step that
/// relaunched the agent found the new
/// process, waited for its first idle point and handed it to its loop; the
/// record stays in flight until that loop carries it on. The host's carry
/// of the old agent's exit went back through `await_new` — the harness's
/// hand on the tab again, the first idle point waited for again — and, past
/// `STALE_S`, stopped the record `no-resume`, a relaunch that "never
/// registered". While the adopted process holds the conversation the carry
/// says `adopted` with that process, dials nothing and writes nothing,
/// however old the relaunch. NEGATIVE CONTROLS: a relaunch not adopted yet,
/// and one whose adopted process holds the conversation no longer, are
/// carried as before (here, dialling a socket that is not there).
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
        let r = carry_restart(&opts, false, Some(old));
        assert_eq!((r.step.as_str(), r.pid), ("adopted", me), "{at_s}");
        assert_eq!(load(&opts, SESSION), Some(saved), "{at_s}: nothing written");
    }
    assert_eq!(ledger_lines(&opts), 0, "nothing said");
    save(&opts, SESSION, &relaunched(now, 0));
    assert_eq!(
        carry_restart(&opts, false, Some(old)).step,
        "wait:no-socket",
        "not adopted yet"
    );
    std::fs::remove_file(opts.home.join(format!(".claude/sessions/{me}.json")))
        .expect("remove the adopted process's session file");
    save(&opts, SESSION, &relaunched(now, me));
    assert_eq!(
        carry_restart(&opts, false, Some(old)).step,
        "wait:no-socket",
        "holding it no longer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE CARRY OF A RESTART WHOSE ADOPTED AGENT IS GONE CLOSES IT: the
/// relaunch was adopted
/// ([`St::relaunched_pid`]) and that process ended before its continuation.
/// No carry types the line again, and the carry of the old agent's exit went
/// back through `await_new`: the harness's hand on the tab 90 s at a time
/// for a process that was gone, `wait:resume`, until past `STALE_S` it
/// stopped the record `no-resume`, a relaunch that "never registered", which
/// the host says as a restart that cannot land. It is closed at once, as
/// `after_exit` closes one overtaken (no failure after the signalled agent's
/// exit), and answered `landed`; that exit is the restart's no longer.
/// NEGATIVE CONTROL: a dry run writes nothing.
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
    assert_eq!(carry_restart(&dry, false, Some(old)).step, "landed");
    assert_eq!(
        load(&opts, SESSION),
        Some(saved),
        "a dry run writes nothing"
    );
    let r = carry_restart(&opts, false, Some(old));
    assert_eq!((r.step.as_str(), r.session.as_str()), ("landed", SESSION));
    assert_eq!(outcome(&r.step), Outcome::Relaunched, "nothing said");
    let st = load(&opts, SESSION).expect("kept");
    assert_eq!(
        (st.phase, st.exited_at),
        (Phase::Failed("exited-before-continuing".to_string()), 0)
    );
    assert_eq!(ledger_lines(&opts), 1, "said once, in the ledger");
    assert!(
        !restarted(&opts, false, Some(old)),
        "the restart's no longer"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE AGENT OF A TAB ATERM'S OWN CRASH TOOK: aterm ended while
/// it ran, its shell's hangup reached it, and it removed its own record as
/// it went — which reads as a graceful exit, someone's decision. It was
/// nobody's: the next launch reopened its tab, and `after_host_ended`
/// relaunches it there on its conversation. A relaunch the crashed run had
/// in flight for its OLD tab (that tab is gone) is closed, not waited on.
/// Negative controls: the same removed record through `after_exit` (the
/// agent's own exit) is left as graceful; and the continuation says why.
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

/// Tier-1 of `HarnessExitRecord` (aterm-spec): every path of the model, from
/// its initial state to one where nothing more is enabled, replayed through
/// the real look ([`look_at_exit`], [`exit_record`]) and the real attempt
/// ([`after_exit`], a dry run) over a real record of a dead process. After each
/// action the state the real code leaves (the record on disk, what the look
/// kept, what the attempt decided) is the model's, and every invariant holds;
/// every action is driven. Negative controls: the buggy host's witness
/// `Crash`, `Look`, `Sweep`, `DecideAtAttempt` replayed through a read at the
/// attempt (`ExitRecord::Unread`) leaves the crash exactly as the buggy model
/// does, and `CrashIsRelaunched` catches it; and a graceful exit's own removal
/// landing inside the real look's settle is read as `OwnRemoval` then `Look`,
/// a state a `LookAtInstant` fix never reaches (its exit relaunched,
/// `GracefulIsLeft`).
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

/// The restart in place: the host restarts the tab's own agent (idle,
/// registered, its conversation on Claude's record) on the same program,
/// resuming its conversation, with nothing typed into it first. Dry runs:
/// decided against real processes and files, nothing typed or written.
/// Negative controls: a tab whose agent could not be read, another tab's
/// agent, no record of the process, a one-shot run (each a wait or a refusal,
/// never a signal); and a restart already in flight for the conversation is
/// carried on, never a second one started.
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
    // A bucket's fallback asks for its model on the line; the way back asks
    // for the bucket's, or (the notice named none) for the model the launch
    // named before the fallback replaced it (its record kept it), else for
    // none: the launch's `--model` dropped.
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

/// A `/model` choice survives a memory restart: a command-line `--model`
/// overrides the default Claude's `/model` saves, so the restart in place asks
/// for the model chosen by hand rather than the launch's flag. Negative
/// controls: nobody chose one (the launch's flags, no model asked for); a
/// launch that named no model (`/model` saved the choice as the default the
/// relaunch starts on, nothing asked for).
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

/// A choice before the process is not its choice: a stalled agent ended with
/// no record left (the stall's remedy relaunches it anyway) has no process
/// start to read, so the start is taken from the host's own snapshot, and a
/// choice a person made for an earlier process is not carried as this one's.
/// Negative control: the same choice answered after the process started is
/// carried.
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

/// The way back from a fallback outlives a memory restart: the model the launch
/// named before a bucket's fallback replaced it is carried on every record
/// while the fallback stands ([`St::fallback_from`]) and spent by the way
/// back, so a restart saved over the fallback's record does not leave the way
/// back with nothing to return to. Negative controls: with no fallback
/// standing the way back drops `--model`; a second fallback keeps the first's
/// origin; and a fallback's record an older build wrote (no origin field)
/// still returns to the launch model it kept.
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

/// The way back returns to what the person had, not to the notice's alias: a
/// bucket's notice almost always names its family ("You've reached your Fable
/// limit" → `fable`), but the conversation may have had an exact model before
/// the fallback (a `[1m]` window, a hand `/model` a relaunch carried). The
/// order is: a hand choice since the fallback, else the origin, else the
/// notice's alias. Negative control: no fallback on record and no hand choice:
/// the notice's alias.
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

/// A fallback from a launch that named no model is still a fallback: a
/// fallback standing is recorded apart from the model it replaced, so an empty
/// origin (no `--model`) is not read as "no fallback stands" and a second
/// fallback's way back returns to Claude's default, not to the first
/// fallback's model. Negative control: one fallback from no model: the way
/// back drops `--model`.
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

/// A `/model` since a fallback outranks the way back: a fallback onto Opus
/// from Fable, then a person's `/model` to Sonnet, then an exit relaunched
/// asking for Sonnet (the hand choice): the relaunch makes the hand choice the
/// origin, so at the bucket's reset the way back asks for Sonnet, what the
/// person has. Negative controls: the same exit with nobody's `/model` carries
/// the origin, and the way back returns to Fable; the memory restart's record
/// reads the same way; with no fallback standing, the hand choice makes none.
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

/// A `/model` before a fallback is what the way back returns to: launched
/// `--model claude-fable-5-1`, moved to Sonnet by hand, then a bucket's
/// fallback onto Opus: the fallback records the hand choice (Sonnet) as the
/// origin, not the launch's Fable. Negative controls: nobody's `/model` (the
/// launch's Fable); a fallback already standing keeps its origin over a hand
/// choice.
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
    let (sock, _) = instance_with(&dir, Answers::default());
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

/// The restart in place saves the fallback origin it decides, driven past its
/// signal to the record it keeps: a memory restart of a fallback that carries
/// a `/model` made since makes it the origin; a fallback made after a `/model`
/// records that. Each wire of [`restart_from`] into [`model_restart`] is read
/// off the saved record (the hand choice read for either restart, and the
/// origin written). Negative control: the same memory restart with nobody's
/// `/model` carries the fallback's own origin.
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

/// The design's own roster: `main` on this machine's login, `work-2` under
/// [`LOGIN_DIR`].
#[cfg(unix)]
fn two_accounts() -> crate::harness::accounts::Roster {
    crate::harness::accounts::Roster::parse(&format!(
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
         [[account]]\nlabel = \"work-2\"\nclass = \"dedicated\"\ndir = \"{LOGIN_DIR}\"\n"
    ))
    .expect("a roster")
}

/// EVERY RESTART IN PLACE RE-APPLIES THE LOGIN ITS PROCESS HAD, driven past
/// its signal to the record it keeps: a memory restart of an agent whose own
/// environment named a login types that login back, quoted, in front of the
/// survey switch; one whose variable was set and EMPTY (the default login,
/// named) types the empty value; and both keep it as a fact on the record
/// ([`St::login`]). NEGATIVE CONTROL: an agent that had no such variable —
/// every agent of a machine with no roster — gets the line it always got,
/// and a record with no `login` key at all.
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

/// AN ACCOUNT MOVE TYPES THE TARGET'S LOGIN, FROM THE ROSTER: driven past its
/// signal, a move to `work-2` types that row's directory verbatim whatever
/// the process had, a move back to `main` (the row with no directory) types
/// the EMPTY value over the directory the process ran under, and the record's
/// cause is `account:<label>` — the label, never the directory. Where a
/// bucket's fallback stands the move is the way back: the line asks for the
/// origin, and the origin is spent. NEGATIVE CONTROLS: a label the roster
/// does not list is refused before anything is signalled or saved, and so is
/// every account move asked of a host with no roster.
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
    // NEGATIVE CONTROLS: nothing signalled, nothing saved.
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

/// AN EXIT'S RELAUNCH COMES BACK ON THE LOGIN THE AGENT RAN UNDER, driven
/// (not dry) to the record it keeps: the snapshot read the login off the
/// process while it ran, and the line types it back — so a session moved to
/// another account stays on it through a crash. NEGATIVE CONTROL: an agent
/// that had no such variable is relaunched with the line it always got.
///
/// AND THE RELAUNCH AFTER ATERM ENDED ([`after_host_ended`], a cold restore's
/// step, `host`) types back the Claude home and the login the reopened
/// leaf names (2026-10-09: the layout carries them off the host's
/// snapshot), the home first — its crash's record filed under that home, as
/// its Claude Code files it.
#[cfg(unix)]
#[test]
fn an_exit_relaunch_types_back_the_login_the_agent_ran_under() {
    let case = |name: &str, home: bool, login: Option<&str>, host_ended: bool| {
        let _quick = QuickWaits::new();
        let mut a = DeadAgent::new(name);
        let (sock, _) = instance_with(&a.dir, Answers::default());
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

/// A transcript of [`SESSION`] with one prompt in it, filed in the Claude
/// Code directory `claude_dir` itself, project `project`.
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

/// AN AGENT STARTED UNDER ANOTHER CLAUDE HOME IS RELAUNCHED OFF ITS OWN
/// RECORDS (review of `fix/cold-restore-login-home`, 2026-10-09). Claude Code
/// started with `CLAUDE_CONFIG_DIR=<dir>` files `sessions/<pid>.json` and
/// `projects/` in `<dir>`, never in `~/.claude`
/// (`docs/DESIGN-aterm-wrapper-2026-09-17.md`, review round 2); the relaunch
/// after aterm itself ended ([`after_host_ended`], a cold restore's step)
/// read them under the user's home alone. Driven (not dry) to the record it
/// keeps, an agent whose snapshot names `<dir>`:
///
/// * whose crash left its record there, its snapshot naming no conversation
///   (all a read of `~/.claude` gave it), is relaunched on the record's
///   conversation — it ended `ended:no-conversation`, its tab never brought
///   back;
/// * whose transcript is there, the user's `~/.claude/projects` readable and
///   without it (any machine that has run Claude Code under the default
///   home), is RESUMED — it started afresh (`exit-fresh`, no `--resume`);
///
/// each line carrying the home. CONTROLS: the same agent whose own projects
/// are readable and hold no transcript of it starts afresh though the user's
/// `~/.claude` holds one of the same id (`--resume` under `<dir>` finds only
/// `<dir>`'s); and an agent with no home is relaunched off `~/.claude`
/// exactly as before.
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
        let (sock, _) = instance_with(&a.dir, Answers::default());
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
    // A record an older build wrote has no such key, and reads as none: its
    // relaunch looks in `~/.claude`, as every one did.
    let old = St::from_json(r#"{"phase":"relaunched","at":7,"cause":"host"}"#);
    assert_eq!(old.map(|st| st.home), Some(None));
}

/// THE HOST'S LOOKS READ THE AGENT'S OWN RECORD ([`look_at_exit`],
/// [`follow`]), under the Claude home its snapshot names (review of
/// `fix/cold-restore-login-home`, 2026-10-09): read in the user's
/// `~/.claude`, the crash of an agent started under another home read as a
/// graceful exit (its record nowhere there: `Removed`), never relaunched;
/// and a `/clear` it made while it ran was never followed, so its relaunch
/// would resume the conversation it had left.
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

/// AN EXIT'S RELAUNCH SAVES THE FALLBACK ORIGIN IT DECIDES, driven (not
/// dry) to the record it keeps: a crashed agent of a standing fallback,
/// moved to Opus by hand since, is relaunched asking for Opus with Opus as
/// the origin — the wire of [`after_exit`] into [`exit_fallback`], the hand
/// choice with it, read off the saved record. NEGATIVE CONTROL: nobody's
/// `/model` — the fallback's own origin carried. The two run side by side:
/// each waits out the stand-in shell's prompt that never comes back.
#[cfg(unix)]
#[test]
fn an_exit_relaunch_saves_the_fallback_origin_it_decides() {
    let origin = "claude-fable-5-1";
    let fallback = "claude-sonnet-5-5";
    let hand = "claude-opus-5-5";
    let case = |name: &str, chose: Option<&str>, want: &str| {
        let _quick = QuickWaits::new();
        let mut a = DeadAgent::new(name);
        let (sock, _) = instance_with(&a.dir, Answers::default());
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

/// A RELAUNCH LEFT IN FLIGHT FINDS ITS PROCESS UNDER THE CLAUDE HOME ITS
/// LINE CARRIED ([`St::home`], [`St::records`]; review of
/// `fix/cold-restore-login-home`, 2026-10-09): the relaunched Claude Code
/// files its record there, so the step that adopts it (a cold restore's,
/// `host`) and the carry-on at its loop's idle point ([`resume`]) read there.
/// Read in the user's `~/.claude`, a relaunched agent under another home was
/// never found — `wait:resume` at every step, then `failed:no-resume` past
/// the stale bound, its continuation never typed. NEGATIVE CONTROL: a record
/// with no home finds its process in `~/.claude`, as ever.
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
    // Its loop's idle point carries it on ([`resume`]): found again, read
    // idle and the conversation's one live owner there, the carry goes on to
    // the tab's own claim — which the stand-in, on no PTY of the tab's, does
    // not hold. Read in `~/.claude`, it waited the whole settle for a record
    // that never came and said the conversation had moved.
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

/// A relaunch handed back: where a supervisor loop takes the relaunched agent
/// (`hand_back`), the step ends as soon as the relaunch holds the conversation
/// (`adopted`, its pid, the record still in flight and owed to its tab), and
/// [`resume`], at the loop's idle point, carries it on. A typed continuation
/// whose model is still to be read is owed too, and once read is owed no
/// more. Negative controls: without `hand_back` the same find goes on to the
/// carry-on; another tab owes nothing; a record whose model was read owes
/// nothing.
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

/// ONE MOVE, ONE `adopted` ROW, IN ITS MOVE'S WORDS (a quiet upgrade's
/// ledger said `adopted` twice in one second,
/// each promising a continuation the same move's `done:resumed` then said was
/// never typed; the second writer, the same pass's orphan pass, is fixed at
/// its cause in
/// `the_orphan_pass_leaves_a_restart_relaunched_since_its_list_was_read`). An
/// adopted restart stays in flight until its loop's `resume`, and any step
/// that finds it meanwhile — the host's carry of the old agent's exit — reads
/// the record from disk, as the second actor here does. NEGATIVE CONTROLS: a
/// new move's relaunched process, found with the last move's adoption still
/// on the record, writes its own row; and a move that owes a continuation
/// says its supervisor types it.
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
    // One step over the record ON DISK, finding `pid` as the relaunch.
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
    // NEGATIVE CONTROL: the next move — an ask-flow round, relaunched
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

/// THE ORPHAN PASS LEAVES A RESTART RELAUNCHED SINCE ITS LIST WAS READ: the
/// pass read the session files
/// while the old agent held the conversation, its visit ended that agent and
/// relaunched it, and the new process registered after the list. Judged from
/// the list, the restart read as orphaned and the same pass carried it again
/// — the harness's hand on the tab a second time and the second `adopted` row
/// of that test. Now it is `visit`'s, or its loop's `resume`: nothing is
/// dialled, said or written. NEGATIVE CONTROL: once the relaunch holds
/// nothing, the orphan pass carries it on (here, dialling a socket that is
/// not there).
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
    // NEGATIVE CONTROL: the relaunched process gone, and its file with it.
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

/// THE SURVEY EXPORT NEVER REFUSES A LAUNCH: a Claude launch whose relaunch line fits
/// the kernel's bound only without `CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1 ` (38 bytes on
/// a bound of 1000) is relaunched without it. NEGATIVE CONTROL: a launch that does not
/// fit even without it is still refused, and one that fits with it carries it.
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

/// A restart relaunched on 2.1.282 from 2.1.281 over a stand-in agent in
/// [`TAB`], at the moment its new process holds the conversation: the state as
/// [`relaunch`] leaves it, on disk, with what the restart recorded of the
/// transcript (through the sweep's own read, [`tail_to_end`] and
/// [`model_and_mark`]) when it was `before`. Dropped, it stops the agent and
/// removes its directory.
#[cfg(unix)]
struct Rig {
    dir: ScratchDir,
    opts: Opts,
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
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

    /// Every line typed into the tab.
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

/// The model, before and after: the continuation names the model the agent's
/// READY answer ran, and the resumed session's first answer past the
/// restart's mark (Claude's own `<synthetic>` row skipped) is the model after,
/// read by the step after the one that typed the continuation. A change is
/// words only: the step is `done`.
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

/// A relaunch after aterm itself ended (`CAUSE_HOST`) types the relaunch's own
/// words once its agent holds its conversation again, not the upgrade's
/// (`Upgraded: this session was restarted on Claude Code …`), which would be
/// false for an agent nobody upgraded. Negative control: the upgrade's own
/// record (no cause) still types the upgrade's words, through the same
/// carry-on.
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

/// The relaunch's production waits (15 s for a shell's prompt, 5 s of a shell
/// holding its tab before the agent reads as ended, a look a second) cut to a
/// fraction of a second for THIS thread, until dropped ([`WaitsInTest`]): a
/// stand-in shell that never takes its terminal back, or holds it throughout, is
/// waited out in well under a second. Nothing here is a latency budget: no
/// assertion bounds from above how long a step took.
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
/// its terminal back; nothing else about any process can be read.
#[cfg(unix)]
struct ShellHolds(Option<bool>);

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
        self.0
    }
}

/// A relaunched agent that ended as it started (Claude's `No conversation
/// found`, a crash at start) leaves its shell holding the tab again at once,
/// and is said at once: `failed:no-resume`, stopped, one ledger line, in
/// seconds, not after a long wait with the harness's hand on the tab.
/// (Negative controls, where no 90 s wait is paid:
/// `relaunch::tests::a_shell_that_has_the_tab_back_reads_as_ended_only_after_a_while`.)
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
    let r = await_new(&opts, r, &mut st, &mut c, SESSION, &ShellHolds(Some(true)));
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
/// now, its shell's hold on the tab read as `holds` ([`ShellHolds`]), and
/// the wait for the relaunched process cut to `wait` (a step that must not
/// end early is waited out): the step, the record after it, its ledger rows.
#[cfg(unix)]
fn held_step(name: &str, cause: &str, holds: Option<bool>, wait: Duration) -> (Report, St, usize) {
    let dir = scratch(name);
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
        cause: cause.to_string(),
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
    let r = await_new(&opts, r, &mut st, &mut c, SESSION, &ShellHolds(holds));
    HELD_WAIT_IN_TEST.with(|w| w.set(None));
    let rows = ledger_lines(&opts);
    let _ = std::fs::remove_dir_all(&dir);
    (r, st, rows)
}

/// A wait long enough that a shell read as holding the tab throughout would
/// have been read as ENDED ([`QuickWaits::ENDED`] and several looks more): the
/// negative controls wait it out. A positive control, which ends at once, passes
/// [`HANG_DETECTOR`] instead, so a loaded machine cannot cut it short.
#[cfg(unix)]
const PAST_ENDED: Duration = Duration::from_millis(1_500);

/// The wait of a step that must END early: only a hang waits it out.
#[cfg(unix)]
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// Negative controls of the early verdict, through the step itself: a restored tab's relaunched agent that is still starting
/// — something other than its shell holds the tab — or whose shell cannot be
/// read is WAITED ON past [`QuickWaits::ENDED`]: `wait:resume`, the record still
/// in flight, nothing stopped and nothing ledgered. Wired to a verdict that
/// ignores the kernel's answer, both would fail as `failed:no-resume`.
#[cfg(unix)]
#[test]
fn a_restored_relaunch_still_starting_or_unread_is_waited_on_not_failed() {
    // Each case waits PAST_ENDED out on a step of its own: side by side.
    side_by_side(
        [(Some(false), "held-starting"), (None, "held-unread")]
            .into_iter()
            .map(|(holds, name)| -> Box<dyn FnOnce() + Send> {
                Box::new(move || {
                    let (r, st, rows) = held_step(name, CAUSE_HOST, holds, PAST_ENDED);
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

/// The early verdict is a restored tab's relaunch's alone. The live relaunch
/// on exit ([`CAUSE_EXIT`]) keeps its wait even with its shell holding the tab
/// throughout: its loop retries `failed:no-resume` after its back-off by typing
/// the relaunch line afresh, so an early verdict there would retype a refusing
/// agent within seconds. Negative control: the same step as a restored tab's
/// is said as ended.
#[cfg(unix)]
#[test]
fn a_live_relaunch_on_exit_keeps_its_wait_when_its_shell_has_the_tab() {
    let (r, st, rows) = held_step("held-live", CAUSE_EXIT, Some(true), PAST_ENDED);
    assert_eq!(r.step, "wait:resume", "{r:?}");
    assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
    assert_eq!(rows, 0);
    let (r, st, rows) = held_step("held-restored", CAUSE_HOST, Some(true), HANG_DETECTOR);
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(rows, 1);
}

/// The kernel's own answer ([`Kernel::shell_holds_tab`]'s default, which
/// [`Live`] uses) on real processes: a shell that leads its own session on a
/// terminal and is its foreground holds the tab; a job in a group of its own
/// that is not the terminal's foreground, or a process in another's group,
/// does not; no shell recorded (pid 0) or a gone one cannot be read.
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

/// Unconfirmed: a resumed session that has not answered by the first sweep
/// past the bound is said to be, the one outcome whose step says so. The turns
/// before the mark name a model: read from the start, they would have
/// "confirmed" it; the mark keeps the old process's model from standing in for
/// the new one's.
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

/// The model before is read from the tail that proved the READY answer, at the
/// restart itself: a sweep that reaches the restart records the model the
/// answer ran and where that read ended, even when its last look then waits
/// (the next restart takes both again). The READY answer is not the last row
/// here: Claude's own `<synthetic>` row follows it and names no model.
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

/// The visit does not wait out the answer: a sweep runs its visits one after
/// another under one lock, and the orphan pass that relaunches a restart left
/// exiting runs after all of them, so every second one visit blocks ages that
/// restart toward [`STALE_S`], past which the agent is never relaunched. The
/// resumed session's first answer can take the whole bound (a long first
/// thought, retries, a usage limit that writes only `<synthetic>` rows), so
/// the visit that types the continuation must not wait for it.
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
    // Once it is said, the next build is upgraded to as ever.
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

/// The same refusal with a model due, which the test above cannot see because
/// it steps with [`ModelCtx::none`].
///
/// A warm conversation (answered moments ago) on `claude-opus-5`, a newer build
/// installed, and `claude-opus-5-5` the newest Opus on offer: the model ladder
/// sends the model along with the build restart (`build-restart`), so
/// `model_to` is non-empty on every pass. The agent is a launcher script's
/// child, which the upgrade must refuse once: the reuse guard must match the
/// Failed state despite the non-empty `model_to`, so the refusal is said and
/// ledgered only once. The negative control is the first pass: it must still
/// refuse, so a pass is never one that simply stopped looking.
#[cfg(unix)]
#[test]
fn a_refusal_with_a_model_due_is_said_once_not_every_pass() {
    let dir = scratch("job-model");
    let opts = drive(&dir);
    let mut agent = parked().spawn().expect("spawn");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    // A WARM transcript: the conversation answered on claude-opus-5 just now.
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
        native: None,
    };
    // Precondition, so the pass below is about THIS path: the move is due,
    // the cache is warm, and the ladder takes it because a build restarts.
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

/// THE RIDE RE-JUDGED AT EVERY VISIT: an announced round carries
/// `claude-opus-5-5`, and the conversation's model record says it was asked
/// for once and did not take, so the ride must not ask for it again. With a newer build
/// to move to, the visit drops the model from the round, said once
/// (`model-ride-dropped`), the round itself KEPT (its salt, its phase, its
/// notice); a later visit says nothing more. With the build current, the round
/// had only the model to move: the agent is released and the round is pending
/// again. And where the same visit's rule moves the conversation to ANOTHER
/// model (a Fable its family's step offers, the carried Opus off the list from
/// it), the retarget abandons the round — a new consent — and the row
/// says so, never "kept". NEGATIVE CONTROL: the same round
/// over a record that never failed keeps its model, and nothing is said.
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

    // NEGATIVE CONTROL: nothing recorded against the model — it rides.
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

/// THE PLAN CARRIES THE CLAUDE HOME ITS AGENT RAN UNDER: `relaunch::plan`,
/// the one place every relaunch line is made (the explicit restart's, the
/// relaunch on exit, the restart in place, the upgrade's), types the
/// `CLAUDE_CONFIG_DIR` its process had in front of the program, and with a
/// login beside it both. Before 2026-10-09 the plan typed no home: the
/// real-claude lab's Claude Code, started as `CLAUDE_CONFIG_DIR=<home>/.claude
/// claude`, came back from `aterm ctl restart` in the first-run theme picker
/// (it read `<home>/.claude.json`), its conversation never held. NEGATIVE
/// CONTROL: no home, the planned line is byte for byte the plain one.
#[test]
fn the_planned_relaunch_line_carries_the_claude_home() {
    let dir = scratch("line-home");
    let opts = drive(&dir);
    let argv = vec!["claude".to_string(), "--verbose".to_string()];
    let planned = |home: Option<&str>, login: Option<&str>| {
        let launch = crate::harness::relaunch::Launch {
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
    // NEGATIVE CONTROL: the process had no home of its own.
    assert!(!plain.contains("CLAUDE_CONFIG_DIR"), "{plain}");
    assert!(
        plain.contains(&format!("'/opt/claude' '--verbose' '--resume' '{SESSION}'")),
        "{plain}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A FAILED `--model` NEVER RIDES THE LINE: a process this
/// harness relaunched with `--model claude-opus-5-5`, an ask the record has
/// since marked FAILED, is relaunched by a site asking no model of its own
/// without any `--model` (the saved default), and records none as kept. A
/// person's own flag — an id no ask of the record names, or an alias — and a
/// site's own model stand. NEGATIVE CONTROL: the same launch over a record
/// with no failure keeps its `--model` verbatim.
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
    // The relaunch as planned — `relaunch::plan`, the one place every
    // relaunch line is made, which asks the rule — on a zsh the table names.
    let planned = || {
        let launch = crate::harness::relaunch::Launch {
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
    // NEGATIVE CONTROL: nothing failed — the flag is kept.
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
    // A site's own model replaces it as ever.
    assert!(line(&ours, Some("claude-fable-5-1")).contains("'--model' 'claude-fable-5-1'"));
    // A person's own id or alias, which no ask on the record names, stands.
    for theirs in ["claude-fable-5-1", "opus"] {
        let argv = argv(theirs);
        assert!(line(&argv, None).contains(&format!("'--model' '{theirs}'")));
        assert_eq!(launch_kept(&opts, SESSION, &argv).as_deref(), Some(theirs));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A DUE MODEL RIDES EVERY RESTART (the model ladder wired into ours): a
/// relaunch on exit and the restart in place happen regardless, so a move
/// that is due goes with them even on a WARM conversation (`build-restart`),
/// and is recorded as asked for. NEGATIVE CONTROL: with nothing due — no
/// model half — no model rides, and nothing is recorded.
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

/// The due clock survives a flicker, and only a definite "nothing due" clears
/// it. A visit that cannot read the live model (a transcript tail with no
/// answer in it, as a long tool result can leave for many visits) answers
/// `model-unknown`; clearing the clock there would restart the one-hour bound
/// every such visit, and an active session's move might never land.
/// Conversely the early return (no model can run, none pending) is "nothing
/// due" and must clear it, or a move that returns later inherits a stale start
/// and skips its warm-cache grace.
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

    // A visit that cannot read the live model: the clock must STAND.
    std::fs::write(&path, "{\"type\":\"user\"}\n").expect("tail with no answer");
    let unknown = model_read(&opts, &sf, None, &mctx);
    assert_eq!(unknown.verdict, ModelVerdict::Keep("model-unknown"));
    let kept = load_model_record(&opts, SESSION);
    assert_eq!(
        (kept.due_to.as_str(), kept.due_since),
        ("claude-opus-5-5", rec.due_since),
        "an unknown read reset the due clock"
    );

    // Readable again: the wait RESUMES where it was, not from zero.
    std::fs::write(&path, &answered).expect("transcript");
    let back = model_read(&opts, &sf, None, &mctx);
    assert!(
        back.due_for_s >= 3000,
        "the bound restarted: {}",
        back.due_for_s
    );

    // A start AHEAD of now (the wall clock stepped back): restarted at now,
    // never left to read as 0 s due for the whole skew.
    let mut ahead = load_model_record(&opts, SESSION);
    ahead.due_since = now_s() + 10_000;
    save_model_record(&opts, SESSION, &ahead);
    let _ = model_read(&opts, &sf, None, &mctx);
    assert!(
        load_model_record(&opts, SESSION).due_since <= now_s(),
        "a future-dated start survived and would postpone the bound"
    );

    // Nothing can run and nothing is pending: THAT clears it.
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

/// An ask that ran stands. The harness asked for `claude-opus-5-5`, the
/// relaunch ran it, and a visit recorded it applied. The conversation now runs
/// another model (a person moved off it), and a visit comes after
/// `MODEL_SETTLE_S`: the ask that ran is not recorded failed, which would be a
/// false `model-failed` after its own `model-verified` and bar the model from
/// this conversation for good. Negative control: an ask that never ran is
/// still recorded failed.
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

/// A session that runs the native build is judged by the native build's own
/// catalog and offer, not the managed one's: the same conversation on
/// `claude-opus-5` is due for Opus 5.5 (its family's newest) under the managed
/// view, and, the native build not offering Opus 5.5 yet, for the list's Fable
/// 5.1 under the native one. With no native build installed, a native session
/// falls back to the managed view rather than to nothing.
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

/// The pre-filter never restarts a native session's warm wait. A native
/// session on `claude-opus-5`, warm: the managed build offers only
/// `claude-opus-5` (the session runs it), the native build offers
/// `claude-opus-5-5` (due), and the move has been due for 50 minutes. Every
/// sweep runs the host's pre-filter before the visit; were it to write what it
/// read under both views, the managed `model-current` would clear the due
/// clock and the native `Due` restart it, so `warm-wait-over` would never
/// fire. Negative control: where the views agree the pre-filter's settle step
/// is still kept: the ask that runs is recorded applied.
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

/// A crashed native session's riding model is the native build's. On the
/// relaunch-on-exit path the session file's pid has exited, so `exe_of` reads
/// nothing; the view is then the program the crash ran (`argv[0]`), which the
/// relaunch runs again. Read off the dead pid alone it would be the managed
/// view, which here offers only the model the session already runs, while the
/// native build offers `claude-opus-5-5`. Negative control: a managed program
/// on the same dead pid is judged by the managed view.
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
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
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

/// The presence guard, one presence fact: a tab is attended when a person gave
/// this session input within ten minutes, by the server's per-session stamp on
/// the very read the sweep judges (`"human_ms"`, `status human_ms=`), the one
/// the supervisor's question policy reads; not the tab's placement or the
/// machine's HID clock. The wait is said once (`held-back:attended`, a ledger
/// row the host logs), then `wait:attended`. A host that sends no stamp proves
/// no absence: it fails closed. `--now` types into it all the same. This rig
/// is a hand-run sweep (no window, no clock injected) and reads the same fact
/// the window does. Negative controls: the person gone eleven minutes, and
/// nobody ever (`null`), each announce once settled, the tab's placement (in
/// front of the focused window) notwithstanding.
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

/// The notice is pressed, not pasted: Claude Code wraps a long paste in
/// `<pasted_content>` and tells the model it may hold instructions its person
/// did not write, so the model may decline to act on it. Where the host's
/// `turn` takes `cap=` (its help names `cap=<n>`), the notice is typed
/// `typed=1 cadence=2 cap=4096`, key by key, whole. Negative control: a host
/// whose help does not name it gets the plain paste, never a `cap=` it would
/// type as text. Either way the fence and the yield stay.
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

/// A PRESSED NOTICE LEAVES CLAUDE CODE'S SURVEY TO THE PERSON (measured: a
/// one-key card over the composer answers from the composer's
/// value, so no key of the notice answers it — but its first key closes the
/// card unanswered). With the session survey open above an empty composer,
/// nothing is typed: the notice is refused `survey`, to be typed at the next
/// quiet moment. NEGATIVE CONTROL: the same screen with no card presses it.
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

/// ONLY THE AGENT'S OWN WORDS ARE PRESSED: the relaunch line at zsh and fish
/// is pasted whole (a person's Enter pressed into a line being typed key by
/// key would run the part typed, a bare `claude`, and send the rest of the
/// keys into it), and so is the continuation. Against a host whose `help turn`
/// names `cap=<n>`, both go as ONE fenced paste — no `typed=1`. NEGATIVE
/// CONTROL: the notice's own turn on the same host is pressed.
#[cfg(unix)]
#[test]
fn only_the_agents_own_words_are_pressed_where_the_host_takes_cap() {
    const CAP_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                            [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
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

/// A PRESSED TEXT'S TURN BUDGET COVERS ITS KEYS AND ITS DIGITS: the host
/// holds each digit of a pressed text for the previous key's echo (the caret
/// anchor, `TypingHold`) — about one Claude Code render — and a notice
/// carries dozens of digits and up to ~2000 keys, more than [`TURN_WAIT`]'s
/// 10 s: past its budget the host cuts the text part-way (`reason=deadline`),
/// its words standing in the person's composer. The
/// REAL notice, naming five processes, is asked a timeout covering its keys
/// at the notice's cadence and its digits' renders on top of TURN_WAIT's
/// own. NEGATIVE CONTROL: a paste on the same host — the relaunch line, the
/// continuation — still waits TURN_WAIT exactly.
#[cfg(unix)]
#[test]
fn a_pressed_notices_turn_budget_covers_its_keys_and_its_digits() {
    const CAP_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [cadence=<ms>] [cap=<n>] \
                            [yield=<floor>] [if-gen=<epoch>.<seq>] ...";
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

/// A NOTICE THE HOST CUT SHORT IS NOT DELIVERED: the host stops a pressed text part-way — a person's
/// key or a screen switch between its keys, or the turn's deadline — and
/// presses no Enter, answering `skipped reason=<why> typed=<k>`. That is
/// [`Typed::Cut`]: never `announced` (the agent has half a notice, without
/// the READY sentence at its end), and never typed again within the visit —
/// the part typed sits in the composer, and a person's keys may sit with
/// it. A skip that typed nothing keeps its word: `changed` is tried again, a
/// person's hand or a spent budget before the first key is a wait
/// (`yield`). Negative controls: the fence's `changed` is tried
/// [`NOTICE_FENCE_TRIES`] times, and a whole pressed notice is announced.
#[cfg(unix)]
#[test]
fn a_notice_the_host_cut_short_is_never_announced_or_typed_again() {
    let cut = "OK 0 turn skipped reason=person typed=40 pasted=0 submitted=0 seq=78 id=1";
    for (head, want) in [
        (cut, Err(Typed::Cut("person".to_string()))),
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

/// A labelled top rule is the input box too: Claude Code writes the effort
/// tag, or `sandboxed`, into the box's top rule, which an all-`─` test would
/// refuse, falling back to the whole-screen generation. The box is read by
/// `aterm_phase`'s one frame reader: the recorded screens (`wait_bg.out`, the
/// footer fixture) fence on their three rows, label and all, and the `turn` a
/// labelled box is typed with carries `if-rows=` and no `if-gen=`. Negative
/// control: the all-`─` test refuses both recorded top rules.
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
    const ROWS_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] \
                             [if-rows=<first>-<last>:<hex16>] ...";
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

/// A pressed notice's Enter is guarded on its own last row: a box Claude Code
/// draws over its input on the same screen (a turn resumed at a workflow's
/// break, its first tool call asking) moves neither the person's-key nor the
/// screen-switch hold, so a plain Enter would confirm the box's focused `1.
/// Yes`. Where the host takes it (`help turn` names `guarded:`), the notice's
/// and the release's turn carries `submit=guarded:` with the supervisor's own
/// composer guard: the cursor's row must still be the composer's, ending in
/// the text's last word. On the host's own engine
/// (`aterm_observe::row_matcher`) it matches the last row of the real notice
/// and release at every width, and no row of the box. A miss (`reason=guard`,
/// no `typed=`) is a cut (the text typed whole and unsent), never announced
/// and never read as a refusal. Negative controls: a host whose help does not
/// name the option gets the plain Enter (never a `submit=` it would type as
/// text), and the guard over the box matches nothing there.
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
    let release = upgrade::release_prompt(upgrade::Agent::Claude);
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

/// A Claude Code prompt for [`left_standing`] alone, 40 columns wide, holding
/// `draft` ([`Prompt::screen`]): `text --json` draws it, a `cell` reads plain,
/// `help` answers `help`, and `key … ctrl+u` (only while its `if-gen=` names
/// the screen drawn) takes back what `kill` says, so a lane that pressed one
/// would be seen to. Returns every request it read, and the draft it was left
/// holding.
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

/// Aterm never takes its own words back: a `ctrl+u` fenced on the read cannot
/// see a person's key the host has counted but Claude Code has not drawn, so it
/// could erase the person's words with aterm's. What a stopped notice or
/// release left in the prompt is only read ([`left_standing`]): aterm's part
/// alone is `unsent`, with a person's key after it the person's (`mixed`), a
/// person's own draft or an empty prompt means aterm's is `Gone`, and in every
/// case nothing is pressed.
#[cfg(unix)]
#[test]
fn aterms_own_words_are_read_never_taken_back() {
    let release = upgrade::release_prompt(upgrade::Agent::Claude);
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

/// A notice the host stopped stands unsent, never a person's draft, and is
/// never pressed back: the part a cut left in the prompt (its deadline, a
/// screen switch, a guard's miss that left it whole, or a person's gesture) is
/// recorded (`St::left_typed`) and every later look reads it first, before any
/// gate, as aterm's own (`wait:left-typed:unsent`), never `draft`, so the
/// supervisor's draft rule does not send it; nothing is pressed to take it
/// back. With a person's key among aterm's words it is theirs
/// (`left-typed:mixed`). Negative control: the same cut with the record
/// forgotten reads aterm's words as a person's draft (`wait:draft`).
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
    // Negative control: the record forgotten, as before — a person's draft.
    let h = rig("cut-forgotten", person, false);
    let _ = h.visit();
    let mut st = load(&h.opts, SESSION).expect("the record");
    st.left_typed.clear();
    save(&h.opts, SESSION, &st);
    assert_eq!(h.visit().step, "wait:draft");
    assert_eq!(keys(&h), 0);
}

/// A TURN BEGUN SINCE THE LOOK TAKES NO NOTICE AND NO RELEASE UNDER THE ROWS
/// FENCE: where the host takes `turn if-rows=`, the notice
/// and the release are fenced on the input box's three rows alone
/// ([`claude_fence`]), so the spinner and the `esc to interrupt` of a turn
/// that started between the look and the fence's fresh read — a person's
/// Enter, a background task's end that woke the agent — sit outside the
/// fence. The fresh read the fence is taken on is held to the look's own
/// turn fact ([`busy_footer`]): the notice waits `busy`, the
/// release `release:busy`, nothing typed, the round as it was. NEGATIVE
/// CONTROLS: the same visits over a screen that stayed idle type the notice
/// and the release.
#[cfg(unix)]
#[test]
fn a_turn_begun_since_the_look_takes_no_notice_and_no_release_under_the_rows_fence() {
    const ROWS_HELP: &str = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] \
                             [if-rows=<first>-<last>:<hex16>] ...";
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
    // THE NOTICE (the ask flow's): typed over a screen that stayed idle, on
    // the box's rows.
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
    // THE RELEASE a give-up owes, under the same fence.
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

/// The check-then-type: the screen read, the process proofs and the paste are
/// separate steps, and a keystroke between them must not be typed in front of
/// the notice. Where the host's `turn` takes the fence (`help turn` names
/// `if-gen=`), the notice is typed with `if-gen=<the fresh read's generation>
/// yield=0.2`; `skipped reason=changed` typed nothing. The visit reads and
/// fences it again, [`NOTICE_FENCE_TRIES`] times in all: a moment's repaint is
/// typed at the next try, and one that outlasts them is a wait with the
/// upgrade still pending, counted (`St::refused`, from the first refusal). A
/// person typing through the park (`ERR yield timeout`) is never typed at
/// again. Negative control: a host without the fence gets the plain turn,
/// never an `if-gen=` it would type as text.
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
    // How many screen reads come before the first paste: the look's and the
    // first fence's. A later screen after them is what a RETRY reads.
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
    // A RETRY TYPES ONLY WHERE THE LOOK'S GATES STILL HOLD: the screen that moved was a person's keys or the program's
    // own output. A person gave the tab input since the look (a message
    // submitted between the look and the paste): the refusal stands. The
    // agent woke into a turn (its spinner and busy footer): the fence's read
    // shows the turn, which the gate waits for (`wait:busy`: no refusal of
    // aterm's). Either way nothing is typed again — never
    // a notice queued behind the person's message or into the live turn.
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

/// THE NOTICE IS TYPED UNDER THE HARNESS'S OWN HAND, AND A REFUSED FENCE IS
/// TRIED AGAIN INSIDE THE VISIT: a bare `turn` reads to the window's presence
/// layer as somebody driving the tab — a row born and folded around it, two
/// PTY resizes that can leave the grid displaced. Under the hand (`lease acquire …
/// hard`, [`Hand`]) aterm's own turn shows no row; the fence, a whole-screen
/// generation that a footer ticking once a second moves about one try in
/// ninety, is tried again on a fresh read ([`NOTICE_FENCE_TRIES`]) instead of a
/// minute later. The order is the hand taken, every try, the hand given back
/// — on the refusal too. NEGATIVE CONTROLS: a tab another driver holds is a
/// wait (`held`) and nothing is typed; a server that takes no hard lease
/// gives no hand, and the notice is typed as it always was; a fence
/// that keeps refusing is a wait after [`NOTICE_FENCE_TRIES`].
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
    // NEGATIVE CONTROL: no hard lease to take (an older server).
    let h = rig(
        "hand-older",
        "ERR lease acquire: unknown arg 'hard' (ttl=<ms> holder=<name>)",
        vec![taken],
    );
    assert_eq!(h.visit().step, "announced:1");
    assert_eq!(order(&requests(&h)), ["acquire", "turn"]);
}

/// THE WAIT LOG: the ledger holds acts, so a long wait's visit-by-visit
/// history lives here. Each CHANGE of wait, and each
/// act, is one row of the record: when, the word, and the facts the look
/// decided on. The same word again adds nothing (a wait of hours is one row),
/// the log is bounded to [`WAITS_KEPT`], and it survives the state file.
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

/// THE WAIT LOG THROUGH THE REAL VISIT: a tab with text typed into its
/// composer waits `draft`, and the row says what the look read — the person
/// stamp, the composer's verdict and where the cursor and the caret stood,
/// the render state, what ran under the agent.
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

/// THE OWNER'S HOLD OWNS NO TURN END: an upgrade `--skip`ped onto its
/// target, or `--defer`red past now, waits `skipped`/`deferred` and takes
/// nothing, and no idle point cures that — a skip is the build's last word
/// (the worker looks again only at the next activation notice or the owner's
/// next word), a deferral is looked at again once it may have run out, and
/// neither is a wait the worker owns the session's turn ends for, so its
/// supervisor continues the worker as ever. Negative controls: the same busy
/// session with no word, `--now`, a deferral that has run out, a skip of
/// another build, and a word the owner put on ANOTHER tab each wait on the
/// turn alone.
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

/// `<exe> --version` is bounded while the sweep holds its lock: the whole
/// probe, the child's exit and the read of its output. A candidate that never
/// answers is killed with its group, and one that answers and exits but leaves
/// a grandchild holding the pipe is cut off at the bound too, its group
/// killed. Either is `TimedOut`. Negative controls: one that answers is read;
/// one that exits without a version is `NoVersion`.
#[cfg(unix)]
#[test]
fn a_version_probe_is_bounded_whole() {
    let dir = scratch("version-probe");
    // The sleeps' lengths carry this process's pid as their fraction, so the
    // `ps` sweep below finds ONLY this test's own processes: a fixed number
    // matched any other run's (a concurrent gate, a killed run's orphan that
    // outlives it for days) and failed this test for a process that was never
    // ours. A fraction, because macOS `sleep` refuses a number past about
    // 4e9 seconds and exits at once, which would end `hang` before its bound.
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

/// The version cache: a clean answer (a version, or a clean failure to name
/// one) is remembered while the file is unchanged, but a timeout is not a
/// verdict on the build (one slow probe on a loaded machine must not stop that
/// build being probed again). It is asked again on a back-off that doubles
/// (unanswered until then, which a look reads as nothing decided,
/// [`UNREAD_VERSION`]), and a changed file is asked at once. Driven on an
/// injected clock.
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

/// The last look before the one irreversible act: a draft a person began after
/// the restart's own screen check (while the state was being written) is read
/// again on the same connection immediately before the SIGTERM, and vetoes it
/// (`wait:changed-before-signal`, the agent alive and the upgrade still
/// announced). Negative control: the same draft already on the restart's first
/// look stops it there (`wait:changed`).
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

/// The continuation on a screen that never holds still: a resumed session
/// whose screen keeps redrawing moves under every fenced try, so after its
/// fenced tries the continuation is typed unfenced, once more checked for an
/// empty composer, rather than ending `done:no-continue` with the agent never
/// told to go on. Negative control: a host that refuses the turn is not typed
/// at again.
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

/// The relaunch line and a person typing through the yield: the fenced line
/// parks on the typing momentum (`yield=0.2`), and a yield that outlives the
/// turn's timeout (`ERR yield timeout`) is a wait, not a refusal
/// (`failed:relaunch` after the agent was ended). Nor is it ever a reason to
/// type the line unfenced into whatever the person is typing: the attempt
/// waits `yield` at once, nothing more typed, and a later sweep reads the
/// prompt afresh. Negative control: a real refusal is still one, at once.
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

/// The heal rides the relaunch line: with the shell's dialect known, the window
/// is asked for a typed re-key of the tab after every check that can make the
/// attempt wait (`rekey shell=<the proven shell>`), and the line typed is the
/// healed one: the shell's own take of the key from the path, then the
/// relaunch line unchanged. Every way the heal can fail leaves the relaunch as
/// it was: a refusal (a healthy tab), an older window that knows no such verb,
/// and a healed line past the tty's bound (its key taken back at once) all
/// type the plain line; no dialect asks nothing. An attempt that then types
/// nothing (a person typing through the yield) takes the key back (`rekey
/// withdraw`) before it returns, so its one-use file never waits for a line
/// that is not coming.
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

/// The heal at a bash prompt: the dialect is read off the live shell
/// ([`shell_dialect`]), and a bash prompt, which is typed to and never pasted,
/// is typed the healed line: bash's own take of the key, then the relaunch
/// line unchanged. Negative control: a re-key the window refuses types the
/// plain line at the same bash, and takes nothing back.
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

/// The requests of `asked` that PASTE into the tab.
#[cfg(unix)]
fn turns_in(asked: &[String]) -> usize {
    asked.iter().filter(|l| l.contains(" turn ")).count()
}

/// The relaunch line is never typed onto a person's command: the fence proves
/// only that the screen held still between a read and the paste, not that the
/// prompt's input line is empty, so a paused `rm -rf build` would get the line
/// appended and Enter pressed on both. The first read marks where the prompt
/// left the cursor, and the mark is kept across attempts: a read that finds
/// typing after it waits (`typing`), nothing typed, fenced or not, and so does
/// the next attempt while the half-typed command is still there. Deleted, the
/// prompt reads as marked again and the line goes. Negative controls: a prompt
/// that holds its cursor is typed on the first fenced try; one whose screen
/// keeps changing with the cursor in place (a ticking right prompt) is still
/// typed unfenced after its fenced tries; one redrawn by an async prompt (its
/// cursor moved, its text changed) waits (`prompt-moved`), never marked afresh
/// within the attempt, and the next attempt marks it and types.
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

/// At a bash prompt the mark still stands between a person's typing and the
/// line: bash is typed to as keystrokes (`send`, which takes no `if-gen=`
/// fence, [`type_relaunch_line`]), so the prompt is read against its mark once
/// more right before them, and a person's typing after it (`ls`) is a wait:
/// nothing is sent. Negative control: the same bash prompt as marked, nothing
/// after it, is typed at.
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

/// The relaunch line is never typed into a program a person started at the
/// returned prompt: the tries after the mark can take seconds (a fenced turn
/// parks up to 30 s on a person's typing), and a person may run `vim x`,
/// `ssh`, a REPL or a second `claude` in that time. A prompt that moves after
/// the mark is a wait within the attempt, and the shell is proven to hold the
/// tab again immediately before every typed try, the unfenced fallback
/// included. Negative control: with the shell holding the tab throughout, the
/// same prompt is typed at.
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

/// A caret moved home over a half-typed command is typing: comparing only the
/// cursor and the text left of it would read `rm -rf build` then ctrl-a as the
/// prompt unmoved (a pure cursor move changes no content, so the fence passes
/// it). The mark keeps where text to its right began (a right prompt): the
/// cells from the cursor up to it must stay blank. Negative controls: a right
/// prompt that ticks in place (a clock) is the prompt unmoved, and so is a
/// bare prompt.
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

/// Typeahead before the first read is never the mark: a command typed while
/// the agent exited is echoed at the prompt before that read, so marking where
/// the cursor sits would append the line to it. Where the shell's integration
/// marked the input's start (`133;B`: `blocks --json`'s `"cmdcol"`, on the row
/// `line` reads), the cursor must sit exactly there; where it did not, the
/// cursor must follow the prompt's trailing blank; either way the cells at the
/// cursor must be blank. Negative controls: a clean prompt is marked and typed
/// at, with the integration's mark and without it.
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
    // The integration's input start: typeahead that DOES end in a blank —
    // which the plain rule alone would take for the prompt — is still typing.
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

/// A MARK THAT NAMES ANOTHER ROW IS NO PERSON'S TYPING (tab #1's shape).
/// An upgrade SIGTERMed Claude Code (fullscreen) and the relaunch read the
/// returned prompt: its cursor where the prompt ends, nothing after it, no
/// person at the keys for nineteen hours — and the shell's `133;B` mark
/// (`blocks 1 --json`: row 69, column = the prompt's length), whose row the
/// `line` verb read as the line ABOVE the prompt (`claude --resume …`; the
/// engine's absolute rows had slid, `aterm-core`'s
/// `shell_mark_rows_across_resize`). The integration's check took the mismatch
/// for typing, every attempt for five minutes waited `typing`, and the agent
/// the upgrade had ended stayed down. The read's own `human_ms` stamp proves
/// what the mark could not: no person keyed the session since the agent's
/// end was asked, so the shell's line holds nothing of a person's, and the
/// line is typed. NEGATIVE CONTROLS — the guard for REAL typing stands: a
/// person's key after the end, a host that stamps no person input, a session
/// the host has never seen keyed (a seamless update's successor reads every
/// adopted session so, whatever was typed before it), or an end not known
/// (a relaunch on exit) each WAIT, and say what holds them (`mark-elsewhere`,
/// never `typing`); a person's typing that wrapped past the prompt's row (the
/// mark on the row above, as here) with their keys since is never typed onto;
/// and a mark that names the cursor's row with typeahead after it is
/// `typing`, as ever.
#[cfg(unix)]
#[test]
fn a_mark_that_names_another_row_is_no_persons_typing() {
    let settled = "OK 0 id=2 submitted=1 status=settled";
    const PROMPT: &str = "me@a-studio-of-128gb aterm %";
    let col = PROMPT.len() + 1;
    // The incident's last rows (`text --json`, tail-trimmed), the cursor
    // where the prompt ends.
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
    // THE INCIDENT: no person since (19 h ago, the end 3 s ago) — typed.
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
    // A person's key AFTER the end (half a second ago): the line holds what
    // they typed, for all the screen shows — a wait, named. So does a read
    // that cannot date the last key against the end.
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
    // A person's typing that WRAPPED past the prompt's row: the mark names
    // the row above (the prompt and the start of their command), the cursor
    // sits on the continuation — after a blank, at the mark's very column.
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

/// TIER-1 OF `RelaunchPromptRead` (`aterm_spec::derive::
/// relaunch_prompt_read_model`): the real relaunch line
/// ([`type_relaunch_line`]) against a stand-in tab in EVERY world of the model
/// — the shell's mark none, on the prompt's row or on another; the host
/// stamping a person's input or not; the text left of the cursor ending in a
/// blank or not — with and without a person's keys standing at the prompt,
/// projected onto the model's state, makes exactly the model's one move
/// there: it types where `Read` is enabled, and elsewhere waits with the word
/// of the model's wait (`typing`, `mark-elsewhere`, `unmarked`). Each world is
/// read in every spelling the host has for it: no mark as no integration
/// (the verb unanswered) and as a newest block that is not `entering` (a
/// command the shell runs — `claude; read` — holds the terminal); no stamp as
/// an older host's (no `human_ms`) and as a session never keyed since the
/// host began serving it (`null`). Tab #1's world (a mark elsewhere, a stamp,
/// nobody keyed) is among them and types. NON-VACUITY: both verdicts occur,
/// and a projection that drops the stamp (an older host's, or tab #1's rule,
/// which never read it) would disagree with the real verdict in tab #1's
/// world.
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
                        // Exactly one of the lane's four moves is the model's here,
                        // and it is the one the real read made: the line, or the
                        // wait with ITS word.
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

/// A STALE EXIT NAMES WHAT HELD ITS LINE: every try notes its wait on the record —
/// the host's carries too — and the stale bound's ledger line says it, in
/// a person's words. With no wait recorded, or one with no words, the line is
/// the bare fact; an end no look has seen yet is only the one asked for.
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

/// NO MARK SAYS A PROMPT IS THERE, NO BLANK ENDS IT, AND NOBODY KEYED: the
/// line is never typed on the stamp alone, and the wait says `unmarked`, not
/// `typing`. The stamp proves no person typed, not
/// that the shell is at a prompt: a shell with no integration whose prompt
/// does not end in a blank looks so, and so does `while :; do claude; read
/// -k1; done` once the agent is gone — the shell's own builtin holds the
/// terminal (its newest block `running`), and a line typed into it would end
/// the `read` with its first key and hand the rest to the agent the loop
/// starts again. With a person's key since, the wait is `typing`, as before;
/// the same prompt ending in a blank is typed at by its shape, as before.
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
    // The shape rule, unchanged: a prompt that ends in a blank is typed at.
    let (got, typed) = run(
        "unmarked-blank",
        screen("me@box proj %", 14, quiet),
        "ERR unscripted",
    );
    assert_eq!(got, Ok(()));
    assert_eq!(typed.len(), 1, "{typed:#?}");
}

/// THE END A RELAUNCH DATES A PERSON'S KEYS AGAINST: a restart that ended its agent itself stamped its record
/// before the signal, after a last look that found nobody's hand, so its
/// stamp is the end; a relaunch on exit stamps its record after the agent
/// left and the back-off ran (up to ten minutes), so keys a person typed at
/// the returned prompt in between would date BEFORE it and read as the
/// agent's — its end is unknown (`0`), and no stamp clears a prompt for it.
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

/// A `--now` reaches the act, and the wait is kept to read. A session whose
/// screen just moved waits `settling`; the state keeps that wait, since when,
/// the tab it is in and how long the session has been behind, and a second
/// sweep with the same wait keeps the same start. That is the negative
/// control: with no request, nothing is typed. `aterm harness upgrade <tab>
/// --now` ([`ask`]) waives exactly that window (it stands the upgrade at the
/// ladder's last rung) and the next visit types the notice; the act clears the
/// kept wait. Ten minutes behind is the ladder's first rung (two hours would
/// be the last rung already, with no settle of its own).
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
    let behind = now_s() - 600;
    let st = St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        // The screen `instance` shows, first seen NOW: not yet quiet.
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

/// The ladder, driven on Claude Code through the real look: the real `visit`,
/// whose `look` builds its facts by the same `ladder_look` the Codex visit
/// does, across two looks, the clock between them moved by rewinding the state
/// the first saved (its still run, the screen's `seq`, as the next look finds
/// them):
///
/// * A repaint at Settled announces: the screen's `seq` moved at each look
///   (its own quiet restarting) while its words stood still a minute, and the
///   second look types the notice. Negative control: the same two looks at the
///   Prefer rung wait `settling`.
/// * A person near the tab at KeysOnly announces (their input a minute old).
///   Negative control: the same person at Settled holds it.
/// * A keystroke within 20 s at Land waits, look after look.
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

/// A `--now` is on the tab it named, not the conversation, so it waives the
/// settling window nowhere the conversation was resumed. A `--now` recorded
/// for another tab is no word here: the settling session waits and nothing is
/// typed. Negative control: the same word recorded for this tab types the
/// notice at once.
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
    let settling = |named: &str| St {
        phase: Phase::Pending,
        from: "1.0.0".to_string(),
        to: "9.9.9".to_string(),
        source: "managed".to_string(),
        // The screen `instance` shows, first seen NOW: not yet quiet.
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

/// A READY answer a hold superseded is no consent. An upgrade held (`--defer`,
/// `--skip`) is yielded nothing, and the window's supervisor waits on a point
/// that answers its notice for three minutes at most, then types `keep going`
/// and the agent goes back to work after its READY. That READY stays in the
/// transcript's tail, so a hold on an announced upgrade ends its notice: the
/// phase is pending again and the marker gone, and once the hold ends the next
/// turn end gets a fresh notice and needs a fresh READY, rather than
/// SIGTERMing an agent in the middle of its work. Negative control: the same
/// READY state, settled, with no hold is ended: the agent dies of the SIGTERM.
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

/// An attended READY session is never signalled on the upgrade's own
/// judgment: a READY answer at a settled point in a tab a person gave input in
/// the last ten minutes (its `human_ms` stamp) waits `attended` (said once,
/// then waited on) however long past the re-ask clock, and the agent is never
/// sent the SIGTERM in front of the person. Negative control: `--now` ends it
/// at once.
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

/// AN AGE BELONGS TO ONE PROCESS: a pending record must not carry its age onto
/// a process the owner restarted by hand since. A newer target carries the age only while the SAME
/// process is still the one behind; a process that changed — by its kernel
/// start, else (a record an older build wrote) by the build it was found
/// behind — starts it from now. NEGATIVE CONTROLS: the same process carries;
/// a record that names neither build nor process carries;
/// a stopped round and a finished one are not this arm's.
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
    // NEGATIVE CONTROLS.
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
    // A PENDING record for the SAME target follows its process too: a session
    // restarted by hand onto another build that is still behind it is behind
    // since the restart, with nothing of the old process's waits.
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
    // NEGATIVE CONTROL: the same process keeps its streak (it is still the one
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
    // An ANNOUNCED record of another process for the same target keeps its
    // own process's start, so the next build's record is minted afresh
    // instead of finding the record 'the same process' one visit later.
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
    // A RECORD AN OLDER BUILD WROTE that predates the process now holding the
    // conversation cannot be that process's age: its age begins when this target was first seen (the
    // record's own salt) or when the process began, whichever is later.
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
    // A STOPPED ROUND IS ABOUT ITS PROCESS OR LAUNCH LINE: kept as it was for
    // the process it was about ...
    let stopped = minted("2.1.275", "B", Phase::Failed("no-resume".to_string()));
    let next = St::for_target(Some(stopped.clone()), &from, &target, None, proc("B"), 900);
    assert_eq!((next.phase, next.pending_since), (stopped.phase, 100));
    // ... and NOT for another: a session the owner
    // restarted by hand onto this still-behind build never had that round, so
    // its age, its answers and the processes recorded under the one before it
    // are not its own. It is asked afresh, from now.
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

/// A STOPPED ROUND'S PROCESS IS STILL BEHIND WHEN A NEWER BUILD LANDS (Claude
/// ships about daily, so builds land in rests, and one that restarted the
/// digest's 24 hours there would forget the round it had answered): the
/// same process keeps its age, its unanswered rounds, its wait log and the
/// last look that saw its work; another process, or a finished upgrade,
/// starts fresh.
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
    // NEGATIVE CONTROLS: another process, and a finished upgrade.
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

/// TIER-1: `harness_upgrade_age_model` (aterm-spec) on EVERY reachable state
/// and EVERY action it enables, against the real code: the record's age
/// carried or begun again by [`St::for_target`] (the process named by its
/// kernel start), closed by [`forget_settled_pending`] when the session runs
/// the build it was moving to, and judged by the owner's view
/// ([`rows`], `Row::stall`) — the state the model says results is the state
/// the real code leaves. NEGATIVE CONTROL: the build that shipped is the
/// model's `Buggy = 1`, whose `AgeIsThisProcess` breaks on a reachable state.
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
        // Where the holder stands: process 0 on 2.1.274; process 1 on 2.1.283
        // (behind 2.1.284 when a newer build has landed).
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
            // The buggy build is a different model; the real code must match
            // the FIXED one (and `Visit`, absent in the buggy one, is driven).
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
    // NEGATIVE CONTROL: the shipped build breaks the property on a state the
    // walk reaches.
    let buggy = interp::with_buggy(&model, 1);
    let mut b = buggy.init_state();
    for action in ["Time", "HandRestart", "NewBuild", "Judge"] {
        assert!(buggy.fire(action, &mut b), "{action} at {b:?}");
    }
    assert!(!buggy.check_invariant("AgeIsThisProcess", &b), "{b:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A STOPPED UPGRADE RETARGETED KEEPS ITS MOVE CLOCK, THE OWNER'S WORD AND ITS
/// STOP STREAK: otherwise a tab that gave up, rested and re-armed would have
/// its `behind_since` reset by every vendor build, and an owner's `--defer`
/// dropped, so the next point could announce inside it. NEGATIVE CONTROLS: a skip names its build and is not carried; a stopped round
/// whose session has MOVED since (it runs another build than the one the
/// round stopped on — `behind_anew`'s case) and a FINISHED upgrade each
/// begin a new wait from now, their word and streak gone.
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

/// The state directory is bounded: a settled upgrade of a conversation no live
/// Claude holds goes once it has sat a week, and nothing else does: a live
/// conversation, a fresh file, an announcement, a restart in flight, a request
/// word, another agent's state, a file that is no state, and every file of a
/// dry run all stay.
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
    // The explicit restart's stamps (review of 3eaa8f83c): no reader looks
    // one up once its tab is gone; the prune removes it once stale, never
    // while it stands.
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
    // `wait:attended`: ONE wait from its first sweep, never an act between.
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
    // An empty model DROPS the launch's (a bucket's way back to the default,
    // D7): no `--model` at all, `--resume` still last.
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

/// The relaunch argv, end to end through the rule: a launch's argv, the model
/// the conversation runs and what the build offers, made into the line the
/// relaunch types by the same three calls the upgrade, the relaunch on exit
/// and the restart in place make ([`upgrade::launch_model`],
/// [`models::model_due`] over the seeded list, and [`line_for`], whose
/// `with_model` puts the model in place of the launch's own). The first row:
/// no `--model`, the transcript on Opus 5, the build's newest Opus 5.5; the
/// line carries `--model claude-opus-5-5`, so the session does not come back
/// on Opus 5.
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
        // An ALIAS is kept verbatim: the build resolves it to its newest.
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
        // Nothing newer of its family, and a Fable NOBODY chose: the list's
        // step moves it up to the Opus 5.5 ranked above it...
        (
            &["claude", "--verbose"],
            "claude-fable-5-1",
            &["--verbose", "--model", "claude-opus-5-5"],
        ),
        // ...but a Fable the LAUNCH chose is never moved across families.
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

/// A SAME-BUILD (model) restart the list no longer asks for is forgotten while
/// nothing has begun, so neither the owner's view nor the tab's supervisor
/// reads it PENDING for the rest of the conversation — and so is a pending
/// record for a build the session now RUNS (the owner restarted it by hand),
/// which is reported, so the ledger says so once.
/// NEGATIVE CONTROLS: a build upgrade to a NEWER build, an announced record,
/// a stopped round, a record that owes a release and a dry run keep their
/// state.
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

/// The window's step answers with the tab's own word ([`pick`]): a Codex tab's
/// pass also reports the daemon it runs on (`tab=-`), and the daemon's act
/// never stands for the client's wait (a `daemon-updated` read as the tab's
/// last word would leave its client unmoved until the next activation
/// notice). A client that is current leaves the daemon's word; with nothing
/// of the tab's, the first act, then the first wait.
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

/// The notice at a break of the agent's own background work, at the drive: a
/// Claude Code whose own status is `busy` (its turn over, a workflow it waits
/// on) is announced to when the window's host takes the step at the break
/// (`Opts::background`), on a screen that has just moved (the loop's settle
/// stands for it). Then, within the re-ask interval, nothing more there: the
/// step waits `background`, typing and ending nothing. (The re-ask a whole
/// `REASK_S` later is the reducer's, pinned in `upgrade_tests`.) Negative
/// control: the same busy session at an idle point's step waits `not-idle`.
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

/// `h`'s record as THIS build writes it: naming the stand-in agent's own
/// process ([`St::proc_start`]). A record that names none is an older
/// build's, and the age it carries from before the process began is
/// repaired to the process's start at the next look (`St::retarget`'s first
/// arm) once a new round's salt is past it — so a rig whose agent has been
/// behind for an hour names the agent the hour belongs to.
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

/// THE QUIET UPGRADE RELEASES THE AGENT AN OLDER BUILD TOLD TO WIND DOWN:
/// `Announced{asks:3}` written by an older build three days ago, Claude's
/// status `shell`, two widowed shells under it that never end — every screen
/// a break. Under the default (`upgrade_ask = false`) the round gives up at
/// the break and types its ONE release in the same visit — worded for the
/// quiet flow, promising no ask before a later restart — then rests (`wait:failed`). Nothing is ended. NEGATIVE CONTROL: the same
/// round inside its window waits `background` and types nothing.
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
    // What the owner's row tells this give-up by (#4's review): its own work
    // was seen — `Upgrade now` does not move it past that work.
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
    // NEGATIVE CONTROL: inside the notice's window.
    let h = rig("quiet-announced-new", now_s() - 60);
    let shells = shells_under(&h);
    assert_eq!(h.visit_with(dead_pid(), &shells).step, "wait:background");
    assert!(h.typed().is_empty(), "{:?}", h.typed());
}

/// A QUIET PENDING ROUND TYPES THE RELEASE IT OWES WHERE ITS AGENT'S OWN WORK
/// HOLDS THE RESTART: a notice reached this agent once and
/// its round was abandoned since (here a retarget), so the agent was asked to
/// wind down and is owed the line that releases it. The quiet flow types no
/// notice, and its restart waits on shells that never end, so the release is
/// the round's next act there (`released:retargeted`),
/// and the round stays pending, its quiet restart still to come. NEGATIVE
/// CONTROLS: a release owed to ANOTHER process is dropped, nothing typed; a
/// round owing nothing types nothing.
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
    // NEGATIVE CONTROLS.
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

/// THE OWNER'S `--now` MOVES A QUIET SESSION WHOSE STATUS ONLY LAGS, ALL THE
/// WAY TO THE SIGNAL: Claude's own `shell` left over an
/// idle screen with nothing under the agent — an agent that finished, or
/// asked its person something — holds the quiet restart (`status-stale`),
/// since the quiet flow types nothing that would rewrite it. The quiet step
/// reads that lag past on the owner's word, and so does the restart's last
/// look before the signal: it takes the very status the look read past, unrewritten since, and the
/// agent is ended (SIGTERM), nothing typed first. NEGATIVE CONTROLS: unasked,
/// the status holds the restart at every look; the first look under the
/// word shows no lag yet; and the last look's own reading
/// ([`status_lets_signal`]) refuses a status rewritten since the look.
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

/// THE OWNER'S `--now` NEVER SIGNALS A TURN BEGUN SINCE THE LOOK OVER A
/// LAGGING STATUS: where the quiet step reads past a status
/// that only lags on the owner's word, the restart's last look takes the very
/// status the look read, unrewritten since ([`status_lets_signal`]) — but a
/// stale `busy` that Claude Code need not restamp when a turn begins (a
/// background task's end that woke the agent) would pass that, so the last
/// look and the re-read before the signal also ask the screen for the turn
/// over ([`turn_over_shown`]): no busy footer, the reader at an
/// idle point. NEGATIVE CONTROL: the same visits over a screen that stays
/// idle end the agent (SIGTERM), nothing typed first.
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
    // NEGATIVE CONTROL: the screen stays idle — ended, nothing typed.
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

/// THE RELEASE A GIVE-UP OWES IS TYPED AT A BREAK: a Claude Code
/// whose own status reads `shell`, two shells under it that never end — every
/// screen a break, no idle point ever. Its fourth notice's window runs out at
/// a break: the upgrade gives up there, and in the same visit types the ONE
/// release line it owes, on the ledger as `released:gave-up`; after it the
/// upgrade's word is `wait:failed`, and nothing more is typed. Nothing is
/// ended: the shells run on. THE LATE READY the shells outlive —
/// the agent stopped for a restart a break never takes — is voided at the
/// break past the drain and the agent released the same way, and so is one
/// that work inside the agent's own process outlives (workflows it waits on,
/// nothing under it). Negative control: an announced upgrade inside its
/// window types nothing at a break.
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

    // The late READY work inside the agent outlives: workflows the agent
    // waits on (`✻ Waiting for 2 dynamic workflows`, Claude's status `busy`)
    // run in its own process, so nothing runs under it. Past the drain the
    // READY is voided at the break and the agent released, as over a shell,
    // rather than waiting `background` on a restart the break never takes.
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

/// What holds the move is kept by start, not age ([`St::note_held`]): `ps` ages
/// to the second, read at another second than the look's clock, so the same
/// process keeps the start it had and its status row stays the same row at
/// every look (the window is sent a row only when it changes). A process that
/// is new under an old pid is its own. A step that acts clears it. Negative
/// control: a look that waited on nothing running names nothing.
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

/// Claude's status against an idle screen, through the real visit: a Claude
/// Code whose idle screen draws no shell count, its own status `shell` over two
/// shells that never end, is asked at its idle point: the first look waits
/// `status-stale` (the status disagrees with the screen, named), the second
/// types the notice, naming the shells; then the status view is told what
/// holds the move, and nothing is ever ended. A status left `busy` over an
/// idle screen and an empty process tree reads idle the same way. Negative
/// control: the same shells under a turn still running (a foreground Bash
/// call) never read an idle screen: `not-idle`, nothing typed.
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

    // A STALE `busy` over an idle screen and nothing under the agent.
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

/// The idle looks are one agent's, in a row: [`St::idle_looks`] counts the looks
/// that read this process's screen idle, and the record carries it across
/// visits keyed to the process it read, so an idle look of an old process
/// (a gone notice's new holder, a record that followed the conversation to
/// another tab) does not count toward the new one's lag and let the notice go
/// on the new agent's first look, the one look [`upgrade::IDLE_LOOKS`] exists
/// to refuse. The run is bounded in time ([`IDLE_RUN_GAP_S`]): a look older
/// than that begins it again. Negative control: the new agent's own second
/// look is its lag, and the notice goes.
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

// ---------------------------------------------------------------- D1: no task

/// A user row the harness typed, as the E2E's transcripts hold one.
#[cfg(unix)]
fn harness_row(text: &str) -> String {
    user_row(&format!("[aterm harness] {text}"))
}

/// Whether a conversation has a task, read on: nobody can say without readable
/// projects; with them, no transcript is no message and no task; the harness's
/// own turns, answered, are no task; a line caught half-written is read once
/// whole; someone else's prompt is a task the moment it is whole (before its
/// answer), and for good, and the next ask of the same conversation reads only
/// what was written since. Two transcripts claiming the conversation: nobody
/// can say. Negative control: another conversation is read from its own start.
#[cfg(unix)]
#[test]
fn whether_a_conversation_has_a_task_is_read_on_from_where_the_last_read_ended() {
    let dir = scratch("tasked-memo");
    let home = dir.join("home");
    assert_eq!(
        tasked(&home, SESSION, &[]),
        None,
        "no projects: nobody can say"
    );
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

/// A stand-in agent in [`TAB`] whose conversation holds `rows` — its state
/// Pending on the newer build, its screen quiet an hour — and what a visit
/// under `kernel` answers, the state it left, and every line typed.
#[cfg(unix)]
fn visit_holding(
    name: &str,
    rows: &[String],
    dry_run: bool,
    kernel: &Script,
) -> (Report, Option<St>, usize) {
    visit_seen(name, rows, dry_run, kernel, 77, None, None)
}

/// [`visit_holding`], the last look having seen the screen at `last_seq` an
/// hour ago (the screen now reads `seq` 77), the tab's `status` saying
/// `status` (`None`: nothing held, no agent verdict), and — `Some` — the
/// tab's supervisor having ledgered those texts as typed
/// ([`Opts::aterm_state`]; `None`: no ledger known).
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
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &table,
        &newer(),
        kernel,
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

/// An upgrade never announces to a conversation with no task: the notice would
/// start a conversation nobody asked for. A fresh session, and one holding only
/// the harness's own turns, is restarted afresh instead (`would-restart:fresh`).
/// Negative controls: once a person's prompt was answered it is announced to
/// (`would-announce`), and so is one whose first prompt is recorded and not
/// answered yet (read as taskless, the fresh restart would end it in flight).
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

/// The quiet upgrade at the driver (the default): a tasked conversation at a
/// genuinely idle point is restarted with nothing typed before it
/// (`would-restart`, never `would-announce`), and the line it plans resumes the
/// conversation (`--resume`, the launch's flags). The person is told first:
/// the round first seen behind at this very look waits `notice-shown`, and so
/// does a conversation with no task. The run up to the signal keeps every last
/// look: backgrounded under it, nothing is signalled and the round stays
/// pending, nothing typed. The knob (`upgrade_ask = true`, every other test
/// here): `would-announce` at the same point.
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
    // THE KNOB: the notice, as before.
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

/// THE UPGRADE MOVES A SESSION ONTO A NEWER BUILD, NEVER ONTO ANOTHER
/// ACCOUNT: the line it plans re-applies exactly the login the agent's own
/// exec environment names — a session an account move relaunched under
/// `work-2` comes back from the upgrade under `work-2`, where the tab's shell
/// would have put it back on its own login. Read off the line the visit
/// planned up to its signal (the stand-in's shell backgrounds it there), and
/// kept as a fact on the record. NEGATIVE CONTROL: an agent whose
/// environment names no login gets no such word, and no `login` on its
/// record.
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
    // NEGATIVE CONTROL.
    let plain = run("upgrade-login-none", &[]);
    assert!(
        !plain.line.contains("CLAUDE_SECURESTORAGE_CONFIG_DIR") && plain.line.contains("--resume"),
        "{}",
        plain.line
    );
    assert_eq!(plain.login, None);
}

/// A QUIET RESTART IS CARRIED ON WITH NOTHING ([`St::quiet`]): the agent was
/// idle and no notice was typed, so the relaunched conversation — a
/// tasked one — is `done:resumed`, said once, nothing typed; the tab's loop
/// decides that point as if nothing had moved. NEGATIVE CONTROL: the same
/// conversation after an announced restart (the knob's) is carried on.
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
    // Nothing waits for a model to confirm here: never "unconfirmed", which
    // read as a check that failed on every default upgrade.
    assert!(
        st.outcome.contains("resumed while idle") && !st.outcome.contains("unconfirmed"),
        "{}",
        st.outcome
    );
    assert_eq!(rig.details("done:resumed").len(), 1, "said once");
    assert!(!moved("done:resumed"), "the point still stands");
    let rig = Rig::new("carry-announced", &asked);
    let mut st = rig.st.clone();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    assert_ne!(r.step, "done:resumed", "{r:?}");
    assert_eq!(rig.typed().len(), 1, "{r:?}");
}

/// A pending round that still owes its agent a release is no quiet restart:
/// the release says a notice reached the agent (abandoned since by a retarget,
/// a `--defer`, a give-up re-armed), so it was asked to wind down and may have
/// answered READY and stopped. Under the default the reducer restarts the
/// pending round at the idle point; the restart is not quiet ([`St::quiet`]
/// false, so its carry-on types the continuation), and it goes through the
/// notice's own fence (the process the notice reached,
/// [`St::notice_belongs_to`]) up to the last look before the signal, where the
/// stand-in's shell backgrounds it and everything is put back, the release
/// included. Negative controls: the same round owing nothing, and one owing a
/// release to another process (void here), are quiet restarts.
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

/// What the tab's own supervisor typed is no task: a record holding the
/// harness's notice and the supervisor's own `keep going` and `answer_text`
/// (unmarked, but in the tab's loop's ledger under [`Opts::aterm_state`]) is
/// restarted afresh. Negative controls: the same record with no ledger known,
/// and with a ledger of other words, is announced to (its `keep going` read as
/// someone's).
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

/// D1: WHAT THE TAB'S SUPERVISOR TYPED INTO ANOTHER CONVERSATION IS NO ROW
/// OF THIS ONE. The ledger is the TAB's, kept across every conversation the
/// tab has held: a person who begins a new conversation there with the
/// loop's own words (`keep going`) has asked it something, and it is
/// announced to and resumed — never restarted afresh, out of its tab, on the
/// strength of a `keep going` the loop typed into an earlier conversation.
/// Its control is [`what_the_tabs_own_supervisor_typed_is_no_task`]: the
/// same words the loop typed as the row was recorded are its own.
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

/// D1: THE FRESH RESTART IS THE UPGRADE'S RESTART WITHOUT THE NOTICE AND
/// WITHOUT `--resume`: nothing is typed into the agent, the line it plans
/// starts the newer build on the launch's flags alone, and every last look
/// before the signal stands — here the agent is put in the background under
/// it, so nothing is signalled and the state goes back to where it was (its
/// cause with it: a later restart of a conversation that gained a task
/// resumes it).
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

/// The upgrade resumes in the mode the pill shows: Claude writes its
/// `permission-mode` row only when it re-appends its session metadata, not as
/// the mode changes, so a session launched with
/// `--dangerously-skip-permissions` and switched to auto still reads bypass in
/// its transcript. The relaunch takes the mode the visit's look read off the
/// tab's pill. Negative control: with no pill on the screen, the transcript's
/// mode decides.
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

/// The announcement is planned in the mode the pill shows: the notice is typed
/// only once a relaunch line is planned, and that plan takes the look's pill,
/// as the restart does (not the transcript's mode alone); the line planned is
/// kept with the announcement. Negative control: with no pill on the screen,
/// the transcript's bypass, the launch's own flags.
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

/// The upgrade carries a model chosen by hand: a session launched `--model
/// claude-sonnet-5-5` and moved to Opus with `/model` (answered on Opus since)
/// is relaunched asking for Opus when the list moves it to no model (its
/// `model_list` empty), in the line its announcement plans and in the fresh
/// restart's. Negative control: nobody's `/model` remembered: the launch's
/// flags.
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

/// The upgrade's restart makes a hand choice it carries the fallback's origin:
/// a session on a bucket's fallback (`--model claude-sonnet-5-5`, launched on
/// Fable), moved to Opus with `/model` since, is relaunched asking for Opus,
/// and once the signal is sent the saved record names Opus as the origin, so
/// the way back at the bucket's reset does not put Fable over it. Negative
/// controls: nobody's `/model`: Fable carried; a restart that stops short of
/// its signal (the job moved before it, the server refused it) leaves the
/// origin as it was.
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

/// A restart holds the tab from its last look ([`Hand`]): a fresh restart can
/// fire the moment an orchestrator's `await agent idle` returns, and a first
/// prompt sent to the bare shell before the relaunch line would run as a
/// command line. The hand is taken before the last look and every read of that
/// look comes after it; a look that then stops the restart (here the agent put
/// in the background under it) gives it back, nothing signalled. Negative
/// control: another driver's live lease on the tab is theirs: the restart
/// waits `held`, reads no last look, signals and releases nothing.
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

/// The last look and the signal: between the restart's first look (under its
/// hand) and the signal the state is written, and a person who keys the tab
/// then (`human_ms` inside the grace), or a prompt sent then and not read yet
/// (`input=pending`), is seen by the last look: nothing is signalled, the
/// state goes back, the hand is given back. The signal itself is the last
/// word, one server decision (`signal term pid=<p> quiet=<grace>`): a
/// keystroke after the last look refuses it (`ERR busy person …`), a wait, the
/// state back, the hand given back. Negative control: the same restart with
/// nothing arriving sends the signal, `quiet=` the grace.
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

/// A SIGNAL NEVER SENT ASKED NOTHING: an announced
/// round whose notice carried `claude-opus-5-5` is READY'd, and its restart
/// records the ask before its one act; the signal then refused (a keystroke
/// after the last look), the ask is TAKEN BACK and the one it replaced — an
/// earlier ask of the harness's, still settling — stands again, time and all.
/// Kept, it settled as FAILED [`models::MODEL_SETTLE_S`] later though nothing
/// relaunched: the model lost to the conversation for good, and to the very
/// round whose READY the refusal left standing (the ride's guard). The ask
/// kept is the negative control of `ModelRecord::take_back`'s unit test.
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

/// A MODEL IS ASKED ONLY BY THE LINE THAT ASKS IT: the restart
/// in place (Claude Code's critical-memory banner, aterm's memory ceiling)
/// rides a due model — the conversation answers on Opus 5 and Opus 5.5 is
/// offered — and the conversation's record names it asked only once the
/// relaunch line carrying it is typed. The signal refused (a person's key
/// after the last look), or sent with its line never typed (the tab not
/// proven the shell's: a wait, which a stale exit later ends), no relaunch
/// asked the model, and the record is as it was. Written before the signal,
/// the ask settled FAILED ten minutes later though nothing relaunched: a
/// `model-failed` row, the model barred from the conversation for good, and a
/// person's own later `--model` of it dropped from every relaunch line.
/// NEGATIVE CONTROL: the line typed at the shell the roster names holding the
/// tab records the ask.
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
            // The stand-in's agent is this test's child, alive to `kill(0)`
            // until reaped: the relaunch is carried on, as the host's next
            // idle point carries it, once it surely is.
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
            // NEGATIVE CONTROL: the line typed asks the model, and the record
            // says so.
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

/// A conversation with no task is carried on with nothing: a carry-on
/// ("Continue where you left off … decide for yourself") typed into one would
/// start a conversation nobody asked for. A relaunched conversation whose
/// record holds only the harness's own turns is done (`done:taskless`, said
/// once in the ledger) and nothing is typed. Negative control: one a person
/// asked something is carried on.
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

/// The settle is the agent's verdict's, not the screen's: counted from the
/// look that first saw a new screen `seq`, a dim `❯ continue` suggestion drawn
/// between two looks would restart it. It is how long the server's own verdict
/// has said the agent's turn is over (`status agent=idle agent_since_ms=`): a
/// screen that moved since the last look, with the agent idle 30 s, is
/// announced to at once. Negative controls: the agent idle only 5 s, or busy,
/// waits `settling`; and a server that publishes no verdict, or one with no
/// evidence (`unknown`), is measured by the looks' samples: a moved screen
/// waits.
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

/// A TURN THE HARNESS TYPES RETURNS ONCE ITS SUBMIT IS TAKEN, NOT WHEN THE
/// AGENT'S ANSWER IS OVER: the step is taken in the session's loop while the
/// loop is still, and an answer that ended inside the step would go unseen by
/// the loop. Here the stand-in agent answers for two minutes
/// after every submit: the carry-on and the notice each return inside a
/// minute's hang detector, asked `idle=1 timeout=10000` ([`TURN_WAIT`]; the
/// stand-in names no `cap=`, so the notice is pasted — a pressed one's
/// timeout grows with its keys, [`turn_wait`], and returns alike) — a
/// step that waited the answer out is told from one that did not whatever
/// the machine's load. Negative control: a turn asked to settle on quiet
/// waits the answer out (a stand-in answering for 1 s, so the
/// control costs a second, not minutes).
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
    // Negative control: the old settle waits the stand-in's answer out. The
    // answer's length is the stand-in's own, not the product's, and the
    // control holds for any: the stand-in sleeps it before it replies, so the
    // turn cannot return sooner at any load.
    let answer = Duration::from_secs(1);
    let dir = scratch("turn-wait-control");
    let (sock, _) = instance_with(&dir, answers(answer));
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

/// A stop that repeats rests longer, never for good: the same reason in a row
/// stretches the rest (RETRY_S, then twice, then four times as long, no
/// longer), so a relaunch that never comes up is not re-asked, re-signalled
/// and left dead every RETRY_S. A different reason starts the streak over, and
/// the rest still ends: the round re-arms. Negative control: the first stop
/// rests exactly RETRY_S.
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

/// The end of the agent's own work re-arms a round that gave up and asks in
/// the same visit: a round that asked at breaks of a workflow, each answered
/// "not yet", gave up and rested. Here the record as it rests (gave up an hour
/// ago, the agent's work seen since its latest notice, `St::work_seen`) meets
/// an idle point with nothing under the agent: the round is re-armed
/// (`rearmed:unanswered`, in the ledger) and its first notice typed in the
/// same visit (`announced:1`), so the loop never has the point in between; the
/// stamp is spent with the notice. An announced round there asks again at
/// once. Negative controls: the same record with no work seen rests
/// (`wait:failed`), and the announced one waits for its answer
/// (`wait:awaiting-ready`).
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

/// THE OWNER'S WORD OUTLIVES THE BUILD IT WAS PRESSED ON: `Not today` is a `--defer` on the SESSION, and the retarget arm
/// carries it to the next build. A record closed because its session reached
/// its build by hand ([`forget_settled_pending`]) must not take the word with
/// it, or the next build's record begins with none and a notice is typed
/// inside the owner's snooze. A record holding a word still in force is kept;
/// a spent one is closed.
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
    // NEGATIVE CONTROLS: a spent defer and no word at all close the record.
    for request in [Request::DeferUntil(1), Request::None] {
        let spent = record(request);
        save(&opts, SESSION, &spent);
        assert!(forget_settled_pending(&opts, SESSION, Some(&spent), &running).is_some());
        assert!(load(&opts, SESSION).is_none());
    }
}

/// A CALLER THAT NAMES NO PROCESS GETS THE BUILD TEST, which cannot tell a
/// session the owner restarted by hand onto the SAME build from the process
/// the record was minted for: so every
/// caller that retargets names its process — the visit from the session file
/// it holds; any later caller (a timer that retargets off a point) must read
/// the session file too, and retarget nothing when it cannot. Only the Codex
/// lane, whose conversation is its daemon's, passes none.
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

/// THE END OF THE AGENT'S OWN WORK ASKS NO SOONER THAN ITS FLOOR AND NO MORE THAN
/// ITS BUDGET: the ask it leads to is exempt
/// from the round's ask cap and clock, so the driver bounds the FACT — ten
/// minutes after the latest notice (for a resting round, after its rest began),
/// six a day. NEGATIVE CONTROLS: the same record outside the bounds reads the
/// end of the work.
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
    // A repeated stop's longer rest counts from the STOP: `failed_at` sits
    // the ladder past it, and the end of the agent's work asks at once.
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
    // A ROLLING window: the asks age out one at a time.
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

/// The notices a round-after-round declining agent is asked in `hours`, by the
/// real reducer and the real record transitions: after every notice its work
/// runs for `job_s` seconds (a job under the agent, seen by the looks) and ends,
/// and it answers "not yet" each time. Looks are a minute apart. `Some(t)`
/// for every notice's second.
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

/// A SESSION THAT KEEPS STARTING AND ENDING WORK IS ASKED A BOUNDED NUMBER OF
/// TIMES A DAY: the ask at the end of the agent's work is exempt from the ask
/// cap, and each is a full model turn on the owner's whole context. The
/// adversary declines every notice and starts a short job after each. The
/// notices in ANY 24 hours stay within what the ladder allows: at most three
/// rounds of [`upgrade::MAX_ASKS`] (rests of 2 h and 4 h between them, the
/// third round's rest 8 h, then a day) and the day's budget of asks at the end
/// of its work, one more for the round's first; and from the third day, with
/// the rest a day long, one round's worth. NEGATIVE CONTROL (mutation-checked):
/// without the budget the same agent is asked in every gap between its jobs.
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

// ---------------------------------------------------------------- tab 1's real shape

/// THE ASK AT THE END OF THE AGENT'S OWN WORK NEEDS NOTHING UNDER THE AGENT AND
/// CLAUDE'S OWN STATUS IDLE. Tab 1's recorded shape: an announced round 15
/// minutes past its latest notice, the agent's disk watcher — a bash and its
/// zsh wrapper, five hours old — under it at every look, Claude's status
/// `busy` and unchanged through two notices' replies, the work seen since the
/// notice. Four points of one
/// round, the end of the work asked exactly where both hold:
///
/// * the watcher under the agent, status busy: no ask (`awaiting-ready`, or
///   the status' own wait) — tab 1;
/// * the watcher gone, status BUSY (a `/goal` keeps Claude's status busy):
///   no ask, the gate's wait (`status-stale`: the screen reads idle, the status
///   does not, and the restart's gate never takes that lag for idle) — the work
///   is gone and the status is not;
/// * the watcher under the agent, status idle: no ask;
/// * the watcher gone, status idle: THE NOTICE, at once.
///
/// So on a tab like tab 1 the end of the workflow alone asks nothing: the
/// watcher must go and the goal must let Claude's status idle.
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

// ---------------------------------------------------- the typed-act guard

/// What makes a function's typing aterm's own hand ([`Hand`]): it takes one
/// (`Hand::take`, `take_or_none`), runs the act through [`under_hand`] or
/// [`type_fenced`], or is given one by its caller (`&mut Hand`).
const HAND_TOKENS: [&str; 5] = [
    "Hand::take",
    "take_or_none",
    "type_fenced(",
    "under_hand(",
    "&mut Hand",
];

/// The calls that put text into an agent's composer or a shell's prompt.
const TYPING_CALLS: [&str; 7] = [
    "turn_fenced(",
    "notice_fenced(",
    "type_text(",
    "type_line(",
    "turn(",
    // The keys the Codex lane sends into a composer (ctrl+u, esc, enter) and
    // the clear that sends them: the presence layer reads a key as somebody
    // driving the tab as it reads a turn.
    "key_args(",
    "composer_clear(",
];

/// The typing a hand-less function may contain, each with WHERE THE HAND IS
/// TAKEN — a function in the same file that holds a [`HAND_TOKENS`] token
/// (checked) and the call chain to it (read; said in the reason). An unleased
/// `turn` is read by the window's presence layer as somebody driving the tab:
/// a row is born and folded around it and the tab's PTY is re-gridded twice.
/// A new typed act joins this
/// list only with a reason that names whose hand it is under.
const UNDER_ANOTHERS_HAND: [(&str, &str, &str, &str); 4] = [
    (
        "upgrade_drive.rs",
        "turn",
        "relaunch.rs:carry_on_once",
        "the unfenced primitive: its one caller is the continuation's last resort \
         (relaunch::type_continuation), itself inside carry_on_once's under_hand",
    ),
    (
        "relaunch.rs",
        "type_continuation",
        "relaunch.rs:carry_on_once",
        "called inside carry_on_once's under_hand (the caller's hand, else one for the typing)",
    ),
    (
        "relaunch.rs",
        "type_at_prompt",
        "relaunch.rs:relaunch_held",
        "the relaunch line: type_relaunch_line(_with) is called by relaunch_held under its \
         `&mut Hand`, and by the Codex lane's relaunch_held under its own",
    ),
    (
        "upgrade_codex_drive.rs",
        "exit_typed",
        "upgrade_codex_drive.rs:exit",
        "its one caller, exit, takes the hand before the last look and gives it back where \
         the /exit is not typed (the relaunch keeps it)",
    ),
];

/// Each function of `src` (before its inline test module) as (name, body),
/// the body's comment lines dropped. A function ends at the next `fn` line
/// no deeper than it.
fn functions_of(src: &str) -> Vec<(String, String)> {
    let mut lines: Vec<&str> = src.lines().collect();
    if let Some(at) = (0..lines.len().saturating_sub(1)).find(|&i| {
        lines[i].trim() == "#[cfg(test)]"
            && lines[i + 1].trim_end().ends_with('{')
            && lines[i + 1].contains("mod ")
    }) {
        lines.truncate(at);
    }
    let head = |line: &str| -> Option<(usize, String)> {
        let indent = line.len() - line.trim_start().len();
        let mut rest = line.trim_start();
        if let Some(after) = rest.strip_prefix("pub") {
            rest = match after.strip_prefix('(') {
                Some(inner) => inner.split_once(')')?.1.trim_start(),
                None => after.trim_start(),
            };
        }
        for word in ["const ", "async ", "unsafe "] {
            rest = rest.strip_prefix(word).unwrap_or(rest);
        }
        let name = rest.strip_prefix("fn ")?;
        let end = name.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
        Some((indent, name[..end].to_string()))
    };
    let heads: Vec<(usize, usize, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| head(l).map(|(indent, name)| (i, indent, name)))
        .collect();
    heads
        .iter()
        .enumerate()
        .map(|(n, (at, indent, name))| {
            let end = heads[n + 1..]
                .iter()
                .find(|(_, deeper, _)| deeper <= indent)
                .map_or(lines.len(), |(i, _, _)| *i);
            let body: Vec<&str> = lines[*at..end]
                .iter()
                .copied()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect();
            (name.clone(), body.join("\n"))
        })
        .collect()
}

/// Whether `body` calls one of [`TYPING_CALLS`] (a call, not its definition).
fn types_text(body: &str) -> bool {
    TYPING_CALLS.iter().any(|call| {
        body.match_indices(call).any(|(at, _)| {
            let before = &body[..at];
            !before.ends_with("fn ") && !before.ends_with(|c: char| c.is_alphanumeric() || c == '_')
        })
    })
}

/// Whether `body` holds a hand. A `&mut Hand` counts only where the caller MUST
/// give one: an `Option<&mut Hand>` may be `None`, so a function that types
/// through it holds a hand only by taking one itself (`under_hand(`,
/// `Hand::take`, ...), which the other tokens see.
fn holds_a_hand(body: &str) -> bool {
    HAND_TOKENS.iter().any(|t| {
        body.match_indices(t).any(|(at, _)| {
            let before = &body[..at];
            // A definition (`fn type_fenced(`) is no call: the function that
            // wraps a hand is judged by its own body, never by its signature.
            !before.ends_with("fn ") && (*t != "&mut Hand" || !before.ends_with("Option<"))
        })
    })
}

/// The functions of `sources` (file name, text) that type text with no hand
/// and no entry in [`UNDER_ANOTHERS_HAND`] whose holder holds one.
fn unleased_typing(sources: &[(String, String)]) -> Vec<String> {
    let fns: Vec<(String, Vec<(String, String)>)> = sources
        .iter()
        .map(|(file, src)| (file.clone(), functions_of(src)))
        .collect();
    let body_of = |file: &str, name: &str| -> Option<&str> {
        fns.iter()
            .find(|(f, _)| f == file)?
            .1
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_str())
    };
    let mut bad = Vec::new();
    for (file, functions) in &fns {
        for (name, body) in functions {
            if !types_text(body) || holds_a_hand(body) {
                continue;
            }
            let covered = UNDER_ANOTHERS_HAND
                .iter()
                .find(|(f, n, ..)| f == file && n == name)
                .and_then(|(_, _, holder, _)| {
                    let (hf, hn) = holder.split_once(':')?;
                    body_of(hf, hn)
                })
                .is_some_and(holds_a_hand);
            if !covered {
                bad.push(format!("{file}::{name}"));
            }
        }
    }
    bad
}

/// THE TYPED-ACT GUARD: every function under `harness/` that types text into an agent's
/// composer or a shell's prompt does it under aterm's own hand, or is
/// named — with where its hand is taken — in [`UNDER_ANOTHERS_HAND`]: an
/// unleased act reads to the window as somebody driving the tab, and
/// re-grids its PTY. NEGATIVE CONTROLS:
/// the scan finds an unleased act in a synthetic source, and a handed one is
/// not; every allowlist entry names a function that exists, types, and is
/// covered; the scan sees the source it is guarding.
#[test]
fn every_typed_act_in_the_harness_is_under_a_hand() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/harness");
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the harness directory") {
        let path = entry.expect("an entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
            sources.push((
                name.to_string(),
                std::fs::read_to_string(&path).expect("source"),
            ));
        }
    }
    assert!(
        ["upgrade_drive.rs", "upgrade_codex_drive.rs", "relaunch.rs"]
            .iter()
            .all(|f| sources.iter().any(|(n, _)| n == f)),
        "the scan reads the three files that type"
    );
    let unleased = unleased_typing(&sources);
    assert!(
        unleased.is_empty(),
        "typed acts with no hand (take one with `under_hand`, or name whose hand it is \
         under in UNDER_ANOTHERS_HAND): {unleased:?}"
    );
    // The scan sees typing at all: the guarded files' typing functions.
    let typing: Vec<String> = sources
        .iter()
        .flat_map(|(file, src)| {
            functions_of(src)
                .into_iter()
                .filter(|(_, body)| types_text(body))
                .map(move |(name, _)| format!("{file}::{name}"))
        })
        .collect();
    for want in [
        "upgrade_codex_drive.rs::type_under",
        "upgrade_codex_drive.rs::carry_on",
        "relaunch.rs::type_continuation",
        "upgrade_drive.rs::type_fenced",
    ] {
        assert!(typing.iter().any(|t| t == want), "{want} types: {typing:?}");
    }
    // Every allowlist entry is real: the function types, the holder holds.
    for (file, name, holder, reason) in UNDER_ANOTHERS_HAND {
        assert!(!reason.is_empty());
        let functions = sources
            .iter()
            .find(|(n, _)| n == file)
            .map(|(_, src)| functions_of(src))
            .expect("the entry's file");
        let body = &functions
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{file}::{name} exists"))
            .1;
        assert!(
            types_text(body) || holds_a_hand(body),
            "{file}::{name} no longer types: drop its entry"
        );
        let (hf, hn) = holder.split_once(':').expect("file:fn");
        let held = sources
            .iter()
            .find(|(n, _)| n == hf)
            .map(|(_, src)| functions_of(src))
            .and_then(|f| f.into_iter().find(|(n, _)| n == hn))
            .unwrap_or_else(|| panic!("{holder} exists"));
        assert!(holds_a_hand(&held.1), "{holder} holds no hand");
    }
    // NEGATIVE CONTROL: the same scan over a synthetic source.
    let synthetic = |body: &str| {
        vec![(
            "synthetic.rs".to_string(),
            format!("fn announce(c: &mut Client) {{\n{body}\n}}\n"),
        )]
    };
    assert_eq!(
        unleased_typing(&synthetic("    let _ = turn_fenced(c, tab, text, None);")),
        ["synthetic.rs::announce"],
        "an unleased turn_fenced is found"
    );
    assert_eq!(
        unleased_typing(&synthetic("    let _ = type_text(c, tab, text, None);")),
        ["synthetic.rs::announce"],
        "an unleased type_text is found"
    );
    assert_eq!(
        unleased_typing(&synthetic("    let _ = notice_fenced(c, tab, text, None);")),
        ["synthetic.rs::announce"],
        "an unleased notice_fenced is found"
    );
    // A function named for the hand it never takes: the signature is no hand.
    assert_eq!(
        unleased_typing(&[(
            "synthetic.rs".to_string(),
            "fn type_fenced(c: &mut Client) {\n    let _ = turn_fenced(c, tab, text, None);\n}\n"
                .to_string(),
        )]),
        ["synthetic.rs::type_fenced"],
        "a type_fenced that takes no hand is found"
    );
    assert!(
        unleased_typing(&synthetic(
            "    let hand = Hand::take_or_none(c, tab);\n    let _ = turn(c, tab, text);"
        ))
        .is_empty(),
        "a handed one is not"
    );
    assert!(
        unleased_typing(&synthetic("    // turn_fenced(c, tab, text, None)")).is_empty(),
        "a comment types nothing"
    );
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

/// A READY answer not yet recorded, driven through the real visit. The notice
/// went out, recorded by Claude Code wrapped as a paste. The agent's answer
/// (several paragraphs, `ATERM-UPGRADE-READY-d6476091` its last line) is on
/// the idle screen, and the window's host takes the step 180 ms later, over a
/// transcript Claude Code has not appended the row to yet (a batched queue:
/// `FLUSH_INTERVAL_MS=100`, then an async append).
///
/// (1) The READY on the idle screen, its row unwritten, holds the point
/// (`wait:ready-unrecorded`, owned: nothing is typed over it). Negative
/// control: the same look with no READY on the screen waits `awaiting-ready`,
/// owning nothing. (2) The row appended, the READY is heard: the restart's
/// settle holds the point, the answer's clock running. (3) A person's message
/// after that look, taken up by the agent: its next idle point is the
/// re-ask's, said `superseded` while the turn settles, and owned. (4)
/// Settled, the notice is typed again at once (a new ask, with a marker of its
/// own), not after the re-ask's half hour. The agent is never signalled.
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

    // (3) Someone directed the agent after that look, and it took that up:
    // its next idle point shows its new answer. The re-ask is due there,
    // said `superseded` while the turn settles, and owned.
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

/// THE QUOTA WALL A CONVERSATION STANDS AT, READ FROM ITS TRANSCRIPT'S TAIL
/// ([`named_standing_wall`]): the reader of [`upgrade::transcript_standing_wall`]
/// over the same BOUNDED read as [`named_limit_reset`] ([`TAIL_BYTES`]), so a
/// transcript megabytes long costs one tail — and a wall the tail no longer
/// holds is not read (the stated bound). NEGATIVE CONTROLS: another
/// conversation, and a home with no transcript, have none.
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
    assert_eq!(named_standing_wall(&home, SESSION), Some(read));
    assert_eq!(
        named_limit_reset(&home, SESSION),
        Some(1_790_870_400),
        "the same row"
    );
    // NEGATIVE CONTROLS.
    assert_eq!(
        named_standing_wall(&home, "00000000-0000-4000-8000-000000000000"),
        None
    );
    assert_eq!(named_standing_wall(&dir.join("nowhere"), SESSION), None);
    // THE BOUND: more than a tail of rows after it, and it is not read.
    let mut rows = vec![wall];
    rows.extend(vec![filler; 80]);
    assert!(rows[1..].iter().map(String::len).sum::<usize>() as u64 > TAIL_BYTES);
    write_transcript(&home, &rows);
    assert_eq!(named_standing_wall(&home, SESSION), None);
}

/// THE LINE A CONVERSATION'S USAGE-LIMIT DIALOG ENDED WITH, READ FROM ITS
/// TRANSCRIPT'S TAIL ([`named_reset_line`]): the reader of
/// [`upgrade::transcript_reset_line`] over the same bounded read as
/// [`named_standing_wall`]. THE RESET ROW IS COMPOSED, never captured: the bundle's text on the two rows measured for this dialog's
/// wait row (`upgrade_stall_tests.rs`); OPEN: replace with a capture.
/// NEGATIVE CONTROLS: asked since a later second, another conversation and
/// a home with no transcript have none.
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
        named_reset_line(&home, SESSION, claimed).as_deref(),
        Some(said)
    );
    // NEGATIVE CONTROLS.
    assert_eq!(named_reset_line(&home, SESSION, claimed + 60), None);
    assert_eq!(
        named_reset_line(&home, "00000000-0000-4000-8000-000000000000", claimed),
        None
    );
    assert_eq!(
        named_reset_line(&dir.join("nowhere"), SESSION, claimed),
        None
    );
}

/// THE ACCOUNT MOVE WHOSE RESTART SIGNALLED AN AGENT IS NAMED BY ITS LABEL
/// ([`super::super::relaunch::account_restart`]): a record in flight whose
/// cause is an account move's, naming that very process in that tab — and
/// one that STOPPED a short while after its agent was seen gone, which is
/// still that exit's. What the window's host reads to tell a move left in
/// flight (its signal sent) from one never begun, whether or not the
/// signalled process has gone yet. NEGATIVE CONTROLS: the upgrade's own
/// restart and the memory banner's (the harness signalled the agent, but
/// for no account move); another process; another tab; no record at all;
/// and a stop long past its exit.
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

    // NEGATIVE CONTROLS.
    for cause in ["", "memory", "model:claude-sonnet-5-5"] {
        save(&opts, SESSION, &record(cause, exiting.clone(), 0));
        assert!(
            restarted(&opts, false, Some(pid)),
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

/// A CONVERSATION IS KEPT UNDER THE HOME ITS TRANSCRIPT IS FILED IN
/// ([`conversation_under`]): what a host with no process to read decides
/// "the agent's Claude home was the default one" on. NEGATIVE CONTROLS:
/// another conversation; a home with no projects, or none at all (a Claude
/// Code under another home files its conversations there); a name that is
/// no session id — one that would walk out of the projects directory to a
/// file that IS there included; and a directory of the transcript's name.
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

/// THE TRANSCRIPT A SUPERVISOR CONFIRMS A WALL BY, FOUND FROM THE PROCESS
/// (H6-HARNESS-1; the review of pass 3 measured the check OFF for a Claude
/// Code launched with its own `CLAUDE_CONFIG_DIR`, whose record and
/// transcript the host looked for under `~/.claude` alone). Found now as the
/// window's footer finds a process's conversation
/// ([`process_transcript_tail`]): under the process's own
/// `CLAUDE_CONFIG_DIR`, through the record that directory keeps of its pid,
/// its own project's transcript first. Under that config directory the
/// tail is the alternate conversation's, though the home's `.claude` keeps
/// a record of the same pid naming another; with no config directory, the
/// home's. NEGATIVE CONTROLS: a record whose `procStart` is not the
/// process's (a reused pid) names nothing; two transcripts of the id are
/// settled by the project's own place, and with that place empty they are
/// none (one planted elsewhere can neither stand in for the conversation
/// nor be picked over it), while one elsewhere alone is read; no transcript,
/// and no record of the process: none.
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
        process_transcript_tail(config.and_then(Path::to_str), home.to_str(), 4242, at)
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
    // No transcript: none.
    std::fs::remove_file(alt.join("projects/-moved").join(format!("{alt_id}.jsonl")))
        .expect("moved");
    assert_eq!(tail(Some(&alt), start), None, "no transcript");
    // No record of the process: none.
    std::fs::remove_file(alt.join("sessions/4242.json")).expect("record");
    assert_eq!(tail(Some(&alt), start), None, "no record");
}

/// …READ FROM THE LIVE PROCESS'S OWN EXEC ENVIRONMENT
/// ([`snapshot_transcript_tail`], what the window's host asks; the review of
/// pass 4 dropped its `CLAUDE_CONFIG_DIR` and no test failed): a stand-in
/// agent launched with `CLAUDE_CONFIG_DIR` naming a directory of its own and
/// `HOME` another, each keeping a record of its pid that names a different
/// conversation — the tail is the config directory's, read from the
/// process's environment, never the home's `.claude`. The same stand-in
/// launched with no `CLAUDE_CONFIG_DIR` reads its home's.
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
    let tail_of = |config: Option<&Path>| {
        let mut c = parked();
        c.env("HOME", &home);
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
        };
        let tail = snapshot_transcript_tail(&snap);
        let _ = agent.kill();
        let _ = agent.wait();
        tail
    };
    assert_eq!(
        tail_of(Some(&alt)).as_deref(),
        Some("{\"the\":\"alt tail\"}\n"),
        "the process's own CLAUDE_CONFIG_DIR"
    );
    assert_eq!(
        tail_of(None).as_deref(),
        Some("{\"the\":\"home tail\"}\n"),
        "none: its HOME's .claude"
    );
}

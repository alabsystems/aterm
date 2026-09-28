// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::relaunch::{
    AtPrompt, CAUSE_EXIT, CAUSE_HOST, CONTINUE_FENCE_TRIES, ENDED_AT_ONCE, EXIT_LOOK, EXIT_SETTLE,
    ExitRecord, HELD_WAIT_IN_TEST, MODEL_WAIT, RELAUNCH_FENCE_TRIES, RelaunchLineError, Restart,
    STALE_S, Snapshot, after_exit, after_host_ended, await_new, carry_on_with_tab_probe,
    continuation_session, exit_record, line_for, look_at_exit, owed, relaunch_by_parent,
    restart_from, resume, resumed_prompt, shell_dialect, type_relaunch_line,
};
use super::super::upgrade::Dialect;
use super::*;
use std::time::{Duration, Instant};

/// `@s-b5cf… text --json trim tail=3`, measured 2026-09-23 on a busy Claude
/// Code 2.1.280 tab: the cursor row is ABSOLUTE (57) and `first` is where the
/// reply's rows start, so the caret sits on reply row 0, column 2.
const SCREEN: &str = r#"{"rows":["❯","────────","  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt · ← for agents"],"cursor":{"row":57,"col":2,"visible":true,"style":"blinking_block"},"dims":{"rows":60,"cols":144},"seq":232803,"trimmed":0,"first":57}"#;

/// `@s-b5cf… status`, measured the same minute.
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

/// A person at the keyboard wins (the owner's rule, 2026-09-24): a tab a
/// person typed into within `[harness] human_grace_s` is held like a hand on
/// it. NEGATIVE CONTROLS: a keystroke older than the grace, `human_ms=-` and
/// no field at all (an older server) are nobody.
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

/// ND1 OF THE LIVE RE-TEST OF 2026-09-26: the harness's OWN hand on a tab
/// whose agent it restarts ([`Hand`]: `hand=lease:aterm-harness@<this
/// pid>`) holds nothing against itself — its last looks, the relaunch line
/// and its supervisor loops read on — while ANOTHER holder's lease, a named
/// driver's turn, and INPUT its program has not read yet (a first prompt on
/// its way, which a signal would drop; a shell's queued lines) hold the tab.
/// NEGATIVE CONTROLS: another process's harness hand is someone else's, and
/// `input=clear`/`-` holds nothing.
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
/// The unknown/Claude arm is the negative control — it must keep the roster
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

/// A BUILD THAT DID NOT ANSWER DECIDES NOTHING (the 2026-09-27 review:
/// nothing held that `due` says so — reverting its arm to a plain `No` left
/// every test green). The tab's Claude Code runs 1.0.0, and the managed
/// twin's `--version` timed out: the version cache ([`cached_version_with`],
/// the product's own) holds it unanswered until its back-off, [`Targets`]
/// carries that, and the look is [`UNREAD_VERSION`] — the host looks again
/// on its unread ladder — never "not due". Asked again before the back-off,
/// it says the same and probes nothing. NEGATIVE CONTROLS: once the build
/// answers, it decides — `No` for a session on it, `Yes` for one behind it;
/// and a record of ANOTHER tab's agent is no record of this one's
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
            hold_since_s: 1_790_000_060,
            hold_seen_s: 1_790_000_120,
            ..St::fresh(&from, &to, 1_790_000_000)
        };
        assert_eq!(St::from_json(&st.to_json()), Some(st));
    }
    assert_eq!(St::from_json(r#"{"phase":"sideways"}"#), None);
}

/// ONE READER FOR BOTH WRITERS: a state origin/main's writer produced (the
/// model half's fields, none of the owner's view's: no `prompt`,
/// `pending_since`, `wait`, `request`, `request_tab`, `outcome`, `done_at`)
/// reads with those at their defaults — the age falls back to
/// `salt`, no owner's word — and keeps every field it did write. NEGATIVE
/// CONTROL: the same text re-written by this build round-trips.
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

/// CLAUDE CODE'S OWN KEEP-AWAKE IS NOT WORK (the end-to-end run of 2026-09-26,
/// D4). Claude Code starts `caffeinate -i -t 300` as its OWN child at every
/// turn ("Started caffeinate to prevent sleep", its 2.1.283 binary) and stops
/// it some 30 s after the turn ends; counted as the agent's background work, it
/// held every restart of that run 82-85 s past the READY answer, and 300 s more
/// whenever it outlasted the second look. The tree is the one `ps` read in that
/// run's scenario B at the READY answer (18:40:48: Claude 4163 under the tab's
/// zsh, its caffeinate 8931): nothing there is work, and the restart goes.
///
/// NEGATIVE CONTROLS, each still holding the restart: a `caffeinate` a Bash
/// tool started (under the tool's shell); the python a Bash tool left running
/// under the agent, beside Claude's own keep-awake (scenario B at 18:36:48:
/// zsh 4492 and its Python 4494 next to caffeinate 4444) — only the
/// keep-awake is let go, not every direct child; a shell Claude's keep-awake
/// itself runs (the walk goes on below it); and a caffeinate some helper of the
/// agent's started, which is not the agent's own.
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

    // Measured at READY: Claude's own keep-awake alone.
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

    // Measured at 18:36:48: the python a Bash tool left running, beside
    // Claude's own keep-awake — the tool's shell still holds the restart.
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

/// THE PROCESSES THE NOTICE NAMES (2026-09-26): two background poll loops,
/// under a Claude Code the upgrade could not move for four days, as `ps`
/// printed them. The notice quotes the `eval` payload and drops the Bash
/// tool's boilerplate. Their pids come from the same walk as the gate's.
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

/// THE RESTART'S LAST LOOK FAILS CLOSED (audit of 2026-09-26). A process
/// table `ps` could not give is empty, and over an empty table the walk
/// answers "nothing runs". That let the one irreversible act pass on no
/// evidence. An empty table now reads as work.
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

/// A SESSION FILE ITS DEAD OWNER LEFT BEHIND HOLDS NOTHING: the restart it
/// names is still the orphan pass's to carry on. Skipping on the file alone
/// stranded it — `visit` acts on live processes only (`wait:stale-file`).
/// Control: the same file with a LIVE owner is `visit`'s, and skipped here.
#[test]
fn a_dead_owners_leftover_session_file_does_not_strand_the_restart() {
    let dir = std::env::temp_dir().join(format!("aterm-upgrade-leftover-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: true,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
    };
    let mut child = Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
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
    let dir = std::env::temp_dir().join(format!("aterm-upgrade-orphan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("home");
    // A pid that WAS a process and is not one now.
    let mut child = Command::new("true").spawn().expect("spawn");
    let dead = child.id();
    child.wait().expect("reap");
    let opts = Opts {
        home,
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: true,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-upgrade-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

#[test]
fn a_second_sweeper_does_nothing_while_the_first_holds_the_lock() {
    let dir = scratch("lock");
    std::fs::create_dir_all(dir.join("home/.claude/sessions")).expect("home");
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
    };
    let first = sweep_lock(&opts).expect("free").expect("a real run locks");
    let got = sweep(&opts);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].step, "busy:another-sweep");
    assert!(!got[0].is_act());
    // Released by `LOCK_UN` (the guard's drop), so the very next sweep runs. A
    // child another test thread spawns at this instant shares the lock's open file
    // description until its exec, and these tests spawn `ps`, `lsof` and stand-in
    // agents throughout: released by the close alone, the lock read held after the
    // drop in 2 runs of 20 before the stand-in tests and most runs after (measured
    // 2026-09-23). `LOCK_UN` strips it from the description itself, copies and all.
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
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
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
    // prompt): refused at the announcement, the notice never typed. It used
    // to be refused only at the signal — after the agent had been told to
    // wind down and had answered READY.
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

    // NEGATIVE CONTROL: an everyday launch IS announced, once, through the same
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

/// THE DRAIN'S BOUND, END TO END OVER THE REAL STATE: an answered announcement
/// whose restart a box nobody answers has held past [`upgrade::DRAIN_S`] is voided
/// by the sweep's own arm — the READY answer still in the transcript no longer
/// counts ([`answered`], whose pre-void answer is the negative control), the
/// ledger says why, the phase keeps its asks, and the reducer then asks again at
/// the next idle point instead of restarting on the stale answer. A dry run says
/// it and changes nothing.
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
    // A lane that types no release (Codex) says nothing of one (the second
    // review of 2026-09-26: the line said "the agent is released" there too).
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
    // no notice follows on the heels of the void — the release goes first
    // ([`upgrade::gate_release`]), and the next notice REASK_S after the void
    // (the box, while it stands, is only a wait for either).
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

/// A SWEEP LOCK IS FREE THE MOMENT ITS GUARD DROPS, whatever copies of its
/// descriptor live on. `copy` stands in for a child another thread is
/// mid-spawning, which holds every descriptor until its exec: were the lock
/// released by the close alone, the copy would keep it held and the next sweep
/// would read `busy:another-sweep` — the 2026-09-24 landing-gate failure, which a
/// 100 ms poll waited out only for a spawn faster than the poll. A regression of
/// [`SweepLock`] to a bare `File` fails the assertion after the release. Negative
/// control first: a lock a sweep holds refuses.
#[test]
fn a_dropped_sweep_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = scratch("lock-window");
    let opts = Opts {
        home: dir.join("home"),
        state: dir.join("state"),
        sock: None,
        only_sid: None,
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
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
/// for one WITH A TASK (D1 of the live E2E of 2026-09-26: a record of the
/// harness's own turns alone is restarted afresh, never announced to, and is
/// carried on with nothing).
const ASKED: &str = r#"{"type":"user","message":{"role":"user","content":"fix the bug"}}"#;

/// The stand-in agent's argv: THIS test binary, running only [`park_when_asked`].
const PARK: [&str; 2] = ["harness::upgrade_drive::tests::park_when_asked", "--exact"];

/// What tells [`park_when_asked`] to park.
const PARK_ENV: &str = "UPGRADE_DRIVE_TEST_PARK";

/// The stand-in agents' body: idle unless asked to park.
#[test]
fn park_when_asked() {
    if std::env::var_os(PARK_ENV).is_some() {
        std::thread::sleep(Duration::from_secs(30));
    }
}

/// A stand-in agent in [`TAB`]: this test binary, parked. Not `/bin/sleep`: a
/// platform binary's environment is hidden from `KERN_PROCARGS2`, a real
/// agent's is not (measured by the audit).
fn parked() -> Command {
    let mut c = Command::new(std::env::current_exe().expect("exe"));
    c.args(PARK)
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    c
}

/// Wait until `pid` runs this test binary (a fork not yet exec'd still reads
/// as its parent).
fn wait_exec(pid: u32) {
    let me = std::env::current_exe().expect("exe");
    for _ in 0..500 {
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
    let start = kernel_start(pid).expect("lstart");
    std::fs::create_dir_all(home.join(".claude/sessions")).expect("sessions");
    std::fs::write(
        home.join(format!(".claude/sessions/{pid}.json")),
        format!(
            r#"{{"pid":{pid},"sessionId":"{session}","cwd":"/","version":"1.0.0","status":"idle","statusUpdatedAt":1,"procStart":"{start}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("session file");
    session_file_of(home, pid).expect("the file parses")
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
    assert_eq!(report.step, "wait:notice-owned-by-other-process");
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
        continuation_session(&opts.home, sf_b.pid, SESSION),
        Some(sf_b.clone())
    );
    let other = "0badf00d-1111-2222-3333-444455556667";
    register(&opts.home, sf_b.pid, other);
    assert!(continuation_session(&opts.home, sf_b.pid, SESSION).is_none());
    b.kill().expect("stop B");
    b.wait().expect("reap B");
    let _ = std::fs::remove_dir_all(dir);
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
    let sibling_pid = dead_pid();
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
    }
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

/// The READY answer is read whatever byte the tail window starts on — also on
/// the SECOND byte of `é` (C3 A9), where a strict decode refused the whole
/// window, left it empty, and the answer read as silence.
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

/// `cell` replies measured 2026-09-23 on s-b5cf… under Claude Code 2.1.281,
/// read only: the placeholder's first letter at the caret row's column 2, the
/// caret glyph, the no-break space after it, a blank past the text.
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

/// THE COMPOSER CHECK trusts only an `OK … dim` at the caret row's column 2.
/// The placeholder is empty; the SAME rows and cursor with the text typed and
/// the caret moved home (←, Home, ctrl-a — measured 2026-09-23: the cursor back
/// at column 2, the cell `none`) are a draft, which the cursor alone read as
/// empty and the notice was pasted in front of; an unreadable cell is a draft.
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
        "the caret row's column 2, in the screen rows `cell` takes"
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

// ---------------------------------------------------------------- the terminal

fn tty_rows(rows: &[(u32, u32, &str, &str)]) -> Vec<(u32, u32, String, String)> {
    rows.iter()
        .map(|(p, pp, tty, name)| (*p, *pp, (*tty).to_string(), (*name).to_string()))
        .collect()
}

/// THE TERMINAL PROOF over process tables: the program that opened the agent's
/// terminal is an aterm, or gone (the tab handed on) — never a multiplexer,
/// `script`, or nothing.
#[test]
fn a_terminal_is_the_tabs_only_when_aterm_opened_it() {
    // Measured 2026-09-23, read only: this machine's live agent, its `-zsh`, and
    // launchd — the aterm that opened the tab handed it on and exited.
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
    // Measured the same day: a program under `script`, on the pty it opened.
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

/// END TO END: an agent on a pty the tab does not own — `script`'s here, the
/// shape of every tmux or screen pane, whose environment still names the tab —
/// is never driven through that tab. The sweep stops before any socket is
/// dialled (unguarded, it reached the socket, which does not exist here), and
/// says why ONCE — in the ledger and as an act the window's host logs — then
/// waits in silence.
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("script");
    let owner = script.id();
    let agent = (0..250)
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

/// [`Job`] from `ps`'s numbers, measured shapes first, then real processes.
#[cfg(unix)]
#[test]
fn only_its_shells_foreground_job_is_ever_announced_to_or_ended() {
    use std::os::unix::process::CommandExt as _;
    // Measured 2026-09-23 (`ps -o pid,ppid,pgid,tpgid`): the live agent and
    // its `-zsh`.
    assert_eq!(job(4205, 4205, 4205, 1916), Job::Foreground);
    // The same agent suspended or backgrounded: the shell holds the terminal.
    assert_eq!(job(4205, 4205, 1916, 1916), Job::Background);
    // Measured by the audit on a pty: a bash launcher (`cc.sh`, no `exec`) and
    // its agent share one group, and it is the terminal's.
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

/// END TO END: an agent no job-control shell started (a launcher script that
/// runs `claude` without `exec`, `sh -c`, `make`, an IDE task) is refused BEFORE
/// anything is typed, once, in the ledger — never announced to, never ended
/// with nothing to relaunch it (unguarded, the sweep went on toward the tab).
#[cfg(unix)]
#[test]
fn an_agent_no_job_control_shell_started_is_refused_before_anything_is_typed() {
    use std::os::unix::process::CommandExt as _;
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
/// job for its first `foreground` job reads and a background job after — a
/// ctrl-z landing between two looks, which no real process does on cue — its
/// terminal the tab's (the tab handed on, as measured live), and its parent
/// `parent`.
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
    /// The `status` reply from the SECOND read after a `lease acquire` on:
    /// what a person's keystroke, or a prompt someone sent, makes it between
    /// a restart's first look (under the hand) and its last.
    after_first_look: Option<&'static str>,
    /// The `signal` reply, sending nothing (empty: the stand-in agent named
    /// by `pid=` is sent SIGTERM, then `OK`).
    signal: &'static str,
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
        }
    }
}

/// A control socket at `<dir>/t.sock`, its token beside it, standing in for
/// the aterm instance of [`TAB`] with an idle Claude in it: `sessions` names the
/// tab, `text --json` is [`idle_screen`], a `cell` is a plain blank, `status`
/// holds nothing (or answers what `<dir>/t.status` says, when a test writes
/// it), and a `turn` is answered OK — recorded here, typed nowhere.
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
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&asked);
    std::thread::spawn(move || {
        let mut turn_replies = answers.turn.clone();
        let mut texts = 0;
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
                    "text" => {
                        texts += 1;
                        match &answers.after {
                            Some((n, later)) if texts > *n => format!("OK {later}"),
                            _ => format!("OK {}", answers.screen),
                        }
                    }
                    "cell" => "OK %20 d0d0d0 111318 none".to_string(),
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
                        next.to_string()
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
    let mut a = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("agent A");
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

/// THE HAZARDS REVIEW OF 2026-09-25 (major): the continuation a relaunched
/// agent is owed was typed with no look for a person — at its first idle
/// point, just when a person who saw the crash is at the keys. A person's
/// keystroke within the grace (`human_ms=`) now holds it (`wait:held`),
/// nothing typed and the restart still in flight, so the next idle point
/// types it. NEGATIVE CONTROL: with nobody at the keys it is typed, with
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
    );
    assert_eq!(report.step, "wait:held");
    assert_eq!(turns(&asked), 0, "nothing typed under a person's hands");
    assert!(matches!(st.phase, Phase::Relaunched { .. }));
    // NEGATIVE CONTROL: nobody at the keys.
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
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        None,
        &mut None,
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
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude --resume",
        None,
        &mut None,
        |_, _, _| true,
    )
    .expect("unchanged tab can receive a relaunch");
    assert_eq!(turns(&asked), 1, "positive control reaches the turn");
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// ND1: THE RELAUNCH LINE IS TYPED UNDER THE HARNESS'S HAND, never let go
/// for it (the review of the first cut: it gave the hand back for the line
/// and took it again, and a retrying orchestrator's `turn` could win either
/// gap — into the bare shell before the line, into the booting agent after
/// it). The hand is the server's HARD lease ([`Hand`]: `lease acquire …
/// hard`), which refuses every other connection's writes and lets its own
/// connection's through, its `turn` included: between the hand taken and
/// the line typed nothing is released or re-taken — for bash's raw `send`
/// and zsh's fenced `turn` alike. A turn refused `ERR busy …` (a hand that
/// could not be taken, or lapsed) is a WAIT, never the refusal that fails a
/// relaunch whose agent is already gone. NEGATIVE CONTROL: from a server
/// that takes no hard lease (an older aterm), the hand holds nothing and is
/// given back by nothing.
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
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude --resume",
            None,
            &mut None,
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
    // NEGATIVE CONTROL: an older server, no hard lease — nothing to give back.
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

/// ND1: THE HAND OUTLASTS EVERY WAIT UNDER IT (the review of the first cut:
/// taken once, with a 60 s life, it lapsed silently under a slow `--resume` —
/// the new process may take 90 s to register, its first idle 15 s more).
/// Each wait under it keeps it ([`Hand::keep`]): renewed once [`HAND_RENEW`]
/// has passed since it was last taken, and not before; a hand that holds
/// nothing is never renewed; and the relaunch keeps it through its waits —
/// the exit, the shell's prompt, the new process, its first idle.
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
    // Every wait of the relaunch under the hand keeps it — Claude Code's and
    // Codex's.
    for (src, waits, for_new) in [
        (
            include_str!("relaunch.rs"),
            ["wait_exit(", "let prompt_back = wait_until("],
            "wait_until(held_wait(&st.cause), || {",
        ),
        (
            include_str!("upgrade_codex_drive.rs"),
            [
                "wait_until(EXIT_WAIT,",
                "wait_until(Duration::from_secs(15), || k.shell_has_terminal",
            ],
            "wait_until(Duration::from_secs(90), || {",
        ),
    ] {
        let body_of = |head: &str| -> &str {
            let at = src.find(head).unwrap_or_else(|| panic!("{head}"));
            let body = &src[at..];
            &body[..body.find("\n}\n").unwrap_or(body.len())]
        };
        let held = body_of("\nfn relaunch_held(\n");
        for wait in waits {
            let at = held.find(wait).unwrap_or_else(|| panic!("{wait}"));
            assert!(held[at..].contains("hand.keep(c);"), "{wait}");
        }
        let awaited = body_of("\nfn await_held(\n");
        let wait = awaited.find(for_new).expect("the wait for the new process");
        let closure = &awaited[wait..];
        let closure = &closure[..closure.find("});").expect("its end")];
        assert!(closure.contains("hand.keep(c);"), "{closure}");
        assert!(awaited.contains("first_idle(c, hand,"), "{awaited}");
    }
    let src = include_str!("relaunch.rs");
    let first = &src[src.find("fn first_idle(").expect("first_idle")..];
    assert!(first[..first.find("\n}\n").expect("its end")].contains("hand.keep(c);"));
}

/// THE LAST LOOK BEFORE THE RELAUNCH LINE is a `status` read at the moment
/// it is typed, after every wait before it: a person who typed at the
/// returned prompt within the grace, a halt and anyone's hand each hold the
/// tab, and nothing is typed — the line is never joined to a person's
/// half-typed command. NEGATIVE CONTROL: a keystroke older than the grace,
/// no hand, no halt — the line is typed.
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
            &mut c,
            shell,
            TAB,
            SESSION,
            "claude",
            None,
            &mut None,
            |_, _, _| true,
        );
        assert_eq!(result, Err(RelaunchLineError::Wait("held")), "{held}");
        assert_eq!(turns(&asked), 0, "{held}: nothing typed");
    }
    std::fs::write(&status, "OK schema=1 hold=0 hand=- human_ms=120000").expect("status");
    type_relaunch_line(
        &opts,
        &mut c,
        shell,
        TAB,
        SESSION,
        "claude",
        None,
        &mut None,
        |_, _, _| true,
    )
    .expect("nobody at the tab");
    assert_eq!(turns(&asked), 1, "negative control: typed");
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// AT A BASH PROMPT THE RELAUNCH LINE IS TYPED, NOT PASTED: a crashed agent
/// leaves the terminal's bracketed-paste mode on, and macOS's bash 3.2 reads
/// the paste's frame as text (measured 2026-09-25: `--resume <id>01~`). Raw
/// bytes (`send --`), then Enter. NEGATIVE CONTROL: a shell that is not
/// bash (this test binary standing in) is pasted to with a `turn`, as zsh
/// and fish are.
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
    for _ in 0..200 {
        if atpkg::caller_shell::process_args(bash.id())
            .is_some_and(|a| Dialect::from_exe_name(&a.exec_path) == Some(Dialect::Bash))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    type_relaunch_line(
        &opts,
        &mut c,
        bash.id(),
        TAB,
        SESSION,
        line,
        None,
        &mut None,
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
        &mut c,
        std::process::id(),
        TAB,
        SESSION,
        line,
        None,
        &mut None,
        |_, _, _| true,
    )
    .expect("pasted elsewhere");
    assert_eq!(turns(&asked), 1, "negative control: pasted");
    let _ = bash.kill();
    let _ = bash.wait();
    drop(c);
    let _ = std::fs::remove_dir_all(dir);
}

/// EVERY LOOK AT THE JOB before an act is the one standing between a ctrl-z and
/// that act. The agent is its shell's foreground job at the first look and a
/// background job from the Nth read on; each N stops the sweep at exactly one
/// look, and nothing is typed or signalled: the re-read before the announce
/// (N = 1, on a pending upgrade), the same re-read before the SIGTERM (N = 1),
/// restart's own gate (N = 2), and the last look before the signal (N = 3).
/// Dropped, each lets the act through — the announce is typed, or the next
/// look answers another word. The control proves the path is live: with every
/// read foreground, the announce IS typed.
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
    let mut agent = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
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
        // Measured 2026-09-23: a person's fresh `claude` at that prompt after a
        // relaunch that failed — the shell's child, another conversation.
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

/// END TO END, the in-flight branch: the new process holding the conversation
/// fails the upgrade for good ONLY from a parent read as another than the
/// recorded shell. Unread, it is a wait — the phase kept, nothing in the
/// ledger, the next sweep asks again — where it was recorded as
/// `resumed-elsewhere` with a ledger line saying a process this upgrade did not
/// relaunch held the conversation, and the real relaunch was never told to
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

/// THE HOST'S NEXT MOVE after one step: park again at the next idle point
/// after an announcement (the answer comes at a turn's end), look again on a
/// growing pause after any wait — every minute while a person's hold stands
/// ([`HOLD_LOOK`]) — say a state that cannot hold the lock ONCE,
/// and stop for this build on anything final. NEGATIVE CONTROL: another
/// actor on the lock is a wait, not a fault.
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
    // last word (the 2026-09-26 incident: after `gave-up` the host never
    // looked again, and a READY given at 06:02 was read by nobody): the agent
    // is owed its release and a late READY is honoured, both at the next idle
    // point. A release typed is looked at again like any wait, and so is one
    // still owed (`wait:release:<why>`).
    assert_eq!(after("gave-up", 0), After::NextIdle);
    assert_eq!(after("drain-expired:box", 0), After::NextIdle);
    assert_eq!(after("released:gave-up", 0), After::Later(LATER[0]));
    assert_eq!(after("wait:release:limited", 2), After::Later(LATER[2]));
    // A stopped round says so as it stops (`failed:<why>`, `refused:<what>`)
    // and, resting with nothing owed, `wait:failed` (an owed release reads
    // `wait:release:<why>`, and a Codex record's stop `wait:failed:<why>`) —
    // NONE a last word (the owner, 2026-09-27: "you should NEVER have
    // upgrades stalled"): a new round starts once it has rested
    // `upgrade::RETRY_S`, so each is looked at again, up to the ten-minute
    // look; so is the new round's `rearmed:<why>`, whose first notice the
    // next look types.
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

/// WHETHER A STEP MOVED THE SESSION (review of 2026-09-26), AND WHETHER IT
/// TYPED A TURN (N1 of the live E2E of 2026-09-26): a step that typed into
/// the agent or ended it leaves no point for the loop to decide again — the
/// typed carry-on was followed by the old point decided at once, while the
/// agent began its answer — and one that TYPED a turn (a notice, a carry-on)
/// is awaited as the harness's own, whose answer is no work of the worker's.
/// NEGATIVE CONTROLS: a step that ended the agent typed no turn into it; and
/// every word that typed nothing and ended nothing — a carry-on's model read
/// later (`done`, `done:model-unconfirmed`) and a daemon-mode Codex carried
/// on with nothing typed (`done`) among them — leaves the point standing.
#[test]
fn a_step_that_typed_or_ended_the_agent_moved_the_session() {
    for turn in ["announced:1", "announced:3", "continued"] {
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
    ] {
        assert!(!moved(left) && !typed(left), "{left}");
    }
}

/// THE UPGRADE OWNS A SESSION'S TURN ENDS BY ITS OWN STEP'S WORD
/// ([`owns_turn_ends`], what the window's host answers its loop with): after
/// the announcement, and while READY is given and the restart's gate only
/// settles or drains background work — bounded — and never after an answer
/// without READY, a hold, a release still owed, or a last word. NEGATIVE
/// CONTROLS: past the bounded looks a drain or a settle owns nothing, and a
/// release owed owns nothing whatever holds it (the review of 2026-09-26: a
/// late READY about to restart the agent read `wait:release:ready`, which
/// owned nothing, and the supervisor could type over the answer).
#[test]
fn the_upgrade_owns_turn_ends_only_while_its_step_says_so() {
    assert!(owns_turn_ends("announced", 0));
    assert!(owns_turn_ends("announced:2", 0));
    assert!(owns_turn_ends("wait:settling", 0));
    assert!(!owns_turn_ends("wait:settling", OWNED_SETTLE_LOOKS));
    assert!(owns_turn_ends(
        "wait:background",
        OWNED_BACKGROUND_LOOKS - 1
    ));
    assert!(!owns_turn_ends("wait:background", OWNED_BACKGROUND_LOOKS));
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
        assert!(!owns_turn_ends(last, 0), "{last}");
    }
}

/// THE RELEASE IS TYPED ONLY WHERE IT IS THE UPGRADE'S NEXT ACT
/// ([`release_is_next`]), and a step that leaves one owed never reads as the
/// host's last word ([`owed_word`]). The upgrade's own give-up, void and
/// stop, and its waits on nothing of its own, leave the release next; every
/// wait of the notice's or the restart's gate keeps its word — what it
/// waits for goes first. A stop's own word, `failed:…` / `refused:…`, is
/// the host's last word ([`After::Finished`]), so one that leaves a release
/// owed says `wait:release:<why>` instead (the review of 2026-09-26: a
/// release owed by a refused signal was never looked for again). NEGATIVE
/// CONTROLS: an act the host looks on after (`gave-up`, `drain-expired:…`)
/// keeps its word.
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

/// [`due`] parks a session only when it has an upgrade to take — a restart
/// of THIS tab in flight is one — and says what it could not read rather
/// than "not due" (the live re-test of 155c72a28: a look whose socket did
/// not answer let the upgrade go for good). NEGATIVE CONTROLS: another
/// tab's restart, and a finished one, are not `Yes` — with no socket to read
/// the tab by, they are `Unread(no-socket)`, never `No`.
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

/// A TAB WHOSE AGENT'S RECORD IS NOT THERE DECIDES NOTHING ([`due_among`]):
/// Claude Code writes `sessions/<pid>.json` a moment after it starts, so a
/// look at a launch before then reads [`UNREAD_RECORD`] — never "not due"
/// (N3 of the live re-test of 155c72a28: the record 1.2 s after the attach
/// that asked, and nothing asked again before the first idle point). Once
/// the record leads the tab it decides: behind is `Yes`, current is `No`.
/// NEGATIVE CONTROL: a record leading ANOTHER tab is no record of this one.
#[test]
fn a_tab_whose_agent_has_no_record_yet_is_unread_never_not_due() {
    let dir = scratch("due-record");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
    let tabs = the_tab();
    let among = |files: &[SessionFile], group: i64| {
        due_among(
            &opts,
            files,
            &tabs,
            || Targets::read(&opts.home),
            |_| Some(atpkg::caller_shell::ProcArgs::default()),
            |_| Some(group),
            |_| Some((10, group, group)),
        )
    };
    assert_eq!(
        among(&[], 101),
        Due::Unread(UNREAD_RECORD),
        "not written yet"
    );
    assert_eq!(
        among(&[record("1.0.0")], 202),
        Due::Unread(UNREAD_RECORD),
        "another tab's agent's record is none of this tab's"
    );
    // Newer than any build it could move to: a current session.
    assert_eq!(
        among(&[record("999.0.0")], 101),
        Due::No,
        "read, and current"
    );
    // A version nobody can compare takes the full path, as a behind one does.
    assert_eq!(
        among(&[record("unknown")], 101),
        Due::Yes,
        "read, and behind"
    );
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
    // NEGATIVE CONTROLS.
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

/// A LAUNCH'S RECORD IS WAITED ON FOR ITS LAUNCH, NOT FOR GOOD ([`no_record`],
/// the review of the silent-drop fix): no record of the tab's agent is a wait
/// ([`UNREAD_RECORD`]) while the agent — the tab's foreground group's leader —
/// is younger than [`RECORD_WINDOW_S`], and nothing to take once it is older:
/// a Claude Code that keeps its records elsewhere (`CLAUDE_CONFIG_DIR`), or
/// whose session is no interactive CLI one, never writes one this look can
/// find, and read as a wait for good it was looked at again at every turn end,
/// for good. A leader whose start cannot be read is a process not read while
/// it lives, and nothing once it is gone; a tab the instance no longer has is
/// nothing. And a machine with no `.claude/sessions` at all holds no records —
/// the same window decides — while one whose records cannot be read whole is
/// unread.
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
    std::fs::write(dir.join(".claude/sessions/101.json"), "{\"pid\": 1").expect("half");
    assert_eq!(claude_records(&dir), None, "caught half-written: unread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tier-1 of `HarnessUpgradeLook`, ITS CLASSIFIER HALF (the review of the
/// silent-drop fix: the host's bind — aterm-gui's
/// `the_real_look_conforms_to_the_upgrade_look_model` — fakes `due` from the
/// model's `readable` bit, so it binds the host's reaction to a verdict, not
/// the reads the bit stands for). Here the real [`due_among`] and
/// [`no_record`] are driven over EVERY combination of what a look reads of
/// one tab — its agent's record there or not, behind or current, its group,
/// its ids and its argv each read or not, the foreground caught moving, its
/// environment naming no tab, its own or another, the agent a second old or
/// past [`RECORD_WINDOW_S`] — and each verdict is taken as the model's
/// action (`Yes` is `Look`, `No` is `NotDue`, `Unread` is `Unread`) at the
/// state the combination projects to, a look owed: `due` an upgrade the step
/// could take (where nothing says, the worst case: one it could), `readable`
/// every read the verdict needed answered. The model must enable each.
/// NEGATIVE CONTROL: the classifier of 8538c65b5 — the record counted as read
/// before its process was — answers `No` where a process read failed:
/// `NotDue` where the model forbids it (`NeverDropped`'s cause).
#[test]
fn the_real_classifier_conforms_to_the_upgrade_look_model() {
    let model = aterm_spec::derive::harness_upgrade_look_model();
    let dir = scratch("look-bind");
    let mut opts = drive(&dir);
    opts.only_sid = Some(TAB.to_string());
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
    // NEGATIVE CONTROL: 8538c65b5's count — a record read once its group
    // led the tab, before its process was — made a behind record whose ids
    // did not read `No`.
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

/// N3 OF THE LIVE RE-TEST OF 2026-09-26: A SESSION IS BEHIND FROM ITS
/// WORKER'S ATTACH ([`note_behind`]). Its state is minted `Pending` there —
/// the target, the tab, its age from now — and the first step, at the first
/// idle point after a busy first turn, reuses it and keeps that age (the
/// re-test's column read `-` for 2m36s, and "behind" counted from the step).
/// NEGATIVE CONTROLS: a state already there is left as it is, and a session
/// on the newest build gets none.
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
    let stepped = St::for_target(load(&opts, SESSION), &running, &target, None, now_s());
    assert_eq!(stepped.pending_since, attached.pending_since);
    // NEGATIVE CONTROL: nothing newer to move to.
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

/// N3 AS THE RE-TEST OF 155c72a28 FOUND IT: A FRESH LAUNCH IS BEHIND FROM
/// ITS WORKER'S ATTACH, though the note can only be made once Claude Code has
/// written its record (1.2 s later there). The host asks again until it can
/// be read, with the second it first saw the session: the state is minted
/// with its age from THEN ([`mint_behind`]'s `since`), not from the ask that
/// finally read it. And a look that cannot read the session notes nothing
/// and says why ([`Behind::Unread`]), never "nothing to note". NEGATIVE
/// CONTROL: a `since` in the future never makes a session younger than now.
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
    let stepped = St::for_target(load(&opts, SESSION), &running, &target, None, now_s());
    assert_eq!(stepped.pending_since, attached);
    // NEGATIVE CONTROL.
    let _ = std::fs::remove_file(state_path(&opts, SESSION));
    assert!(mint_behind(&opts, &sf, TAB, &newer(), false, now_s() + 600));
    let st = load(&opts, SESSION).expect("minted");
    assert!(st.pending_since <= now_s(), "{st:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// N3, A SESSION UPGRADED BEFORE (the review of the N3 fix): the state an
/// EARLIER upgrade left — done, or failed on another build — no longer
/// says where the session stands once a newer build lands: it is minted
/// behind anew ([`behind_anew`]), its age from now, as the step would mint
/// it — not left to the first step, whose `for_target` started the age at
/// the step. NEGATIVE CONTROLS: a state still in flight, a carry-on whose
/// model is still waited for, a failure on this very build (final for it),
/// and a pending one (the step's, its age kept) are left as they are.
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

/// A shell whose dialect the relaunch line is written for, alive for the
/// test: `bash -c '…; true'` so bash stays the process rather than `exec`ing.
fn stand_in_shell() -> std::process::Child {
    Command::new("/bin/bash")
        .args(["-c", "sleep 30; true"])
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("bash")
}

/// RELAUNCH ON EXIT relaunches only an agent that CRASHED — exited leaving
/// Claude's own record behind (a crash cannot remove it) — on the
/// conversation that record names, and carries on a relaunch in flight
/// rather than typing a second one. Dry runs: the steps are decided against
/// real processes and files, and nothing is typed or written. NEGATIVE
/// CONTROLS: an agent that still runs; a launch no record names a
/// conversation of, and a one-shot run — each the launch's own end, never
/// relaunched and never refused; and a GRACEFUL exit (its record removed:
/// a person's or an orchestrator's `/exit`, a `kill`) — someone's decision
/// (the hazards review of 2026-09-25: it was relaunched from the snapshot).
#[test]
fn a_relaunch_on_exit_needs_an_exit_and_a_conversation_on_record() {
    let dir = scratch("after-exit");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
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
    };
    // Each step reads Claude's record at the attempt: an exit nothing could
    // read as it was seen ([`ExitRecord::Unread`]; the look at the exit is the
    // next test's).
    let at_attempt = |snap: &Snapshot, stalled: bool| {
        after_exit(&opts, snap, &ExitRecord::Unread, true, stalled)
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
    // term`, whose handler ran), and it is relaunched on its conversation
    // (U1).
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
    // be read at all is resumed (NEGATIVE CONTROL above: no projects dir).
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
    // ...unless it is ANOTHER process's (a relaunch that landed and whose
    // agent — the one that just left — exited before its continuation):
    // closed, and this exit relaunched afresh.
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
    dir: PathBuf,
    shell: std::process::Child,
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
        std::fs::write(
            &self.record,
            format!(
                r#"{{"pid":{},"sessionId":"{SESSION}","cwd":"/","version":"2.1.283","status":"busy","statusUpdatedAt":1,"procStart":"{}","kind":"interactive","entrypoint":"cli"}}"#,
                self.snap.pid, self.snap.start
            ),
        )
        .expect("the record");
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
        after_exit(&self.opts, &self.snap, left, true, false).step
    }
}

impl Drop for DeadAgent {
    fn drop(&mut self) {
        let _ = self.shell.kill();
        let _ = self.shell.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// D2 of the 2026-09-26 live test: WHAT THE EXIT LEFT is read as the exit is
/// seen and KEPT. A crashed agent's record that another Claude Code removes
/// during the back-off (measured on Claude Code 2.1.283: a SIGKILLed agent's
/// record gone 0.79 s after the kill, the moment a Claude in another tab
/// started) is still relaunched, on the conversation the look kept.
/// NEGATIVE CONTROLS: a graceful exit whose record goes a moment after the
/// exit is seen — inside the settle; `/exit` removes its own in its last
/// moments, measured 0.02 s after the `/exit` — is read as removed and
/// left, though the first look saw the record; and the record read at the
/// attempt, as the host of 57a2b7050 read
/// it, replays the defect: the crash left as a graceful exit.
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
    let r = after_exit(&a.opts, &a.snap, &left, true, false);
    assert_eq!(r.session, SESSION, "the conversation the look kept");
    // The defect, replayed: the record read at the attempt says graceful.
    assert_eq!(a.attempt(&ExitRecord::Unread), "ended:graceful-exit");
    // THE GRACEFUL EXIT: its record still there at the first look, removed
    // by the exit a moment later — inside the settle.
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
    let r = after_exit(&a.opts, &a.snap, &left, true, true);
    assert_eq!(r.step, "would-relaunch");
    // A record of another process's start is no record of this agent's.
    a.write_record();
    let reused = Snapshot {
        start: "Mon Jan  1 00:00:00 2024".to_string(),
        ..a.snap.clone()
    };
    assert_eq!(look_at_exit(&a.opts.home, &reused).record, None);
}

/// THE AGENT OF A TAB ATERM'S OWN CRASH TOOK (2026-09-27): aterm ended while
/// it ran, its shell's hangup reached it, and it removed its own record as
/// it went — which reads as a graceful exit, someone's decision. It was
/// nobody's: the next launch reopened its tab, and `after_host_ended`
/// relaunches it there on its conversation. A relaunch the crashed run had
/// in flight for its OLD tab (that tab is gone) is closed, not waited on.
/// NEGATIVE CONTROLS: the same removed record through `after_exit` (the
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
        after_exit(&a.opts, &a.snap, &ExitRecord::Removed, true, true).step,
        "wait:conversation-in-other-tab",
        "a relaunch on exit waits for the tab its record names"
    );
    assert_eq!(
        after_host_ended(&a.opts, &a.snap, true).step,
        "would-relaunch"
    );
    assert!(
        resumed_prompt("2.1.283", CAUSE_HOST).contains("aterm ended while this session ran"),
        "{}",
        resumed_prompt("2.1.283", CAUSE_HOST)
    );
}

/// Tier-1 of `HarnessExitRecord` (aterm-spec): EVERY path of the model, from
/// its initial state to one where nothing more is enabled, replayed through
/// the real look ([`look_at_exit`], [`exit_record`]) and the real attempt
/// ([`after_exit`], a dry run) over a real record of a dead process. After
/// each action the state the real code leaves — the record on disk, what
/// the look kept, what the attempt decided — is the model's, and every
/// invariant holds; every action of the model is driven. NEGATIVE
/// CONTROLS: the buggy host's witness `Crash`, `Look`, `Sweep`,
/// `DecideAtAttempt` replayed through the real read at the attempt
/// (`ExitRecord::Unread`, the host of 57a2b7050) leaves the crash exactly
/// as the buggy model does, and `CrashIsRelaunched` catches it; and a
/// graceful exit's own removal landing inside the real look's settle is
/// read as `OwnRemoval` then `Look`, a state the naive fix's
/// `LookAtInstant` never reaches (its exit relaunched, `GracefulIsLeft`).
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
    // NEGATIVE CONTROL: the host that read the record at the attempt.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let witness = ["Crash", "Look", "Sweep", "DecideAtAttempt"];
    let end = replay(&buggy, &witness);
    assert_eq!(end["decided"], 2, "the crash left as a graceful exit");
    assert!(!buggy.check_invariant("CrashIsRelaunched", &end), "{end:?}");
    // A graceful exit's own removal landing INSIDE the real look's settle
    // (`/exit` removes its own in its last moments): the real look reads it
    // as the model's `OwnRemoval` then `Look` — never as the naive fix's
    // `LookAtInstant`, which keeps the record the exit is still removing
    // and relaunches the exit.
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

/// THE RESTART IN PLACE (D3): the host restarts the tab's own agent — idle,
/// registered, its conversation on Claude's record — on the SAME program,
/// resuming its conversation, with nothing typed into it first. Dry runs:
/// decided against real processes and files, nothing typed or written.
/// NEGATIVE CONTROLS: a tab whose agent could not be read, another tab's
/// agent, no record of the process, a one-shot run — each a wait or a
/// refusal, never a signal; and a restart already in flight for the
/// conversation is carried on, never a second one started.
#[cfg(unix)]
#[test]
fn a_restart_in_place_is_planned_on_the_same_program_and_carried_on_once() {
    let dir = scratch("restart-here");
    let opts = Opts {
        dry_run: true,
        only_sid: Some(TAB.to_string()),
        ..drive(&dir)
    };
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
    // A bucket's fallback asks for its model on the line (D7); the way back
    // for the bucket's, or — the notice named none — for the model the
    // launch named before the fallback replaced it (its record kept it),
    // else for none: the launch's `--model` dropped.
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

/// A RELAUNCH HANDED BACK: where a supervisor loop takes the relaunched agent
/// (`hand_back`), the step ends as soon as the relaunch itself holds the
/// conversation — `adopted`, its pid, the record still in flight and OWED to
/// its tab — and [`resume`], at the loop's idle point, carries it on. A typed
/// continuation whose model is still to be read is owed too, and once read
/// is owed no more. NEGATIVE CONTROLS: without `hand_back` the same find
/// goes on to the carry-on; another tab owes nothing; a record whose model
/// was read owes nothing.
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
    // NEGATIVE CONTROL: no loop takes it — the same find goes on to the
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

/// A relaunch record keeps its cause across the state file, so whichever
/// caller carries it on types the relaunch's words, not the upgrade's.
#[test]
fn a_relaunchs_cause_survives_the_state_file() {
    let st = St {
        phase: Phase::Relaunched { at_s: 7 },
        cause: super::super::relaunch::CAUSE_EXIT.to_string(),
        ..St::default()
    };
    assert_eq!(St::from_json(&st.to_json()), Some(st));
    // NEGATIVE CONTROL: an upgrade's record (and one an older build wrote,
    // with no field at all) reads as the upgrade's.
    let old = r#"{"phase":"relaunched","at":7}"#;
    assert_eq!(St::from_json(old).map(|s| s.cause), Some(String::new()));
}

/// A fresh start carries every launch flag and no `--resume` — the one pair
/// the resume adds — and a resume carries the pair. NEGATIVE CONTROL: a
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
    )
    .unwrap_or_else(|_| panic!("a line"));
    assert_eq!(
        resumed,
        format!(" '/opt/claude' '--model' 'opus' '--resume' '{SESSION}'")
    );
    let fresh = line_for(Dialect::Bash, None, None, exe, &argv, None, None, None)
        .unwrap_or_else(|_| panic!("a line"));
    assert_eq!(fresh, " '/opt/claude' '--model' 'opus'");
    let print: Vec<String> = ["claude", "-p", "hi"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert!(line_for(Dialect::Bash, None, None, exe, &print, None, None, None).is_err());
}

// ---------------------------------------------------------------- the model

/// An assistant row naming `model`, as Claude Code 2.1.282 writes one
/// (measured 2026-09-24 in the owner's transcript), saying `text`.
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

/// A restart relaunched on 2.1.282 from 2.1.281 over a stand-in agent in
/// [`TAB`], at the moment its new process holds the conversation: the state as
/// [`relaunch`] leaves it, ON DISK, with what the restart recorded of the
/// transcript — through the sweep's own read, [`tail_to_end`] and
/// [`model_and_mark`] — when it was `before`. Dropped, it stops the agent and
/// removes its directory.
#[cfg(unix)]
struct Rig {
    dir: PathBuf,
    opts: Opts,
    asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    agent: std::process::Child,
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

/// A restart carried on END TO END ([`Rig`]): its transcript is `before` when
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

/// THE MODEL, BEFORE AND AFTER: the continuation names the model the agent's
/// READY answer ran, and the resumed session's first answer — past the
/// restart's mark, Claude's own `<synthetic>` row skipped (measured just after
/// a 2026-09-23 restart) — is the model after, read by the step after the one
/// that typed the continuation. A change is words only: the step is `done`.
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

/// RULING 293 of the messages design: a relaunch after aterm itself ended
/// (`CAUSE_HOST`, P6a) types the relaunch's own words once its agent holds
/// its conversation again. It typed the upgrade's (`Upgraded: this session
/// was restarted on Claude Code …`), which is false for an agent nobody
/// upgraded. NEGATIVE CONTROL: the upgrade's own record (no cause) still
/// types the upgrade's words, through the same carry-on.
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

/// DAY SIX, D30 of the messages design: a relaunched agent that ENDED AS IT
/// STARTED — Claude's `No conversation found`, a crash at start — leaves its
/// shell holding the tab again at once, and is said at once: `failed:no-resume`,
/// stopped, one ledger line, in seconds. It was waited on 90 s at a time for
/// five minutes, the harness's hand on the tab throughout, and the restored
/// tab's row came 14 minutes after the launch. (NEGATIVE CONTROLS, where no
/// 90 s wait is paid: `relaunch::tests::a_shell_that_has_the_tab_back_reads_as_ended_only_after_a_while`.)
#[cfg(unix)]
#[test]
fn a_relaunch_that_ended_as_it_started_is_said_at_once() {
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
    assert!(took < Duration::from_secs(20), "said in {took:?}");
    assert!(
        took >= ENDED_AT_ONCE,
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
    HELD_WAIT_IN_TEST.with(|w| w.set(Some(wait)));
    let r = await_new(&opts, r, &mut st, &mut c, SESSION, &ShellHolds(holds));
    HELD_WAIT_IN_TEST.with(|w| w.set(None));
    let rows = ledger_lines(&opts);
    let _ = std::fs::remove_dir_all(&dir);
    (r, st, rows)
}

/// A wait long enough that a shell read as holding the tab throughout would
/// have been read as ENDED ([`ENDED_AT_ONCE`] and two looks more).
#[cfg(unix)]
const PAST_ENDED: Duration = Duration::from_secs(ENDED_AT_ONCE.as_secs() + 2);

/// NEGATIVE CONTROLS of D30's early verdict, through the step itself (review
/// of day six, R2): a restored tab's relaunched agent that is still starting
/// — something other than its shell holds the tab — or whose shell cannot be
/// read is WAITED ON past [`ENDED_AT_ONCE`]: `wait:resume`, the record still
/// in flight, nothing stopped and nothing ledgered. Wired to a verdict that
/// ignores the kernel's answer, both would fail as `failed:no-resume`.
#[cfg(unix)]
#[test]
fn a_restored_relaunch_still_starting_or_unread_is_waited_on_not_failed() {
    for (holds, name) in [(Some(false), "held-starting"), (None, "held-unread")] {
        let (r, st, rows) = held_step(name, CAUSE_HOST, holds, PAST_ENDED);
        assert_eq!(r.step, "wait:resume", "{holds:?}: {r:?}");
        assert!(
            matches!(st.phase, Phase::Relaunched { .. }),
            "{holds:?}: {st:?}"
        );
        assert_eq!(rows, 0, "{holds:?}");
    }
}

/// RULING 296 AS AMENDED (review of day six, R1): the early verdict is a
/// restored tab's relaunch's alone. The LIVE relaunch on exit
/// ([`CAUSE_EXIT`]) keeps main's wait even with its shell holding the tab
/// throughout: its loop retries `failed:no-resume` after its back-off by
/// typing the relaunch line afresh, so an early verdict there retyped a
/// refusing agent within seconds. NEGATIVE CONTROL: the same step as a
/// restored tab's is said as ended.
#[cfg(unix)]
#[test]
fn a_live_relaunch_on_exit_keeps_its_wait_when_its_shell_has_the_tab() {
    let (r, st, rows) = held_step("held-live", CAUSE_EXIT, Some(true), PAST_ENDED);
    assert_eq!(r.step, "wait:resume", "{r:?}");
    assert!(matches!(st.phase, Phase::Relaunched { .. }), "{st:?}");
    assert_eq!(rows, 0);
    let (r, st, rows) = held_step("held-restored", CAUSE_HOST, Some(true), PAST_ENDED);
    assert_eq!(r.step, "failed:no-resume", "{r:?}");
    assert_eq!(st.phase, Phase::Failed("no-resume".to_string()));
    assert_eq!(rows, 1);
}

/// THE KERNEL'S OWN ANSWER ([`Kernel::shell_holds_tab`]'s default, which
/// [`Live`] uses) on real processes: a shell that leads its own session on a
/// terminal and is its foreground holds the tab; a job in a group of its
/// own that is not the terminal's foreground, or a process in another's
/// group, does not; no shell recorded (pid 0) or a gone one cannot be read.
#[cfg(unix)]
#[test]
fn the_kernel_reads_whether_a_shell_has_its_tab_back() {
    use std::os::unix::process::CommandExt as _;
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
    for c in [&mut zsh, &mut own, &mut plain] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(got, [Some(true), Some(false), Some(false)]);
    assert_eq!(Live.shell_holds_tab(0), None);
    assert_eq!(Live.shell_holds_tab(dead_pid()), None);
}

/// UNCONFIRMED: a resumed session that has not answered by the first sweep
/// past the bound is said to be, and it is the one outcome whose step says so.
/// The turns BEFORE the mark name a model: read from the start, they would have
/// "confirmed" it — the mark is what keeps the old process's model from
/// standing in for the new one's.
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

/// THE MODEL BEFORE is read from the tail that proved the READY answer, at
/// the restart itself: a sweep that reaches the restart records the model the
/// answer ran and where that read ended, even when its last look then waits
/// (the next restart takes both again). The READY answer is not the last row
/// here — Claude's own `<synthetic>` row follows it and names no model.
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
    let mut agent = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
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
    assert_eq!(saved.model_before, "claude-opus-5-5");
    assert_eq!(saved.mark, body.len() as u64, "the read ended at the end");
    assert_eq!(saved.launch_model, "claude-sonnet-5", "the kept --model");
    assert!(
        matches!(saved.phase, Phase::Announced { .. }),
        "never signalled"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// TYPED ONCE: the sweep that types the continuation can die before the
/// resumed session has answered — the window quits or hands itself over, a
/// hand-run `aterm harness upgrade` is interrupted — and what it leaves on
/// disk must not read as a restart still in flight, or the next sweep carries
/// on again and the agent is told twice. Read here the moment the continuation
/// has been typed, while the resumed session has not answered.
#[cfg(unix)]
#[test]
fn a_sweep_that_dies_after_the_continuation_never_types_it_again() {
    let before = [
        user_row("prepare"),
        turn_by("claude-opus-5-5", "Saved.\\nATERM-UPGRADE-READY-0badf00d"),
    ];
    let rig = Rig::new("continued-once", &before);
    rig.append(&[user_row("[aterm harness] Upgraded: carry on")]);
    // What the disk holds once the continuation was typed, while the resumed
    // session has not answered (its answer is written only after the join):
    // the first state read that is no longer in flight, or whatever the disk
    // still holds `settle` after the turn. The two answers sit a minute
    // apart. Saved as the turn returns, Done lands microseconds after it and
    // is taken then; saved only after the answer was waited for (the TYPED
    // TWICE defect), `relaunched` stays on disk for the whole `wait` — the
    // answer never comes in it — and is what the snapshot reads at `settle`.
    // It used to be read a fixed 300 ms after the turn, so a loaded gate
    // that held the typing thread off the CPU that long between the reply
    // and the save read like the defect (the load-sensitive test audit of
    // 2026-09-27).
    let (settle, wait) = (Duration::from_secs(30), Duration::from_secs(90));
    let snapshot = {
        let (asked, opts) = (std::sync::Arc::clone(&rig.asked), rig.opts.clone());
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            while turns(&asked) == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            let settled = Instant::now() + settle;
            loop {
                let on_disk = load(&opts, SESSION).expect("state");
                if !on_disk.in_flight() || Instant::now() >= settled {
                    return on_disk;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let mut st = rig.st.clone();
    let _ = rig.carry_on_from(&mut st, wait);
    let on_disk = snapshot.join().expect("snapshot");
    assert_eq!(rig.typed().len(), 1, "one continuation: {:?}", rig.typed());
    // A sweep resumed from that disk: a restart in flight is carried on.
    let mut resumed = on_disk.clone();
    if resumed.in_flight() {
        let _ = rig.carry_on_from(&mut resumed, Duration::from_millis(300));
    }
    assert_eq!(
        rig.typed().len(),
        1,
        "state on disk once the continuation was typed is {:?}; a sweep resumed from it typed \
         {} more continuation(s)",
        on_disk.phase,
        rig.typed().len() - 1
    );
    // What that sweep still owed — the model — the next one says.
    rig.append(&[turn_by("claude-opus-5-5", "Resumed.")]);
    let said = confirmations(&rig.opts);
    assert_eq!(
        said.iter().map(|r| r.step.as_str()).collect::<Vec<_>>(),
        ["done"]
    );
    assert_eq!(
        rig.details("done"),
        ["claude restarted on 2.1.282 · model claude-opus-5-5"]
    );
}

/// THE VISIT DOES NOT WAIT OUT THE ANSWER: a sweep runs its visits one after
/// another under one lock, and the orphan pass that relaunches a restart left
/// exiting runs after all of them, so every second one visit blocks ages that
/// restart toward [`STALE_S`] — past it, the agent is never relaunched and sits
/// dead at a shell prompt. The resumed session's first answer can take the
/// whole bound (a long first thought, retries, a usage limit that writes only
/// `<synthetic>` rows); the visit that types the continuation must not wait
/// for it.
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
    assert!(
        blocked < Duration::from_secs(20),
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

/// THE REASON, END TO END: a session launched with `--model claude-sonnet-5`
/// and moved to opus with `/model` comes back on sonnet — the kept flag did it,
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

/// ONE OUTCOME PER RESTART: a newer build landing while the last restart's
/// model is still owed waits for that `done` row — a new upgrade starts from a
/// fresh state, and would lose it.
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

/// THE SAME REFUSAL, WITH A MODEL DUE — the case the test above cannot see,
/// because it steps with [`ModelCtx::none`] and so never carries a model.
///
/// A WARM conversation (answered moments ago) on `claude-opus-5`, a newer build
/// installed, and `claude-opus-5-5` the newest Opus on offer: THE MODEL LADDER
/// sends the model along with the build restart (`build-restart`), so
/// `model_to` is non-empty on every pass. The agent is a launcher script's child, which the
/// upgrade must refuse — once. Before the fix the reuse guard compared the
/// Failed state's empty `model_list` with that non-empty `model_to`, re-minted
/// a fresh state each pass, and said the same refusal (and ledgered it) every
/// time (adversarial review, 2026-09-25). The negative control is the FIRST
/// pass: it must still refuse, so a pass is never one that simply stopped
/// looking.
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

/// THE DUE CLOCK SURVIVES A FLICKER, and only a definite "nothing due" clears
/// it. A visit that cannot read the live model (a transcript tail with no
/// answer in it — a long tool result does this for many visits running)
/// answers `model-unknown`; if that cleared the clock, the one-hour bound would
/// restart on every such visit and an active session's move might never land
/// (adversarial review, 2026-09-25). Conversely the early return — no model
/// can run, none pending — IS "nothing due" and must clear it, or a move that
/// returns later inherits a stale start and skips its warm-cache grace.
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

/// AN ASK THAT RAN STANDS (skeptic review of the `HarnessModelSwitch` model,
/// 2026-09-27). The harness asked for `claude-opus-5-5`, the relaunch ran it,
/// and a visit recorded it APPLIED. The conversation now runs another model (a
/// person moved off it), and a visit comes after `MODEL_SETTLE_S`. The failed
/// arm did not look at `applied`: the ask that ran was recorded FAILED too — a
/// false `model-failed` after its own `model-verified` — which bars the model
/// from this conversation for good. NEGATIVE CONTROL: an ask that never ran is
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

    // NEGATIVE CONTROL: the same ask, never run, settles as failed.
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

/// A session that runs the NATIVE build is judged by the native build's own
/// catalog and offer, not the managed one's (design record §2, OPEN since
/// 2026-09-24): the same conversation on `claude-opus-5` is due for Opus 5.5
/// (its own family's newest) under the managed view, and — the native build
/// not offering Opus 5.5 yet — for the list's Fable 5.1 under the native one.
/// With no native build installed, a native session falls back to the managed
/// view rather than to nothing.
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
    let due_to = |native: bool, ctx: &ModelCtx| match model_read_for(&opts, &sf, None, ctx, native)
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

/// THE PRE-FILTER NEVER RESTARTS A NATIVE SESSION'S WARM WAIT (skeptic review,
/// 2026-09-27). A native session on `claude-opus-5`, warm: the managed build
/// offers only `claude-opus-5` (the session runs it), the native build offers
/// `claude-opus-5-5` (due), and the move has been due for 50 minutes. Every
/// sweep runs the host's pre-filter before the visit. Were the pre-filter to
/// WRITE what it read under both views, the managed `model-current` would
/// clear the due clock and the native `Due` restart it, so the visit would
/// read ~0 s due on every sweep and `warm-wait-over` never fire. NEGATIVE
/// CONTROL: where the views agree the pre-filter's settle step is still kept —
/// the ask that runs is recorded applied.
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
    let first = model_read_for(&opts, &sf, None, &mctx, true);
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
        let read = model_read_for(&opts, &sf, None, &mctx, true);
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
    let read = model_read_for(&opts, &sf, None, &mctx, true);
    assert_eq!(
        models::model_moves_now(read.cold, false, read.due_for_s),
        Some("warm-wait-over"),
        "the warm wait must end for an active native session"
    );

    // NEGATIVE CONTROL: the views agree, and the pre-filter keeps its settle.
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

/// A CRASHED NATIVE SESSION'S RIDING MODEL IS THE NATIVE BUILD'S (skeptic
/// review, 2026-09-27). On the relaunch-on-exit path the session file's pid
/// has exited, so `exe_of` reads nothing; the view is then the program the
/// crash ran (`argv[0]`), which that relaunch runs again. Read off the dead pid
/// alone it would be the MANAGED view: here that offers only the model the
/// session already runs, so nothing would ride, while the native build the
/// relaunch runs offers `claude-opus-5-5`. NEGATIVE CONTROL: a managed program
/// on the same dead pid is judged by the managed view.
#[test]
fn a_crashed_native_sessions_riding_model_is_the_native_builds() {
    let dir = scratch("riding-native");
    let opts = drive(&dir);
    // The crash's own record, read while it lived; its pid since exited.
    let mut sf = register(&opts.home, std::process::id(), SESSION);
    let mut child = Command::new("true").spawn().expect("spawn");
    sf.pid = child.id();
    child.wait().expect("reap");
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
    dir: PathBuf,
    opts: Opts,
    /// The stand-in agent, until a test takes it to reap it itself
    /// ([`Parked::reaped`]).
    agent: Option<std::process::Child>,
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
        let agent = Command::new(std::env::current_exe().expect("exe"))
            .arg(PARK[0])
            .env(PARK_ENV, "1")
            .env("ATERM_PARENT_SESSION_ID", TAB)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("agent");
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

    /// The stand-in agent handed to a thread that reaps it the moment it
    /// ends and removes its session file, as Claude's own exit does — so a
    /// restart's wait for it to be gone ends on the EVENT — and says how it
    /// ended.
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
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    let (mut master, mut slave) = (0, 0);
    // SAFETY: `openpty` writes the two descriptors it opens into the two
    // integers; the name, termios and winsize are optional (NULL).
    let rc = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty");
    // SAFETY: both descriptors were just opened above and are owned here
    // alone; the master lives as long as the test process (leaked below) so
    // the terminal never hangs up under the shell.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let tty = |fd: &OwnedFd| std::process::Stdio::from(fd.try_clone().expect("dup"));
    let mut zsh = Command::new("/bin/zsh");
    zsh.args(["-f", "-c", "sleep 60; :"])
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
    for _ in 0..500 {
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

/// RANK 8, the presence guard — ONE presence fact (review of 2026-09-25): a
/// tab is ATTENDED when a PERSON gave THIS session input within ten minutes,
/// by the server's per-session stamp on the very read the sweep judges
/// (`"human_ms"`, `status human_ms=`), the one the supervisor's question
/// policy reads. Not the tab's placement and not the machine's HID clock: an
/// owner working in a browser all afternoon held the front tab's upgrade back
/// while that tab's own stamp read hours. The wait is said ONCE
/// (`held-back:attended`, a ledger row the host logs), then `wait:attended`.
/// A host that sends no stamp proves no absence: it FAILS CLOSED. The owner's
/// `--now` types into it all the same. This rig is a HAND-RUN sweep — no
/// window, no clock injected — and reads the same fact the window does
/// (review of 2026-09-25: a hand-run sweep never read a tab as attended, and
/// typed its notice into, then signalled, the tab a person was typing in).
/// NEGATIVE CONTROLS: the person gone eleven minutes, and nobody ever
/// (`null`), each announce once settled — the tab's placement (in front of
/// the focused window) notwithstanding.
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
    ask(&h.opts, TAB, Ask::Now).expect("--now");
    assert_eq!(h.visit().step, "announced:1");
    drop(h);
    // Negative controls: in front of the focused window, but nobody keyed it.
    for (case, human) in [("person-away", "660000"), ("never", "null")] {
        let h = rig(&format!("unattended-{case}"), Some(human));
        assert_eq!(h.visit().step, "announced:1", "{case}");
    }
}

/// RANK 8, the check-then-type: the screen read, the process proofs and the
/// paste were separate steps, and a keystroke between them was typed in front
/// of the notice. Where the host's `turn` takes the fence (`help turn` names
/// `if-gen=`), the notice is typed with `if-gen=<the fresh read's generation>
/// yield=0.2`; a host that answers `skipped reason=changed` typed nothing,
/// and the step is a wait with the upgrade still pending. NEGATIVE CONTROL:
/// a host without the fence gets the plain turn, never an `if-gen=` it would
/// type as text.
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
    assert_eq!(
        load(&h.opts, SESSION).map(|st| st.phase),
        Some(Phase::Pending)
    );
    let typed = h.typed();
    assert_eq!(typed.len(), 1);
    assert!(
        typed[0].contains(&format!(
            " turn if-gen=4.77 yield=0.2 {TURN_WAIT} [aterm harness]"
        )),
        "{typed:?}"
    );
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

/// THE OWNER'S HOLD OWNS NO TURN END: an upgrade `--skip`ped onto its
/// target, or `--defer`red past now, waits `skipped`/`deferred` and takes
/// nothing, and no idle point cures that — a skip is the build's last word
/// (the worker looks again only at the next activation notice or the owner's
/// next word), a deferral is looked at again once it may have run out, and
/// neither is a wait the worker owns the session's turn ends for, so its
/// supervisor continues the worker as ever. NEGATIVE CONTROLS: the same busy
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

/// RANK 23: the ledger grew by every step of every upgrade ever run. Past
/// its bound it is cut back to its newest rows at the next write, the row
/// just written among them (the bound is the disk journal's).
#[test]
fn the_upgrade_ledger_is_bounded() {
    let dir = scratch("ledger-bound-rank23");
    let opts = drive(&dir);
    std::fs::create_dir_all(state_dir(&opts)).expect("dir");
    let row = format!("{{\"step\":\"wait\",\"pad\":\"{}\"}}\n", "x".repeat(1000));
    let rows = usize::try_from(LEDGER_MAX_BYTES).expect("bound") / row.len() + 10;
    std::fs::write(state_dir(&opts).join("ledger.jsonl"), row.repeat(rows)).expect("ledger");
    let r = Report {
        pid: 1,
        tab: TAB.to_string(),
        session: SESSION.to_string(),
        from: "1".to_string(),
        to: "2".to_string(),
        step: "announced:1".to_string(),
    };
    ledger(&opts, &r, "marker");
    let text = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl")).expect("ledger");
    assert_eq!(text.lines().count(), LEDGER_KEEP_ROWS);
    assert!(
        text.lines()
            .last()
            .is_some_and(|l| l.contains("announced:1"))
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// RANK 19: `<exe> --version` ran with no bound while the sweep held its
/// lock. The WHOLE probe is bounded — the child's exit and the read of its
/// output: a candidate that never answers is killed with its group, and one
/// that answers and exits but leaves a grandchild holding the pipe (the
/// 2026-09-24 review measured 12.22 s against a 300 ms bound, the read
/// joined with no deadline) is cut off at the bound too, and its group
/// killed. Either is `TimedOut`. NEGATIVE CONTROLS: one that answers is read;
/// one that exits without a version is `NoVersion`.
#[cfg(unix)]
#[test]
fn a_version_probe_is_bounded_whole() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("version-probe");
    let script = |name: &str, body: &str| {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    };
    // A grandchild holding the pipe as well as the child: the group is
    // killed, or the read of the pipe would never end.
    let hang = script("hang", "sleep 987653 &\nexec sleep 987654");
    // The child answers and exits; its grandchild keeps the pipe open for
    // the review's twelve seconds — so a read joined with no deadline FAILS
    // this test (it reads the version, late) rather than hanging it.
    let lingers = script(
        "lingers",
        "sleep 12.98765 &\necho '2.1.282 (Claude Code)'\nexit 0",
    );
    let ours = |l: &str| l.contains("sleep 98765") || l.contains("sleep 12.98765");
    let answer = script("answer", "echo '2.1.282 (Claude Code)'");
    let silent = script("silent", "exit 3");
    // The FIRST exec of a script this test process just wrote can take
    // seconds on macOS before its first instruction runs (measured 2026-09-26
    // on the owner's M5 Max: 6.6 s for `answer`'s first probe, 5.8 ms for the
    // next exec — the system's check of a new file from a new binary), which
    // pushed `answer` past its 10 s bound under load. That is not the probe's
    // time: run the two scripts that end once, unbounded, so the bounded
    // probes below time the probe. (`hang` and `lingers` are expected to
    // time out anyway.)
    for warm in [&answer, &silent] {
        let _ = std::process::Command::new(warm).output();
    }
    for (name, exe) in [("hang", &hang), ("lingers", &lingers)] {
        let started = Instant::now();
        assert_eq!(
            version_within(exe, Duration::from_millis(300)),
            Probed::TimedOut,
            "{name}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{name}: bounded: {:?}",
            started.elapsed()
        );
    }
    // Every group was killed: gone once the kernel has reaped it (the pipe's
    // close, which ended the read, comes first in an exit).
    let gone = (0..100).any(|_| {
        let left = ps(&["-A", "-o", "stat=,command="]).unwrap_or_default();
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
        version_within(&answer, Duration::from_secs(10)),
        Probed::Version(Version::parse("2.1.282").expect("v"))
    );
    assert_eq!(
        version_within(&silent, Duration::from_secs(10)),
        Probed::NoVersion
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// RANK 19, the cache: a clean answer — a version, or a clean failure to name
/// one — is remembered while the file is unchanged, but a TIMEOUT is not a
/// verdict on the build (the 2026-09-24 review: one slow probe on a loaded
/// machine and that build was never probed again, so never upgraded to). It
/// is asked again on a back-off that doubles — unanswered until then, which
/// a look reads as nothing decided ([`UNREAD_VERSION`]) — and a changed file
/// is asked at once. Driven on an injected clock.
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
    // NEGATIVE CONTROL: a clean failure is remembered for the file as it is.
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

/// One `turn` reply's head: a `skipped` verdict typed nothing — the fence's
/// `reason=changed` is a wait — and an `OK` verdict typed.
#[test]
fn a_skipped_turn_typed_nothing() {
    assert_eq!(turn_verdict("OK 3 id=1 submitted=1 status=settled"), Ok(()));
    assert_eq!(
        turn_verdict("OK 0 turn skipped reason=changed submitted=0 seq=9 id=1"),
        Err(Typed::Changed)
    );
    assert!(matches!(
        turn_verdict("OK 0 turn skipped reason=guard submitted=0 seq=9 id=1"),
        Err(Typed::Refused(_))
    ));
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

/// RANK 8, the last look before the one irreversible act: a draft a person
/// began after the restart's own screen check — while the state was being
/// written — is read again on the same connection immediately before the
/// SIGTERM, and vetoes it (`wait:changed-before-signal`, the agent alive and
/// the upgrade still announced). NEGATIVE CONTROL: the same draft already on
/// the restart's first look stops it there (`wait:changed`).
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

/// THE CONTINUATION ON A SCREEN THAT NEVER HOLDS STILL (the 2026-09-24
/// review): fenced on a fresh read, a resumed session whose screen kept
/// redrawing moved under every fenced try, and after three the restart ended
/// `done:no-continue` — the resumed agent never told to go on, where every
/// continuation before the fence was typed. After its fenced tries the
/// continuation is typed UNFENCED, once more checked for an empty composer.
/// NEGATIVE CONTROL: a host that REFUSES the turn is not typed at again.
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

/// THE RELAUNCH LINE AND A PERSON TYPING THROUGH THE YIELD (the 2026-09-24
/// review): the fenced line parks on the typing momentum (`yield=0.2`), and a
/// yield that outlived the turn's timeout (`ERR yield timeout`) read as a
/// REFUSAL — `failed:relaunch` after the agent had already been ended. It is
/// a wait. And it is NEVER a reason to type the line UNFENCED (review of
/// 2026-09-25: after its fenced tries it was typed with no fence and no
/// yield, straight into whatever the person was typing): the attempt waits
/// `yield` at once, nothing more typed, and a later sweep reads the prompt
/// afresh. NEGATIVE CONTROL: a real refusal is still one, at once.
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
            &mut c,
            shell,
            TAB,
            SESSION,
            line,
            None,
            &mut None,
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
            &mut c,
            shell,
            TAB,
            SESSION,
            line,
            None,
            &mut None,
            |_, _, _| true
        ),
        Err(RelaunchLineError::Turn(_))
    ));
    assert_eq!(turns(&asked), 1);
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE HEAL RIDES THE RELAUNCH LINE (2026-09-26): with the shell's dialect
/// known, the window is asked for a typed re-key of the tab AFTER every check
/// that can make the attempt wait — `rekey shell=<the proven shell>` — and the
/// line typed is the healed one: the shell's own take of the key from the
/// path, then the relaunch line unchanged. Every way the heal can fail leaves
/// the relaunch as it was: a refusal (a healthy tab), an older window that
/// knows no such verb, and a healed line past the tty's bound (whose key is
/// taken back at once) all type the plain line; no dialect asks nothing. An
/// attempt that then types NOTHING — a person typing through the yield — takes
/// the key back (`rekey withdraw`) before it returns, so its one-use file never
/// waits for a line that is not coming.
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
            &mut None,
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
    // back before the attempt returns. NEGATIVE CONTROL: the same yield with no
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

/// THE HEAL IS THE ONE PRIMITIVE'S, FOR EVERY CALLER (the merge of
/// 2026-09-26): the dialect is read off the LIVE shell ([`shell_dialect`],
/// the read [`relaunch`] makes for the upgrade, the restart in place and a
/// relaunch on exit alike — none of them records it), and a bash prompt,
/// which is typed to and never pasted, is typed the HEALED line: bash's own
/// take of the key, then the relaunch line unchanged. NEGATIVE CONTROL: a
/// re-key the window refuses types the plain line at the same bash, and
/// takes nothing back.
#[cfg(unix)]
#[test]
fn a_relaunch_typed_at_bash_takes_the_key_its_live_shell_names() {
    const ISSUED: &str = "OK rekey ttl=120 path=/c/aterm/rekey/s-0a.1";
    let line = " '/pkg/claude' '--resume' 'x'";
    let mut bash = stand_in_shell();
    let mut dialect = None;
    for _ in 0..200 {
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
            &mut c,
            bash.id(),
            TAB,
            SESSION,
            line,
            dialect,
            &mut None,
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
    // And every caller types it: the upgrade's restart, the restart in place
    // and a relaunch on exit all end in the one `relaunch`, which hands the
    // line its live shell's dialect — never `None`, which would type no heal.
    let src = include_str!("relaunch.rs");
    let body_of = |head: &str| -> &str {
        let at = src.find(head).unwrap_or_else(|| panic!("{head}"));
        let body = &src[at..];
        &body[..body.find("\n}\n").unwrap_or(body.len())]
    };
    assert!(
        body_of("\npub(super) fn relaunch(\n")
            .contains("relaunch_held(opts, r, st, c, &mut hand, session, k)")
    );
    let relaunch = body_of("\nfn relaunch_held(\n");
    let read = relaunch
        .find("let dialect = shell_dialect(shell, &table());")
        .expect("the dialect read off the live shell");
    let typed = relaunch
        .find("type_relaunch_line(")
        .expect("the line typed");
    assert!(read < typed && relaunch[typed..].contains("\n        dialect,\n"));
    // A relaunch on exit — the agent's own, or its host's — is typed in
    // `after_exit_as`, the one body both of those wrap.
    for caller in ["\npub(super) fn restart_from(\n", "\nfn after_exit_as(\n"] {
        assert!(
            body_of(caller).contains("relaunch(opts, r, &mut st, &mut c, &session, &Live)"),
            "{caller}"
        );
    }
    assert!(
        include_str!("upgrade_drive.rs").contains("relaunch(opts, r, st, c, &sf.session_id, k)"),
        "the upgrade's restart"
    );
}

/// The requests of `asked` that PASTE into the tab.
#[cfg(unix)]
fn turns_in(asked: &[String]) -> usize {
    asked.iter().filter(|l| l.contains(" turn ")).count()
}

/// LIVE, run by name against an ISOLATED instance (never the owner's): the
/// shared relaunch typing path — the one both lanes call — heals a degraded
/// tab end to end. The window is asked for the key (`rekey shell=`), the line
/// typed is the healed one, and the shell's next prompt turns the tab's
/// `integration=` on. `ATERM_LIVE_REKEY_SOCK` names the instance's control
/// socket, `ATERM_LIVE_REKEY_TAB` the tab, `ATERM_LIVE_REKEY_SHELL` its shell
/// — a zsh spawned by a development build under `ATERM_DEBUG_LOST_SHELL_NONCE`
/// (spawn.rs), which is the owner's lost-nonce tab. The line relaunches
/// nothing: `echo` stands in for the agent.
#[cfg(unix)]
#[test]
#[ignore = "drives a live isolated aterm named by $ATERM_LIVE_REKEY_SOCK; run by name"]
fn live_the_relaunch_line_heals_a_degraded_tab() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name}"));
    let (sock, tab) = (var("ATERM_LIVE_REKEY_SOCK"), var("ATERM_LIVE_REKEY_TAB"));
    let shell: u32 = var("ATERM_LIVE_REKEY_SHELL").parse().expect("a pid");
    let dir = scratch("live-rekey");
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, &tab).expect("control connection");
    let integration = |c: &mut Client| {
        let status = c.request_line(&format!("@{tab} status")).expect("status");
        status
            .split_whitespace()
            .find_map(|w| w.strip_prefix("integration="))
            .map(str::to_owned)
    };
    assert_eq!(integration(&mut c).as_deref(), Some("degraded"));
    let got = type_relaunch_line_with(
        &opts,
        &mut c,
        shell,
        &tab,
        " echo RELAUNCHED",
        Some(Dialect::Zsh),
        &mut None,
        || true,
        |c, pid, tab| process_in_tab(c, pid, tab, None, None),
    );
    assert_eq!(got, Ok(()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while integration(&mut c).as_deref() != Some("on") {
        assert!(
            Instant::now() < deadline,
            "the next prompt never turned it on"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE RELAUNCH LINE IS NEVER TYPED ONTO A PERSON'S COMMAND (review of
/// 2026-09-25): the fence proves only that the screen held still between a
/// read and the paste, never that the prompt's input line is empty — a person
/// who typed `rm -rf build` at the returned prompt and paused got the line
/// appended to theirs (`rm -rf build '/…/claude' '--resume' …`) and Enter
/// pressed on both. The first read MARKS where the prompt left the cursor, and
/// the mark is kept across attempts: a read that finds typing after it WAITS
/// (`typing`), nothing typed, fenced or not — and so does the NEXT attempt
/// while the half-typed command is still there (re-marked, it would have been
/// typed onto). Deleted, the prompt reads as marked again and the line goes.
/// NEGATIVE CONTROLS: a prompt that holds its cursor is typed on the first
/// fenced try; one whose screen keeps changing with the cursor in place (a
/// ticking right prompt) is still typed unfenced after its fenced tries; one
/// redrawn by an async prompt (its cursor moved, its text changed) WAITS
/// (`prompt-moved`) — never marked afresh within the attempt, whose next read
/// could be a program a person started — and the next attempt marks it and
/// types.
#[cfg(unix)]
#[test]
fn the_relaunch_line_waits_for_a_person_typing_at_the_prompt() {
    let prompt = |text: &str, col: usize, seq: u64| {
        format!(
            r#"{{"rows":["{text}"],"cursor":{{"row":0,"col":{col}}},"seq":{seq},"first":0,"gen":"4.{seq}"}}"#
        )
    };
    let help = "OK 1\nturn [option=value ...] <text>: ... [if-gen=<epoch>.<seq>] ...";
    let settled = "OK 0 id=2 submitted=1 status=settled";
    let run = |name: &str, answers: Answers, mark: &mut Option<PromptMark>| {
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
            &mut c,
            std::process::id(),
            TAB,
            SESSION,
            " '/pkg/claude' '--resume' 'x'",
            None,
            mark,
            |_, _, _| true,
        );
        drop(c);
        let typed: Vec<String> = asked
            .lock()
            .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (got, typed)
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

/// A PERSON'S TYPING THAT WRAPS is typing too: at the bottom of the screen the
/// wrap scrolls it, so the cursor keeps its row — further LEFT — and the marked
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
        &mut c,
        std::process::id(),
        TAB,
        SESSION,
        " '/pkg/claude' '--resume' 'x'",
        None,
        mark,
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

/// AT A BASH PROMPT THE MARK STILL STANDS BETWEEN A PERSON'S TYPING AND THE
/// LINE: bash is typed to as keystrokes (`send`, which takes no `if-gen=`
/// fence — [`type_relaunch_line`]), so the prompt is read against its mark
/// once more right before them, and a person's typing after it (`ls`, typed
/// at the returned prompt) is a wait: nothing is sent. NEGATIVE CONTROL: the
/// same bash prompt as marked, nothing after it, is typed at.
#[cfg(unix)]
#[test]
fn a_bash_prompt_with_typing_after_its_mark_is_never_typed_at() {
    let mut bash = stand_in_shell();
    for _ in 0..200 {
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
            &mut c,
            bash.id(),
            TAB,
            SESSION,
            "claude --resume x",
            None,
            &mut mark,
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

/// THE RELAUNCH LINE IS NEVER TYPED INTO A PROGRAM A PERSON STARTED AT THE
/// RETURNED PROMPT (review of 2026-09-25): the shell was proven to hold the
/// tab only twice, both before the prompt was marked, while the tries after
/// the mark can take seconds (a fenced turn parks up to 30 s on a person's
/// typing). A person who ran `vim x` (or ↑⏎, `ssh`, a REPL, a second
/// `claude`) in that time had the line and its Enter typed into the program:
/// its screen was marked afresh and the fence taken from that same read.
/// Now a prompt that MOVES after the mark is a wait within the attempt, and
/// the shell is proven to hold the tab again immediately before every typed
/// try, the unfenced fallback included. NEGATIVE CONTROL: with the shell
/// holding the tab throughout, the same prompt is typed at.
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

/// A CARET MOVED HOME OVER A HALF-TYPED COMMAND IS TYPING (review of
/// 2026-09-25): the mark compared only the cursor and the text LEFT of it, so
/// a person who typed `rm -rf build` and pressed ctrl-a read as the prompt
/// unmoved — and a pure cursor move changes no content, so the fence let the
/// line in front of their command, with Enter under both. The mark keeps
/// where text to its right began (a right prompt): the cells from the cursor
/// up to it must stay blank. NEGATIVE CONTROLS: a right prompt that ticks in
/// place (a clock) is the prompt unmoved, and so is a bare prompt.
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

/// TYPEAHEAD BEFORE THE FIRST READ IS NEVER THE MARK (review of 2026-09-25):
/// the prompt was marked where the first read found the cursor, so a command
/// typed while the agent exited — echoed at the prompt before that read —
/// became the mark, and the line was appended to it (`rm -rf build
/// '/…/claude' '--resume' …`, Enter under both). Where the shell's
/// integration marked the input's start (`133;B`: `blocks --json`'s
/// `"cmdcol"`, on the row `line` reads), the cursor must sit exactly there;
/// where it did not, the cursor must follow the prompt's trailing blank; and
/// either way the cells at the cursor must be blank. NEGATIVE CONTROLS: a
/// clean prompt is marked and typed at, with the integration's mark and
/// without it.
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

// ---------------------------------------------------------------- the owner's view and word

/// THE OWNER'S WORD REACHES THE ACT, and the wait is KEPT for the owner to
/// read (gap audit 2026-09-24: 8h22m of `step=wait:not-idle` with nothing kept
/// of how long or why, and no way to say "move this one now"). A session whose
/// screen just moved waits `settling`; the state keeps that wait, since when,
/// the tab it is in and how long the session has been behind — and a second
/// sweep with the same wait keeps the SAME start. That is the negative
/// control: with no request, nothing is typed. `aterm harness upgrade <tab>
/// --now` ([`ask`]) waives exactly that window, and the next visit types the
/// notice; the act clears the kept wait.
#[cfg(unix)]
#[test]
fn an_owners_now_moves_a_settling_session_and_the_wait_is_kept_until_then() {
    let dir = scratch("owner-now");
    let (sock, asked) = instance(&dir);
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut agent = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), SESSION);
    let behind = now_s() - 7_200;
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
    let row = ask(&opts, TAB, Ask::Now).expect("the owner's word lands");
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
    assert_eq!(row.stall(now_s()), None, "two hours behind is not stalled");
    assert_eq!(
        moved.step, "announced:1",
        "the owner's word waived the window"
    );
    assert_eq!(typed_after, 1);
    assert!(after.wait.is_empty(), "an act clears the kept wait");
    assert_eq!(after.behind_since(), behind, "the age is the session's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE OWNER'S WORD IS ON THE TAB IT NAMED (review of 2026-09-25: it was kept
/// on the conversation, so a `--now` given for one tab waived the settling
/// window wherever the conversation was resumed). A `--now` recorded for
/// another tab is no word here: the settling session waits and nothing is
/// typed. NEGATIVE CONTROL: the same word recorded for THIS tab types the
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
    let mut agent = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("agent");
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

/// A READY ANSWER THE OWNER'S HOLD SUPERSEDED IS NO CONSENT (review of
/// 2026-09-25). An upgrade the owner holds (`--defer`, `--skip`) is yielded
/// nothing, and the window's supervisor waits on a point that answers its
/// notice for three minutes at most: then it types `keep going` and the
/// agent goes back to work AFTER its READY answer. That answer stayed in the
/// transcript's tail, and once the hold ended — the deferral ran out, or
/// `--now` over the skip — the sweep read it as consent and SIGTERMed an
/// agent in the middle of the work it was told to carry on with. A hold on
/// an ANNOUNCED upgrade now ends its notice: the phase is pending again and
/// the marker is gone, so the next turn end gets a fresh notice and a fresh
/// READY is needed. NEGATIVE CONTROL: the same READY state, settled, with no
/// hold is ended: the agent dies of the SIGTERM.
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
        ask(&h.opts, TAB, word).expect("the owner's word lands");
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
            ask(&h.opts, TAB, Ask::Now).expect("--now");
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
    // NEGATIVE CONTROL: no hold, the same READY at a settled point: ended.
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
    let status = ended
        .recv_timeout(Duration::from_secs(20))
        .expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert_eq!(h.details("terminated"), ["SIGTERM"], "{r:?}");
}

/// AN ATTENDED READY SESSION IS NEVER SIGNALLED ON THE UPGRADE'S OWN
/// JUDGMENT (review of 2026-09-25): a READY answer at a settled point in a
/// tab a person gave input in the last ten minutes (its `human_ms` stamp)
/// waits `attended` — said once, then waited on — however long past the
/// re-ask clock, and the agent is never sent the SIGTERM in front of the
/// person. NEGATIVE CONTROL: the owner's own `--now` ends it at once.
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
    ask(&h.opts, TAB, Ask::Now).expect("--now");
    let ended = h.reaped();
    let mut shell = tty_zsh();
    let r = h.visit_under(shell.id());
    let status = ended
        .recv_timeout(Duration::from_secs(20))
        .expect("the agent ended");
    let _ = shell.kill();
    let _ = shell.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{r:?}");
    assert_eq!(h.details("terminated"), ["SIGTERM"], "{r:?}");
}

/// A NEWER TARGET keeps how long the session has been behind and the owner's
/// word — except a skip, which is of one version — while nothing has begun; a
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
    let next = St::for_target(Some(prior.clone()), &from, &to, None, 900);
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
    let unskipped = St::for_target(Some(skipped), &from, &to, None, 900);
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
    let fresh = St::for_target(Some(done), &from, &to, None, 900);
    assert_eq!(fresh.behind_since(), 900, "behind again from now");
    assert_eq!(fresh.request, Request::None);
    let same_target = Candidate {
        version: Version::parse("2.1.282").expect("v"),
        ..to.clone()
    };
    let same = St::for_target(Some(prior.clone()), &from, &same_target, None, 900);
    assert_eq!(same, prior, "the same target is the same upgrade");
    // The same build with a model the announcement did not name is another
    // restart: announced afresh, keeping how long the session has waited and
    // the owner's word.
    let remodel = St::for_target(
        Some(prior.clone()),
        &from,
        &same_target,
        Some("claude-opus-5-5"),
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
            900
        ),
        pending,
        "nothing announced yet: the pending state is the same upgrade"
    );
}

/// THE LABEL FOLLOWS THE BUILD until the restart begins: managed and native
/// builds share a version, and a state minted for one kept naming it after
/// the sweep moved to the other (measured 2026-09-25: the ledger said
/// `2.1.282(native)` while the target was the managed twin). Once the restart
/// is in flight, or the upgrade failed, the label is history and stays.
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
        let next = St::for_target(Some(prior.clone()), &from, &managed, None, 900);
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
        let next = St::for_target(Some(prior.clone()), &from, &managed, None, 900);
        assert_eq!(next, prior, "{phase:?}: begun or final, kept as it was");
    }
}

/// THE STATE DIRECTORY IS BOUNDED: a settled upgrade of a conversation no live
/// Claude holds goes once it has sat a week, and nothing else does — a live
/// conversation, a fresh file, an announcement, a restart in flight, an
/// owner's word, another agent's state, a file that is no state, and every
/// file of a dry run all stay.
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
    st.note_step("wait:attended", "idle", 360);
    assert_eq!(st.wait_since, 300, "the same wait keeps its start");
    // The give-up is an act.
    st.note_step("gave-up", "idle", 420);
    assert!(st.wait.is_empty() && st.wait_since == 0);
}

/// THE LEDGER IS BOUNDED (it grew without one until 2026-09-24): past the
/// disk journal's 1 MiB it is cut back to its newest rows at the next write,
/// the newest row the one just written. NEGATIVE CONTROL: a small ledger is
/// only appended to.
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

/// THE MODEL ON THE RELAUNCH LINE: every spelling of a launch `--model` is
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
    )
    .unwrap_or_else(|_| panic!("a line"));
    assert!(
        line.contains("'--model' 'claude-opus-5-5'") && !line.contains("'claude-opus-5'"),
        "{line}"
    );
}

/// THE RELAUNCH ARGV, END TO END THROUGH THE RULE: a launch's argv, the model
/// the conversation runs and what the build offers, made into the line the
/// relaunch types by the same three calls the upgrade, the relaunch on exit
/// and the restart in place make — [`upgrade::launch_model`],
/// [`models::model_due`] (over the seeded list), and [`line_for`] (whose
/// `with_model` puts the model in place of the launch's own). The first row
/// is the 2026-09-25 incident: no `--model`, the transcript on Opus 5, the
/// build's newest Opus 5.5 — the line this rule types carries `--model
/// claude-opus-5-5`, where the line typed that day carried none and the
/// session came back on Opus 5.
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
        )
        .unwrap_or_else(|_| panic!("a line for {argv:?}"))
    };
    let line = |words: &[&str]| {
        std::iter::once("/x/claude")
            .chain(words.iter().copied())
            .chain(["--resume", ID])
            .map(|w| format!(" '{w}'"))
            .collect::<String>()
    };
    let rows: [(&[&str], &str, &[&str]); 10] = [
        // The incident: nothing named, the newest of the family asked for.
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
    // NEVER DOWN: the build offers only an older Opus — and a Fable the list
    // ranks BELOW Opus 5.5, which the list's step never takes.
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

/// No model half (a test's pass, an unreadable list): nothing is ever due,
/// so the driver behaves exactly as it did before the model layer.
#[test]
fn without_a_model_half_no_model_is_ever_due() {
    let ctx = ModelCtx::none();
    assert!(ctx.offered.is_empty());
}

/// A SAME-BUILD (model) restart the list no longer asks for is forgotten while
/// nothing has begun, so neither the owner's view nor the tab's supervisor
/// reads it PENDING for the rest of the conversation. NEGATIVE CONTROLS: a
/// build upgrade, an announced model restart and a dry run keep their state.
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
        ..model_only.clone()
    };
    let announced = St {
        phase: Phase::Announced { at_s: 9, asks: 1 },
        ..model_only.clone()
    };
    for (why, st) in [
        ("a build upgrade", &build),
        ("an announced one", &announced),
    ] {
        save(&opts, SESSION, st);
        forget_unwanted_model_restart(&opts, SESSION, Some(st), &running);
        assert_eq!(load(&opts, SESSION).as_ref(), Some(st), "{why} is kept");
    }
    save(&opts, SESSION, &model_only);
    let dry = Opts {
        dry_run: true,
        ..opts.clone()
    };
    forget_unwanted_model_restart(&dry, SESSION, Some(&model_only), &running);
    assert!(load(&opts, SESSION).is_some(), "a dry run writes nothing");
    forget_unwanted_model_restart(&opts, SESSION, Some(&model_only), &running);
    assert!(
        load(&opts, SESSION).is_none(),
        "the pending model restart is gone"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// THE WINDOW'S STEP answers with THE TAB'S OWN word ([`pick`]): a Codex
/// tab's pass also reports the daemon it runs on (`tab=-`), and the daemon's
/// act never stands for the client's wait — read as the tab's last word, a
/// `daemon-updated` finished the tab's upgrade (`after` → `Finished`) and
/// left its client unmoved until the next activation notice. A client that
/// is current leaves the daemon's word; with nothing of the tab's, the first
/// act, then the first wait, as before.
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
    // NEGATIVE CONTROL: nothing of the tab's — the first act, then the first
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

/// THE NOTICE AT A BREAK OF THE AGENT'S OWN BACKGROUND WORK, AT THE DRIVE
/// (the owner's answer of 2026-09-26): a Claude Code whose own status is
/// `busy` — its turn over, a workflow it waits on — is announced to when the
/// window's host takes the step at the break (`Opts::background`), on a
/// screen that has just moved (the loop's settle stands for it). Then,
/// within the re-ask interval, nothing more there: the step waits
/// `background`, typing nothing and ending nothing. (The re-ask a whole
/// `REASK_S` later is the reducer's, pinned in `upgrade_tests`.) NEGATIVE
/// CONTROL: the same busy session at an idle point's step waits `not-idle`,
/// as before.
#[cfg(unix)]
#[test]
fn a_break_of_background_work_takes_a_notice_and_ends_nothing() {
    let busy = |h: &mut Parked| {
        let path = h
            .opts
            .home
            .join(format!(".claude/sessions/{}.json", h.sf.pid));
        let text = std::fs::read_to_string(&path).expect("session file");
        std::fs::write(
            &path,
            text.replace(r#""status":"idle""#, r#""status":"busy""#),
        )
        .expect("rewrite");
        h.sf = session_file_of(&h.opts.home, h.sf.pid).expect("parses");
        assert_eq!(h.sf.status, "busy");
    };
    let mut h = Parked::new("bg-idle-point", Answers::default(), unsettled(), 3600);
    busy(&mut h);
    assert_eq!(h.visit().step, "wait:not-idle", "an idle point's step");
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

// ---------------------------------------------------------------- D1: no task

/// A user row the harness typed, as the E2E's transcripts hold one.
#[cfg(unix)]
fn harness_row(text: &str) -> String {
    user_row(&format!("[aterm harness] {text}"))
}

/// WHETHER A CONVERSATION HAS A TASK, READ ON (D1 of the live E2E of
/// 2026-09-26): nobody can say without readable projects; with them, no
/// transcript is no message and no task; the harness's own turns, answered,
/// are no task; a line caught half-written is read once whole; someone
/// else's prompt is a task the moment it is whole — before its answer (the
/// review of 2026-09-26) — and for good, and the next ask of the same
/// conversation reads only what was written since. Two transcripts claiming
/// the conversation: nobody can say. NEGATIVE CONTROL: another conversation
/// is read from its own start.
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
    // NEGATIVE CONTROL: another conversation starts over.
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
    let mut agent = Command::new(std::env::current_exe().expect("exe"))
        .arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("agent");
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
    let table = vec![(kernel.shell, 1, "zsh".to_string())];
    let files = session_files(&opts.home);
    let args = atpkg::caller_shell::process_args(agent.id()).expect("agent argv");
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

/// D1: AN UPGRADE NEVER ANNOUNCES TO A CONVERSATION WITH NO TASK — the E2E's
/// fresh sessions (0eada4a1, 16827ada, 1a5299ab) were each typed the notice,
/// which started a conversation nobody asked for. A fresh session, and one
/// holding only the harness's own turns, is restarted AFRESH instead
/// (`would-restart:fresh`). NEGATIVE CONTROLS: once a person's prompt was
/// answered it is announced to (`would-announce`), as before — and so is one
/// whose first prompt is recorded and not answered yet (the review of
/// 2026-09-26: it read taskless, and the fresh restart ended it in flight).
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

/// D1: WHAT THE TAB'S OWN SUPERVISOR TYPED IS NO TASK (the review of
/// 2026-09-26): a record holding the harness's notice and the supervisor's
/// own `keep going` and `answer_text` — unmarked, but in the tab's loop's
/// ledger under [`Opts::aterm_state`] — is restarted afresh. NEGATIVE
/// CONTROLS: the same record with no ledger known, and with a ledger of
/// other words, is announced to (its `keep going` read as someone's).
#[cfg(unix)]
#[test]
fn what_the_tabs_own_supervisor_typed_is_no_task() {
    let shell = dead_pid();
    let cfg = crate::supervise::SupervisorConfig::default();
    let rows = vec![
        harness_row("Claude Code 2.1.283 (managed) is installed"),
        turn_by("claude-haiku-4-5", "I cannot stop now."),
        user_row(&cfg.continue_text),
        turn_by("claude-haiku-4-5", "What would you like me to work on?"),
        user_row(&cfg.answer_text),
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

/// ND1 OF THE LIVE RE-TEST OF 2026-09-26: A RESTART HOLDS THE TAB FROM ITS
/// LAST LOOK ([`Hand`]) — the fresh restart fired 1.3 s after the first
/// idle verdict, the moment an orchestrator's `await agent idle` returns, and
/// the tab was a bare shell with `hand=-` until the relaunch line: a first
/// prompt sent then ran as a shell command line. The hand is taken before
/// the last look and every read of that look comes after it; a look that
/// then stops the restart (here the agent put in the background under it)
/// gives it back, nothing signalled. NEGATIVE CONTROL: another driver's live
/// lease on the tab is theirs — the restart waits `held`, reads no last
/// look, signals nothing and releases nothing.
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
    // NEGATIVE CONTROL: another driver holds the tab.
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

/// ND1, THE LAST LOOK AND THE SIGNAL (the review of the first cut: nothing
/// covered a person's key or a prompt arriving between the first look and
/// the last, and removing that re-read passed every test). Between the
/// restart's first look — under its hand — and the signal, the state is
/// written; a person who keys the tab then (`human_ms` inside the grace), or
/// a prompt sent then and not read yet (`input=pending`), is seen by the
/// LAST look, and nothing is signalled: the state goes back, the hand is
/// given back. And the signal itself is the last word, one server decision
/// (`signal term pid=<p> quiet=<grace>`): a keystroke after the last look
/// refuses it (`ERR busy person …`) — a wait, the state back, the hand
/// given back. NEGATIVE CONTROL: the same restart with nothing arriving
/// sends the signal, `quiet=` the grace.
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

/// The first request of `asked` that ends with `needle`.
#[cfg(unix)]
fn at_in(asked: &[String], needle: &str) -> Option<usize> {
    asked.iter().position(|l| l.ends_with(needle))
}

/// D1: A CONVERSATION WITH NO TASK IS CARRIED ON WITH NOTHING — a carry-on
/// ("Continue where you left off … decide for yourself") typed into one
/// starts a conversation nobody asked for (the E2E's B 1a5299ab). A
/// relaunched conversation whose record holds only the harness's own turns is
/// done (`done:taskless`, said once in the ledger) and nothing is typed.
/// NEGATIVE CONTROL: one a person asked something is carried on — as the
/// E2E's C was, whose finished task haiku then padded with invented work
/// (an open hazard, not this rule's).
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
    // NEGATIVE CONTROL: asked something, carried on.
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

/// D3(b) OF THE LIVE E2E OF 2026-09-26: THE SETTLE IS THE AGENT'S VERDICT'S,
/// NOT THE SCREEN'S. Quiet was counted from the look that first saw a new
/// screen `seq`, so Claude Code's dim `❯ continue` suggestion, drawn between
/// two looks, started it over and the notice waited five more minutes. Now it
/// is how long the server's own verdict has said the agent's turn is over
/// (`status agent=idle agent_since_ms=`): a screen that moved since the last
/// look, with the agent idle 30 s, is announced to at once. NEGATIVE
/// CONTROLS: the agent idle only 5 s, or busy, waits `settling`; and a
/// server that publishes no verdict, or one with no evidence (`unknown`), is
/// measured by the looks' samples, as before — a moved screen waits.
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

/// D5 OF THE LIVE E2E OF 2026-09-26: A TURN THE HARNESS TYPES RETURNS ONCE ITS
/// SUBMIT IS TAKEN, NOT WHEN THE AGENT'S ANSWER IS OVER. The step is taken in
/// the session's loop while the loop is still, and typed `idle=1500
/// timeout=30000`: the tab was held up to 30 s and the answer the carry-on
/// started ended inside the step, unseen by the loop — `keep going` followed
/// it half a second later. Here the stand-in agent answers for 3 s after
/// every submit: the carry-on and the notice each return at once, asked
/// `idle=1 timeout=10000` ([`TURN_WAIT`]). NEGATIVE CONTROL: a turn asked to
/// settle on quiet, as they were, waits the answer out.
#[cfg(unix)]
#[test]
fn a_typed_turn_returns_once_its_submit_is_taken_not_after_the_answer() {
    let answer = Duration::from_secs(3);
    let answers = || Answers {
        answer,
        ..Answers::default()
    };
    let asked = [
        user_row("Reply with one short sentence saying hello."),
        turn_by("claude-haiku-4-5", "Hello."),
    ];
    // The carry-on.
    let rig = Rig::with("turn-wait-carry", &asked, answers());
    let mut st = rig.st.clone();
    let t0 = Instant::now();
    let r = rig.carry_on_from(&mut st, MODEL_WAIT);
    let took = t0.elapsed();
    let typed = rig.typed();
    assert_eq!(typed.len(), 1, "{r:?}");
    assert!(typed[0].contains(&format!(" {TURN_WAIT} ")), "{typed:?}");
    assert!(
        took < answer,
        "the carry-on waited the answer out: {took:?}"
    );
    // NEGATIVE CONTROL: the old settle waits it out.
    let mut c = connect(&rig.opts, TAB).expect("control connection");
    let t0 = Instant::now();
    let (head, _) = c
        .request_counted(&format!("@{TAB} turn yield=0.2 idle=1500 timeout=30000 hi"))
        .expect("turn");
    assert!(head.starts_with("OK"), "{head}");
    assert!(
        t0.elapsed() >= answer,
        "the stand-in answers as the server settles"
    );
    drop(rig);
    // The notice.
    let dir = scratch("turn-wait-notice");
    let (sock, log) = instance_with(&dir, answers());
    let opts = Opts {
        sock: Some(sock),
        ..drive(&dir)
    };
    let mut c = connect(&opts, TAB).expect("control connection");
    let t0 = Instant::now();
    assert_eq!(
        turn_fenced(&mut c, TAB, "[aterm harness] notice", None),
        Ok(())
    );
    assert!(t0.elapsed() < answer, "the notice waited the answer out");
    assert!(
        log.lock()
            .expect("log")
            .iter()
            .any(|l| l.contains(" turn ") && l.contains(&format!(" {TURN_WAIT} "))),
        "{log:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// The stall of 2026-09-25/26: F1-F3 and the review's findings through the
/// real driver, and the Tier-1 bind of `harness_upgrade_never_strands_model`.
#[path = "upgrade_stall_tests.rs"]
mod stall;

/// A STOP THAT REPEATS RESTS LONGER, NEVER FOR GOOD (the no-stall review of
/// 2026-09-27, S1): the same reason in a row stretches the rest — RETRY_S, then
/// twice, then four times as long, and no longer — so a relaunch that never
/// comes up is not re-asked, re-signalled and left dead every RETRY_S. A
/// different reason starts the streak over, and the rest still ends: the
/// round re-arms. NEGATIVE CONTROL: the first stop rests exactly RETRY_S.
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

/// The login wall of 2026-09-27: no notice into it, no ask spent on a notice
/// it answered, a late READY honoured, and the Tier-1 bind of
/// `harness_login_wall_model` to the real code.
#[path = "upgrade_login_wall_tests.rs"]
mod login_wall;

// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A recorded login-wall session (Claude Code 2.1.281), replayed through the
//! real driver.

use super::*;
use crate::harness::upgrade::{
    Agent, GAVE_UP, Gate, HOLD_S, MAX_ASKS, REASK_S, announce_asks, clock_held, gate_announce,
    gate_release, gate_restart, next_step, ready_since, requested_step,
};

/// The recorded conversation, target and salt (from its state file:
/// `"salt":1790373062`, `"to":"2.1.283"`).
const INCIDENT_SESSION: &str = "03396a15-856e-4f1b-8174-ae9a3e4b369f";
const INCIDENT_SALT: u64 = 1_790_373_062;
const INCIDENT_TAB: &str = "s-b5cf2faabac5ce5127bd";

/// The ledger's four announcements (`t`, marker), UTC.
const LEDGER: [(u64, &str); 4] = [
    (1_790_485_402, "ATERM-UPGRADE-READY-cd35db51"),
    (1_790_487_206, "ATERM-UPGRADE-READY-86dddca2"),
    (1_790_489_010, "ATERM-UPGRADE-READY-39a18cbf"),
    (1_790_490_814, "ATERM-UPGRADE-READY-12cbabd0"),
];

/// The state file aterm 0.93.0 left, byte for byte.
const OWNERS_STATE: &str = r#"{"asks":0,"at":0,"confirm_by":0,"from":"2.1.281","last_seq":766968,"launch_model":"","line":"","mark":0,"marker":"ATERM-UPGRADE-READY-12cbabd0","model_before":"","noted":"","notice_pid":4205,"notice_start":"Thu Sep 24 04:27:57 2026","phase":"failed","pid":0,"resumed_on":"","resumed_pid":0,"salt":1790373062,"seq_since":1790519879,"shell":0,"source":"managed","tab":"s-b5cf2faabac5ce5127bd","to":"2.1.283","why":"unanswered"}"#;

/// Where the recorded rows ran (the home directory rewritten).
const CWD: &str = "/Users//owner/aterm/.claude/worktrees/harness-upgrade";

/// The keys every row of the recorded transcript carries after its own.
fn tail_keys(ts: &str, session: &str) -> String {
    format!(
        r#""timestamp":"{ts}","userType":"external","entrypoint":"cli","cwd":"{CWD}","sessionId":"{session}","version":"2.1.281","gitBranch":"docs/keeper-approved","slug":"shimmying-wibbling-salamander""#
    )
}

fn json_str(text: &str) -> String {
    aterm_json::to_string(&Value::from(text)).expect("json")
}

/// A turn typed into the composer (`origin` `human` for a person's, the
/// supervisor's and the upgrade's alike).
fn typed_row(ts: &str, session: &str, text: &str, source: &str) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000001","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000002","type":"user","message":{{"role":"user","content":{}}},"uuid":"00000000-0000-4000-8000-000000000003","permissionMode":"bypassPermissions","origin":{{"kind":"human"}},"promptSource":"{source}","turnOrigin":"human",{}}}"#,
        json_str(text),
        tail_keys(ts, session)
    )
}

/// A `/loop` wakeup's `continue` (`isMeta`, `turnOrigin` `scheduled`).
fn scheduled_row(ts: &str, session: &str) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000004","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000005","type":"user","message":{{"role":"user","content":"continue"}},"isMeta":true,"uuid":"00000000-0000-4000-8000-000000000006","permissionMode":"bypassPermissions","promptSource":"system","scheduledTaskId":"00000001","scheduledFireId":"00000000-0000-4000-8000-000000000004","turnOrigin":"scheduled","queuePriority":"later","queueSkipAttachments":true,{}}}"#,
        tail_keys(ts, session)
    )
}

/// A background workflow's completion (`origin` `task-notification`).
fn notification_row(ts: &str, session: &str) -> String {
    let text = "<task-notification>\n<task-id>w0000001</task-id>\n<status>completed</status>\n<summary>Dynamic workflow \"keeper phases\" completed</summary>\n</task-notification>";
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000007","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000008","type":"user","message":{{"role":"user","content":{}}},"uuid":"00000000-0000-4000-8000-000000000009","permissionMode":"bypassPermissions","origin":{{"kind":"task-notification"}},"promptSource":"system","turnOrigin":"task_notification","queueSkipAttachments":true,{}}}"#,
        json_str(text),
        tail_keys(ts, session)
    )
}

/// The login wall as Claude Code 2.1.281 writes it: a `<synthetic>` assistant
/// row, `isApiErrorMessage` and `error` `authentication_failed`, its one text
/// `Login expired · Please run /login` (every turn from 05:00:24 to 14:33:36
/// UTC ended on one, 45-86 ms after it began).
fn auth_wall_row(ts: &str, session: &str) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-00000000000a","isSidechain":false,"type":"assistant","uuid":"00000000-0000-4000-8000-00000000000b","message":{{"diagnostics":null,"id":"00000000-0000-4000-8000-00000000000c","container":null,"model":"<synthetic>","role":"assistant","stop_details":null,"stop_reason":"stop_sequence","stop_sequence":"","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"Login expired · Please run /login"}}],"context_management":null}},"error":"authentication_failed","isApiErrorMessage":true,"perTurnEffort":"xhigh","session_id":"{session}",{}}}"#,
        tail_keys(ts, session)
    )
}

fn duration_row(ts: &str, session: &str, ms: u64) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-00000000000d","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":{ms},"messageCount":3794,"uuid":"00000000-0000-4000-8000-00000000000e","isMeta":false,{}}}"#,
        tail_keys(ts, session)
    )
}

/// What a person's `/login` writes once the browser sign-in is done: the
/// caveat (`isMeta`), the command and its output.
fn login_rows(ts: &str, session: &str) -> [String; 3] {
    let caveat = "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>";
    let command = "<command-name>/login</command-name>\n            <command-message>login</command-message>\n            <command-args></command-args>";
    let row = |text: &str, meta: bool, n: u8| {
        let meta = if meta { r#""isMeta":true,"# } else { "" };
        format!(
            r#"{{"parentUuid":"00000000-0000-4000-8000-0000000000{n:02x}","isSidechain":false,"type":"user","message":{{"role":"user","content":{}}},{meta}"uuid":"00000000-0000-4000-8000-0000000001{n:02x}",{}}}"#,
            json_str(text),
            tail_keys(ts, session)
        )
    };
    [
        row(caveat, true, 1),
        row(command, false, 2),
        row(
            "<local-command-stdout>Login successful</local-command-stdout>",
            false,
            3,
        ),
    ]
}

/// A row of the session's own model saying `text` (the agent's prose is
/// the owner's and is paraphrased; the READY line is verbatim).
fn model_row(ts: &str, session: &str, text: &str, stop: &str) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-00000000000f","isSidechain":false,"message":{{"model":"claude-opus-5-5","id":"msg_0000000000000000000001","type":"message","role":"assistant","content":[{{"type":"text","text":{}}}],"container":null,"stop_reason":"{stop}","stop_sequence":null,"stop_details":null,"usage":{{"input_tokens":2,"output_tokens":40}},"input_transformations":[],"diagnostics":null,"context_management":null}},"apiBlockIndex":1,"requestId":"req_0000000000000000000001","type":"assistant","uuid":"00000000-0000-4000-8000-000000000010","advisorModel":"claude-opus-5-5","effort":"xhigh","perTurnEffort":"xhigh","session_id":"{session}",{}}}"#,
        json_str(text),
        tail_keys(ts, session)
    )
}

/// A tool's result, as Claude Code writes it between two model rows.
fn tool_result_row(ts: &str, session: &str) -> String {
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000011","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000012","type":"user","message":{{"role":"user","content":[{{"tool_use_id":"toolu_0000000000000000000001","type":"tool_result","content":"(the worktrees' state)","is_error":false}}]}},"uuid":"00000000-0000-4000-8000-000000000013","sourceToolAssistantUUID":"00000000-0000-4000-8000-00000000000f","session_id":"{session}",{}}}"#,
        tail_keys(ts, session)
    )
}

/// The upgrade's notice with `marker`, as 0.93.0 typed it (the same text
/// this build types).
fn notice_text(marker: &str) -> String {
    upgrade::prepare_prompt(
        &Version::parse("2.1.281").expect("from"),
        &Version::parse("2.1.283").expect("to"),
        Source::Managed,
        marker,
    )
}

/// The recorded rows, 05:00:24 to 14:34:21 UTC (shape only: every key, nesting
/// and type and the vendor's and harness's texts kept; ids, paths and the
/// agent's prose rewritten), for the conversation `session`. `upto` cuts it:
/// the rows up to and including the `upto`-th notice's wall (1-4), or
/// everything (`5`). `login`: the person's `/login` and what followed.
fn incident(session: &str, upto: usize, login: bool) -> String {
    let wall = |ts_in: &str, ts_out: &str, ms: u64| {
        [
            auth_wall_row(ts_in, session),
            duration_row(ts_out, session, ms),
        ]
    };
    let mut rows = vec![notification_row("2026-09-27T05:00:24.013Z", session)];
    rows.extend(wall(
        "2026-09-27T05:00:24.039Z",
        "2026-09-27T05:00:24.042Z",
        84_859_905,
    ));
    rows.push(typed_row(
        "2026-09-27T05:00:27.124Z",
        session,
        "continue",
        "suggestion_accepted",
    ));
    rows.extend(wall(
        "2026-09-27T05:00:27.168Z",
        "2026-09-27T05:00:27.177Z",
        70,
    ));
    rows.push(typed_row(
        "2026-09-27T05:00:58.260Z",
        session,
        "keep going",
        "typed",
    ));
    rows.extend(wall(
        "2026-09-27T05:00:58.300Z",
        "2026-09-27T05:00:58.304Z",
        64,
    ));
    rows.push(scheduled_row("2026-09-27T05:02:00.052Z", session));
    rows.extend(wall(
        "2026-09-27T05:02:00.083Z",
        "2026-09-27T05:02:00.086Z",
        45,
    ));
    let notices = [
        ("2026-09-27T05:03:21.004Z", "2026-09-27T05:03:21.041Z", 57),
        ("2026-09-27T05:33:25.028Z", "2026-09-27T05:33:25.086Z", 86),
        ("2026-09-27T06:03:29.083Z", "2026-09-27T06:03:29.141Z", 82),
        ("2026-09-27T06:33:33.097Z", "2026-09-27T06:33:33.157Z", 84),
    ];
    for (k, ((at, wall_at, ms), (_, marker))) in notices.iter().zip(LEDGER).enumerate() {
        if k >= upto {
            break;
        }
        if k == 1 {
            rows.push(scheduled_row("2026-09-27T05:23:00.483Z", session));
            rows.extend(wall(
                "2026-09-27T05:23:00.520Z",
                "2026-09-27T05:23:00.524Z",
                55,
            ));
        }
        rows.push(typed_row(at, session, &notice_text(marker), "typed"));
        rows.extend(wall(wall_at, wall_at, *ms));
    }
    if login {
        rows.extend(login_rows("2026-09-27T14:33:36.978Z", session));
        rows.push(typed_row(
            "2026-09-27T14:33:41.102Z",
            session,
            "continue",
            "typed",
        ));
        rows.push(model_row(
            "2026-09-27T14:33:59.169Z",
            session,
            "Before answering the upgrade notice, I am checking that nothing I started still runs.",
            "tool_use",
        ));
        rows.push(tool_result_row("2026-09-27T14:34:01.553Z", session));
        rows.push(model_row(
            "2026-09-27T14:34:21.484Z",
            session,
            "Nothing I started is still running, and all work in progress is committed.\n\nATERM-UPGRADE-READY-12cbabd0",
            "end_turn",
        ));
        rows.push(duration_row("2026-09-27T14:34:21.505Z", session, 40_425));
    }
    rows.join("\n") + "\n"
}

/// The screen the session showed at the wall, as `text --json` answers it:
/// [`aterm_phase::prompt::fixtures::LOGIN_EXPIRED`], the caret at column 2, no
/// person's input.
fn walled_screen_json() -> String {
    let rows = aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
    screen_json(&rows)
}

fn screen_json(rows: &[String]) -> String {
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("a caret row");
    let rows: Vec<Value> = rows.iter().map(|r| Value::from(r.as_str())).collect();
    format!(
        r#"{{"rows":{},"cursor":{{"row":{caret},"col":2}},"seq":77,"human_ms":null,"first":0}}"#,
        aterm_json::to_string(&Value::Array(rows)).expect("json")
    )
}

/// A stand-in agent in [`TAB`] whose argv a relaunch can carry: this test
/// binary, parked, with the filter alone (a positional; `--exact` is a flag
/// the plan refuses).
fn stand_in() -> Command {
    let mut c = Command::new(std::env::current_exe().expect("exe"));
    c.arg(PARK[0])
        .env(PARK_ENV, "1")
        .env("ATERM_PARENT_SESSION_ID", TAB)
        // An unset Claude home is its process's own HOME's: the sweep's.
        .env_remove("HOME")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    c
}

/// Targets holding the recorded target build.
fn to_2_1_283() -> Targets {
    Targets {
        managed: Some(Candidate {
            exe: PathBuf::from("/nonexistent/claude"),
            version: Version::parse("2.1.283").expect("version"),
            source: Source::Managed,
        }),
        native: None,
        unanswered: false,
    }
}

/// What one real visit did.
#[derive(Debug)]
struct Visited {
    step: String,
    /// The `turn`s typed into the tab, their text.
    typed: Vec<String>,
    /// The state the visit saved.
    saved: Option<St>,
    /// The ledger's rows it wrote.
    ledger: Vec<String>,
}

/// One real visit ([`visit_with_claim`]) of a stand-in agent in [`TAB`]
/// holding `session`: its session file registered, its transcript
/// `transcript`, its state what `st` makes of the stand-in's session file, its
/// tab showing `screen` (`text --json`), `targets` the builds on offer, and the
/// agent its shell's foreground job (then suspended: a restart reached waits
/// `changed` at its last look, nothing signalled).
#[cfg(unix)]
fn visit_at(
    label: &str,
    session: &str,
    st: impl FnOnce(&SessionFile, u64) -> St,
    transcript: &str,
    screen: String,
    targets: &Targets,
    fg: usize,
) -> Visited {
    let dir = scratch(&format!("login-wall-{label}"));
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
    let mut agent = stand_in().spawn().expect("agent");
    wait_exec(agent.id());
    let sf = register(&opts.home, agent.id(), session);
    let project = opts.home.join(".claude/projects/p");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(project.join(format!("{session}.jsonl")), transcript).expect("transcript");
    let st = st(&sf, now_s());
    std::fs::create_dir_all(state_dir(&opts)).expect("state");
    std::fs::write(state_path(&opts, session), st.to_json()).expect("write");
    let shell = dead_pid();
    let table = vec![(shell, 1, "zsh".into())];
    let args = atpkg::caller_shell::process_args(agent.id()).expect("argv");
    let files = session_files(&opts.home);
    let r = visit_with_claim(
        &opts,
        &sf,
        files.as_deref(),
        &table,
        targets,
        &Script::new(shell, fg, None),
        Some(&args),
        None,
    );
    let saved = load(&opts, session);
    let ledger = std::fs::read_to_string(state_dir(&opts).join("ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let typed = asked
        .lock()
        .map(|a| a.iter().filter(|l| l.contains(" turn ")).cloned().collect())
        .unwrap_or_default();
    assert!(alive(agent.id()), "{label}: nothing was signalled");
    let _ = agent.kill();
    let _ = agent.wait();
    let _ = std::fs::remove_dir_all(dir);
    Visited {
        step: r.step,
        typed,
        saved,
        ledger,
    }
}

/// The screen once the person's `/login` is done: its command and output
/// under the wall, the composer empty.
fn logged_in_rows() -> Vec<String> {
    let mut rows =
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
    let at = rows
        .iter()
        .position(|r| r.starts_with("⏺ Login expired"))
        .expect("the wall row");
    rows.splice(
        at + 1..at + 1,
        [
            String::new(),
            "❯ /login".to_string(),
            "  ⎿  Login successful".to_string(),
        ],
    );
    rows
}

fn logged_in_screen_json() -> String {
    screen_json(&logged_in_rows())
}

/// The recorded screen seq (the state's `last_seq`): a screen that has not
/// moved since the state was written.
const OWNERS_SEQ: &str = r#""seq":766968"#;

/// `screen` (`text --json`) at the owner's seq.
fn at_owners_seq(screen: String) -> String {
    screen.replace(r#""seq":77"#, OWNERS_SEQ)
}

/// The owner's state file with the stand-in's pid and start where 0.93.0's
/// notice named the owner's (4205, `Thu Sep 24 04:27:57 2026`), and the
/// stand-in's tab and running build (the stand-in runs 1.0.0, and a record
/// an older build wrote names its process by the build it was found behind):
/// every other byte as read.
fn owners_state_for(sf: &SessionFile) -> St {
    let text = OWNERS_STATE
        .replace(r#""from":"2.1.281""#, r#""from":"1.0.0""#)
        .replace(
            r#""notice_pid":4205"#,
            &format!(r#""notice_pid":{}"#, sf.pid),
        )
        .replace("Thu Sep 24 04:27:57 2026", &squash(&sf.proc_start))
        .replace(INCIDENT_TAB, TAB);
    St::from_json(&text).expect("the owner's state reads")
}

/// A state of the recorded upgrade: `phase`, the round's first `asks` markers
/// typed, the notice's fences on the stand-in.
fn incident_state(phase: Phase, asks: usize) -> impl FnOnce(&SessionFile, u64) -> St {
    move |sf, now| {
        let asked: Vec<String> = LEDGER[..asks].iter().map(|(_, m)| m.to_string()).collect();
        St {
            phase,
            from: "2.1.281".to_string(),
            to: "2.1.283".to_string(),
            source: "managed".to_string(),
            marker: asked.last().cloned().unwrap_or_default(),
            markers: asked.clone(),
            asked,
            tab: TAB.to_string(),
            notice_pid: if asks == 0 { 0 } else { sf.pid },
            notice_start: if asks == 0 {
                String::new()
            } else {
                squash(&sf.proc_start)
            },
            last_seq: 77,
            seq_since_s: now - 3600,
            salt: INCIDENT_SALT,
            ..St::default()
        }
    }
}

/// The recorded rows up to the person's `/login` and its output: the login
/// back, the agent not yet asked anything.
fn up_to_the_login(upto: usize) -> String {
    let all = incident(INCIDENT_SESSION, upto, true);
    let cut = all
        .find("\"2026-09-27T14:33:41.102Z\"")
        .and_then(|at| all[..at].rfind('\n'))
        .expect("the person's continue");
    all[..=cut].to_string()
}

/// `epoch` as a transcript's `timestamp`.
fn ts(epoch: u64) -> String {
    crate::harness::usage::rfc3339_utc(i64::try_from(epoch).expect("epoch"))
}

/// The unix second of one of the recorded stamps.
fn at(stamp: &str) -> u64 {
    crate::harness::upgrade_models::parse_utc(stamp).expect("a stamp")
}

/// The facts of an idle, settled Claude at an empty composer, nobody at it.
fn idle() -> Facts {
    Facts {
        status: "idle".to_string(),
        status_age_s: 3_600,
        composer_empty: true,
        quiet_s: 3_600,
        ..Facts::default()
    }
}

// ---------------------------------------------------------------- the readers

/// The login fact is the supervisor's reading and the transcript's: the
/// recorded screen reads as the wall by the supervisor's wall recogniser; the
/// transcript's last word on the login is Claude Code's
/// `authentication_failed` row from 05:00:24 until the person's
/// `Login successful` at 14:33:36, when the wall lifted; a turn the session's
/// model answers lifts it too. Negative controls: the screen once the login is
/// back; the transcript before the wall, after the lift and after a model's
/// answer; a synthetic row that is not a login's (`No response requested.`,
/// an overload); the wall's words in a subagent's row.
#[test]
fn the_login_wall_is_read_from_the_screen_and_the_transcript() {
    use aterm_phase::prompt::fixtures::{LOGIN_EXPIRED, screen};
    let walled = screen(LOGIN_EXPIRED);
    assert!(upgrade::login_wall(Agent::Claude, &walled));
    assert!(
        !upgrade::limited(&walled),
        "the login wall is no usage limit"
    );
    let back: Vec<String> = aterm_json::from_str::<Value>(&logged_in_screen_json())
        .expect("json")
        .get("rows")
        .and_then(Value::as_array)
        .expect("rows")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    assert!(!upgrade::login_wall(Agent::Claude, &back));
    let mut gutter = walled.clone();
    let row = gutter
        .iter()
        .position(|r| r.starts_with("⏺ Login expired"))
        .expect("the wall row");
    gutter[row] = "  ⎿  Not logged in · Please run /login".to_string();
    assert!(
        upgrade::login_wall(Agent::Claude, &gutter),
        "the gutter form"
    );

    // The transcript, cut at each recorded moment.
    for upto in 0..=4 {
        let walled = incident(INCIDENT_SESSION, upto, false);
        assert!(upgrade::transcript_login_wall(&walled), "{upto} notices in");
        assert_eq!(
            upgrade::login_lifted_at(&walled),
            None,
            "{upto}: not lifted"
        );
    }
    let lifted = up_to_the_login(4);
    assert!(
        !upgrade::transcript_login_wall(&lifted),
        "the login is back"
    );
    assert_eq!(
        upgrade::login_lifted_at(&lifted),
        Some(at("2026-09-27T14:33:36Z"))
    );
    let answered = incident(INCIDENT_SESSION, 4, true);
    assert!(!upgrade::transcript_login_wall(&answered));
    assert_eq!(
        upgrade::login_lifted_at(&answered),
        Some(at("2026-09-27T14:33:36Z"))
    );
    // A model's answer lifts it with no `/login` row (a login finished
    // elsewhere, then a turn typed here).
    let by_model = [
        notification_row("2026-09-27T05:00:24.013Z", INCIDENT_SESSION),
        auth_wall_row("2026-09-27T05:00:24.039Z", INCIDENT_SESSION),
        typed_row(
            "2026-09-27T09:00:00.000Z",
            INCIDENT_SESSION,
            "continue",
            "typed",
        ),
        model_row(
            "2026-09-27T09:00:05.000Z",
            INCIDENT_SESSION,
            "Back at it.",
            "end_turn",
        ),
    ]
    .join("\n");
    assert!(!upgrade::transcript_login_wall(&by_model));
    assert_eq!(
        upgrade::login_lifted_at(&by_model),
        Some(at("2026-09-27T09:00:05Z"))
    );

    // Negative controls.
    assert!(!upgrade::transcript_login_wall(""));
    let before = notification_row("2026-09-27T05:00:24.013Z", INCIDENT_SESSION);
    assert!(!upgrade::transcript_login_wall(&before));
    let others = [
        r#"{"type":"assistant","isApiErrorMessage":false,"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"No response requested."}]}}"#,
        r#"{"type":"assistant","isApiErrorMessage":true,"error":"overloaded","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"API Error: 529 Overloaded."}]}}"#,
    ];
    for row in others {
        assert!(!upgrade::transcript_login_wall(row), "{row}");
    }
    let subagent = auth_wall_row("2026-09-27T05:00:24.039Z", INCIDENT_SESSION)
        .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
    assert!(
        !upgrade::transcript_login_wall(&subagent),
        "a subagent's row"
    );
}

/// A second recorded session (Claude Code 2.1.283): its rows' shared keys,
/// home, branch and slug rewritten.
fn keys_2_1_283(ts: &str) -> String {
    format!(
        r#""timestamp":"{ts}","userType":"external","entrypoint":"cli","cwd":"/Users//owner/aterm","sessionId":"{OTHER_SESSION}","version":"2.1.283","gitBranch":"main","slug":"rewritten-slug""#
    )
}

/// That session's conversation.
const OTHER_SESSION: &str = "07e2381c-db77-4897-bfb1-0e1d5acd881d";

/// A local command's row as Claude Code 2.1.283 writes it: a `system` row,
/// `subtype` `local_command`, the text in a top-level `content`, not the
/// caveat-first `user` row 2.1.281 writes.
fn local_command_2_1_283(ts: &str, content: &str, run: bool) -> String {
    let run = if run {
        r#""commandRun":{"command":"login","args":""},"#
    } else {
        ""
    };
    format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000020","isSidechain":false,"type":"system","subtype":"local_command","content":{},"level":"info","uuid":"00000000-0000-4000-8000-000000000021","isMeta":false,{run}{}}}"#,
        json_str(content),
        keys_2_1_283(ts)
    )
}

/// The login as Claude Code 2.1.283 writes it lifts the wall: the wall
/// (`authentication_failed`, `Not logged in · Please run /login`), then the
/// person's `/login` and `Login successful` as two `system`/`local_command`
/// rows with a top-level `content`, then, nothing typed, the model answering
/// the prompt the wall had answered. The login is back at `Login successful`,
/// not at the model's answer (a session whose pending prompt is not asked
/// again has none). Negative control: another local command's output lifts
/// nothing.
#[test]
fn the_login_as_2_1_283_writes_it_lifts_the_wall() {
    let asked = format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000022","isSidechain":false,"promptId":"00000000-0000-4000-8000-000000000023","type":"user","message":{{"role":"user","content":"are you done now?"}},"uuid":"00000000-0000-4000-8000-000000000024","permissionMode":"bypassPermissions","origin":{{"kind":"human"}},"promptSource":"typed","turnOrigin":"human",{}}}"#,
        keys_2_1_283("2026-09-27T14:33:45.093Z")
    );
    let wall = format!(
        r#"{{"parentUuid":"00000000-0000-4000-8000-000000000024","isSidechain":false,"type":"assistant","uuid":"00000000-0000-4000-8000-000000000025","message":{{"diagnostics":null,"id":"00000000-0000-4000-8000-000000000026","container":null,"model":"<synthetic>","role":"assistant","stop_details":null,"stop_reason":"stop_sequence","stop_sequence":"","type":"message","usage":{{"input_tokens":0,"output_tokens":0}},"content":[{{"type":"text","text":"Not logged in · Please run /login"}}],"context_management":null}},"error":"authentication_failed","isApiErrorMessage":true,"perTurnEffort":"xhigh","session_id":"{OTHER_SESSION}",{}}}"#,
        keys_2_1_283("2026-09-27T14:33:45.213Z")
    );
    let walled = [asked, wall].join("\n") + "\n";
    assert!(upgrade::transcript_login_wall(&walled), "the control");
    let command = "<command-name>/login</command-name>\n            <command-message>login</command-message>\n            <command-args></command-args>";
    let logged_in = walled.clone()
        + &local_command_2_1_283("2026-09-27T14:33:54.544Z", command, false)
        + "\n"
        + &local_command_2_1_283(
            "2026-09-27T14:33:54.544Z",
            "<local-command-stdout>Login successful</local-command-stdout>",
            true,
        )
        + "\n";
    assert!(
        !upgrade::transcript_login_wall(&logged_in),
        "the login is back"
    );
    assert_eq!(
        upgrade::login_lifted_at(&logged_in),
        Some(at("2026-09-27T14:33:54Z"))
    );
    // Negative control: another command's output.
    let other = walled
        + &local_command_2_1_283(
            "2026-09-27T14:33:54.544Z",
            "<local-command-stdout>Status dialog dismissed</local-command-stdout>",
            false,
        )
        + "\n";
    assert!(upgrade::transcript_login_wall(&other));
    assert_eq!(upgrade::login_lifted_at(&other), None);
}

/// Which notice reached the model: over the recorded rows, each of the four
/// notices' own turn was the login wall's, before the login and after it (the
/// model's later READY answered the last notice as history). Negative
/// controls: a notice the model answered, one nothing has answered yet, and a
/// marker the tail holds no notice of (`None`: too short to judge).
#[test]
fn a_notice_answered_only_by_the_wall_never_reached_the_model() {
    for upto in 1..=4 {
        let tail = incident(INCIDENT_SESSION, upto, false);
        for (_, marker) in &LEDGER[..upto] {
            assert_eq!(upgrade::notice_fate(&tail, marker), Some(true), "{marker}");
        }
    }
    let whole = incident(INCIDENT_SESSION, 4, true);
    assert_eq!(upgrade::notice_fate(&whole, LEDGER[3].1), Some(true));
    assert!(upgrade::transcript_has_ready(&whole, LEDGER[3].1));

    let marker = LEDGER[0].1;
    let delivered = [
        typed_row(
            "2026-09-27T04:00:00.000Z",
            INCIDENT_SESSION,
            &notice_text(marker),
            "typed",
        ),
        model_row(
            "2026-09-27T04:00:09.000Z",
            INCIDENT_SESSION,
            "Winding down; two workflows still run.",
            "end_turn",
        ),
        // The wall later, on the wind-down's next turn: the notice was read.
        auth_wall_row("2026-09-27T04:30:00.000Z", INCIDENT_SESSION),
    ]
    .join("\n");
    assert_eq!(upgrade::notice_fate(&delivered, marker), Some(false));
    let unanswered = typed_row(
        "2026-09-27T04:00:00.000Z",
        INCIDENT_SESSION,
        &notice_text(marker),
        "typed",
    );
    assert_eq!(upgrade::notice_fate(&unanswered, marker), Some(false));
    assert_eq!(upgrade::notice_fate(&delivered, LEDGER[1].1), None);
    // The notice typed again and answered: its latest copy decides.
    let again = [
        incident(INCIDENT_SESSION, 1, true),
        typed_row(
            "2026-09-27T14:40:00.000Z",
            INCIDENT_SESSION,
            &notice_text(marker),
            "typed",
        ),
        model_row(
            "2026-09-27T14:40:09.000Z",
            INCIDENT_SESSION,
            "Winding down.",
            "end_turn",
        ),
    ]
    .join("\n");
    assert_eq!(upgrade::notice_fate(&again, marker), Some(false));
}

// ---------------------------------------------------------------- the reducer

/// A session at the login wall is never asked, and nothing of the upgrade's
/// is spent there: every gate waits `login` (the owner's `--now` and a break
/// of background work included), with no re-ask, give-up, void or restart, and
/// an announced upgrade's clock (and the READY's) restarts at every look that
/// finds the wall. Negative controls: the same facts off the wall take each of
/// those steps.
#[test]
fn a_session_at_the_login_wall_is_never_asked_and_no_clock_runs() {
    let wall = Facts {
        login: true,
        ..idle()
    };
    assert_eq!(gate_announce(&wall), Gate::Wait("login"));
    assert_eq!(gate_restart(&wall, true), Gate::Wait("login"));
    assert_eq!(gate_release(&wall, false), Gate::Wait("login"));
    let now = Facts {
        owner_now: true,
        ..wall.clone()
    };
    assert_eq!(
        gate_announce(&now),
        Gate::Wait("login"),
        "--now waives no wall"
    );
    assert_eq!(
        requested_step(&Request::Now, &Phase::Pending, &wall, false, 1, "2.1.283"),
        Step::Wait("login")
    );
    let at_break = Facts {
        status: "busy".to_string(),
        background_point: true,
        ..wall.clone()
    };
    assert_eq!(gate_announce(&at_break), Gate::Wait("login"));
    let t0 = LEDGER[0].0;
    let asked = Phase::Announced { at_s: t0, asks: 1 };
    let tired = Phase::Announced {
        at_s: t0,
        asks: MAX_ASKS,
    };
    let gave_up = Phase::Failed(GAVE_UP.to_string());
    let mut boxed = wall.clone();
    boxed.approval_box = true;
    boxed.hold_s = HOLD_S;
    let later = t0 + 4 * REASK_S;
    for (phase, ready, facts) in [
        (&Phase::Pending, false, &wall),
        (&asked, false, &wall),
        (&tired, false, &wall),
        (&asked, true, &wall),
        (&asked, true, &boxed),
        (&gave_up, true, &wall),
    ] {
        assert_eq!(
            next_step(phase, facts, ready, later),
            Step::Wait("login"),
            "{phase:?} ready={ready}"
        );
    }
    assert_eq!(
        clock_held(&asked, &wall, later),
        Phase::Announced {
            at_s: later,
            asks: 1
        }
    );
    assert_eq!(ready_since(50, true, &wall, 90), 90);
    // Negative controls: off the wall the same looks ask, re-ask, give up, end
    // and void, and the clocks run.
    let free = idle();
    assert_eq!(
        next_step(&Phase::Pending, &free, false, later),
        Step::Announce
    );
    assert_eq!(next_step(&asked, &free, false, later), Step::Announce);
    assert_eq!(next_step(&tired, &free, false, later), Step::GiveUp);
    assert_eq!(next_step(&asked, &free, true, later), Step::Terminate);
    assert_eq!(next_step(&gave_up, &free, true, later), Step::Terminate);
    let mut person = free.clone();
    person.approval_box = true;
    person.hold_s = HOLD_S;
    assert_eq!(next_step(&asked, &person, true, later), Step::Void("box"));
    assert_eq!(clock_held(&asked, &free, later), asked);
    assert_eq!(ready_since(50, true, &free, 90), 50);
}

// ---------------------------------------------------------------- the real visit

/// The recorded session through the real visit (the driver's `visit`, a parked
/// stand-in agent, a stand-in instance serving the screens, the recorded rows
/// as transcript): at the wall nothing is typed. A pending upgrade and an
/// announced one whose window ran out both wait `login`, as does one whose
/// screen no longer shows the wall while the transcript's last word on the
/// login is still the wall. Once the login is back, the upgrade that typed four
/// notices into the wall types its last one again (ask 4, marker `12cbabd0`,
/// the ledger saying why) instead of giving up. Negative control: had the
/// model received that fourth notice, the same visit gives up.
#[cfg(unix)]
#[test]
fn the_incident_is_waited_out_and_its_last_notice_asked_again() {
    let window_out = |asks: u32| Phase::Announced {
        at_s: now_s() - REASK_S - 1,
        asks,
    };
    let pending = visit_at(
        "pending",
        INCIDENT_SESSION,
        incident_state(Phase::Pending, 0),
        &incident(INCIDENT_SESSION, 0, false),
        walled_screen_json(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (pending.step.as_str(), pending.typed.len()),
        ("wait:login", 0)
    );
    let asked = visit_at(
        "asked",
        INCIDENT_SESSION,
        incident_state(window_out(1), 1),
        &incident(INCIDENT_SESSION, 1, false),
        walled_screen_json(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!((asked.step.as_str(), asked.typed.len()), ("wait:login", 0));
    // The dialog dismissed: the screen shows no wall, the transcript does.
    let dismissed = {
        let mut rows =
            aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::LOGIN_EXPIRED);
        let at = rows
            .iter()
            .position(|r| r.starts_with("⏺ Login expired"))
            .expect("the wall row");
        rows.splice(
            at + 1..at + 1,
            [
                String::new(),
                "❯ /login".to_string(),
                "  ⎿  Login interrupted".to_string(),
            ],
        );
        assert!(aterm_phase::wall(&rows).is_none());
        screen_json(&rows)
    };
    let off_screen = visit_at(
        "off-screen",
        INCIDENT_SESSION,
        incident_state(Phase::Pending, 0),
        &incident(INCIDENT_SESSION, 0, false),
        dismissed,
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(
        (off_screen.step.as_str(), off_screen.typed.len()),
        ("wait:login", 0)
    );

    // The login back, four notices spent into the wall.
    let again = visit_at(
        "again",
        INCIDENT_SESSION,
        incident_state(window_out(MAX_ASKS), 4),
        &up_to_the_login(4),
        logged_in_screen_json(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(again.step, "announced:4", "{again:?}");
    assert_eq!(again.typed.len(), 1);
    assert!(again.typed[0].contains(LEDGER[3].1), "{:?}", again.typed);
    let saved = again.saved.expect("state");
    assert!(matches!(saved.phase, Phase::Announced { asks: 4, .. }));
    assert_eq!(saved.marker, LEDGER[3].1);
    assert!(
        again
            .ledger
            .iter()
            .any(|l| l.contains(r#""step":"announced:4""#)),
        "{:?}",
        again.ledger
    );
    assert!(
        !again.ledger.iter().any(|l| l.contains("gave-up")),
        "{:?}",
        again.ledger
    );
    // Negative control: the fourth notice answered by the model.
    let read = up_to_the_login(4).replace(
        &auth_wall_row("2026-09-27T06:33:33.157Z", INCIDENT_SESSION),
        &model_row(
            "2026-09-27T06:33:40.000Z",
            INCIDENT_SESSION,
            "Not yet: two workflows still run.",
            "end_turn",
        ),
    );
    assert_eq!(upgrade::notice_fate(&read, LEDGER[3].1), Some(false));
    let gave_up = visit_at(
        "read",
        INCIDENT_SESSION,
        incident_state(window_out(MAX_ASKS), 4),
        &read,
        logged_in_screen_json(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert!(
        gave_up.step == "gave-up" || gave_up.step == "released:gave-up",
        "{gave_up:?}"
    );
}

/// A READY to the current marker authorizes the restart under every gate, read by the real reader
/// from the real transcript: the notice typed again once the login is back, the agent's
/// `ATERM-UPGRADE-READY-12cbabd0` on a line by itself, and the restart is taken (the stand-in's
/// job read stops it just before the signal: `wait:changed`). A person's draft holds it
/// (`draft`), and so does the wall returning after the answer (`login`). Negative control: the
/// READY quoted in a sentence is no answer.
#[cfg(unix)]
#[test]
fn a_ready_to_the_current_marker_restarts_under_every_gate() {
    let marker = LEDGER[3].1;
    let asked_again = || {
        let mut t = up_to_the_login(4);
        t.push_str(&typed_row(
            "2026-09-27T14:40:00.000Z",
            INCIDENT_SESSION,
            &notice_text(marker),
            "typed",
        ));
        t.push('\n');
        t
    };
    let answered = |text: &str| {
        let mut t = asked_again();
        t.push_str(&model_row(
            "2026-09-27T14:41:00.000Z",
            INCIDENT_SESSION,
            text,
            "end_turn",
        ));
        t.push('\n');
        t
    };
    let ready = answered(&format!("Everything is committed.\n\n{marker}"));
    assert!(upgrade::transcript_has_ready(&ready, marker));
    let announced = || {
        incident_state(
            Phase::Announced {
                at_s: now_s() - 60,
                asks: MAX_ASKS,
            },
            4,
        )
    };
    let restart = visit_at(
        "ready",
        INCIDENT_SESSION,
        announced(),
        &ready,
        logged_in_screen_json(),
        &to_2_1_283(),
        3,
    );
    // The restart's own last look (the stand-in's job read) stops it before
    // the signal; the model it ran and the mark are recorded only there.
    assert_eq!(restart.step, "wait:changed", "{restart:?}");
    assert!(restart.typed.is_empty());
    let saved = restart.saved.expect("state");
    assert_eq!(saved.model_before, "claude-opus-5-5");
    assert_eq!(saved.mark, ready.len() as u64);
    // A person's draft holds it.
    let drafted = {
        let mut rows = logged_in_rows();
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('❯'))
            .expect("caret");
        rows[caret] = "❯ wait, one more thing".to_string();
        screen_json(&rows)
    };
    let held = visit_at(
        "draft",
        INCIDENT_SESSION,
        announced(),
        &ready,
        drafted,
        &to_2_1_283(),
        3,
    );
    assert_eq!(held.step, "wait:draft", "{held:?}");
    // The wall back after the answer: no restart into it.
    let mut walled = ready.clone();
    walled.push_str(&typed_row(
        "2026-09-27T14:42:00.000Z",
        INCIDENT_SESSION,
        "continue",
        "typed",
    ));
    walled.push('\n');
    walled.push_str(&auth_wall_row("2026-09-27T14:42:00.050Z", INCIDENT_SESSION));
    walled.push('\n');
    let at_wall = visit_at(
        "wall",
        INCIDENT_SESSION,
        announced(),
        &walled,
        logged_in_screen_json(),
        &to_2_1_283(),
        3,
    );
    assert_eq!(at_wall.step, "wait:login", "{at_wall:?}");
    // Negative control: quoted, not answered.
    let quoted = answered(&format!(
        "I will reply with {marker} once the build is done."
    ));
    assert!(!upgrade::transcript_has_ready(&quoted, marker));
    let waits = visit_at(
        "quoted",
        INCIDENT_SESSION,
        announced(),
        &quoted,
        logged_in_screen_json(),
        &to_2_1_283(),
        3,
    );
    assert_eq!(waits.step, "wait:awaiting-ready", "{waits:?}");
}

/// A recorded state file of a given-up upgrade is read and recovered: it reads
/// as `failed:unanswered` with its last marker, tab and fences, and writes
/// back the same. A READY to its marker after the give-up is honoured. Where
/// the owner spoke after that READY the give-up is still taken back (none of
/// its notices reached the model), found further back than the ordinary tail
/// when the conversation has gone on. A newer target asks afresh. Negative
/// control: a give-up whose last notice the model received stays given up.
#[cfg(unix)]
#[test]
fn the_state_0_93_0_left_is_read_and_recovered() {
    let owners = St::from_json(OWNERS_STATE).expect("the owner's state reads");
    assert_eq!(owners.phase, Phase::Failed(GAVE_UP.to_string()));
    assert_eq!(owners.marker, LEDGER[3].1);
    assert_eq!(owners.tab, INCIDENT_TAB);
    assert_eq!(
        (owners.notice_pid, owners.notice_start.as_str()),
        (4205, "Thu Sep 24 04:27:57 2026")
    );
    assert_eq!(
        (owners.from.as_str(), owners.to.as_str()),
        ("2.1.281", "2.1.283")
    );
    assert_eq!(owners.salt, INCIDENT_SALT);
    assert_eq!(St::from_json(&owners.to_json()), Some(owners.clone()));

    let owners_screen = || at_owners_seq(logged_in_screen_json());
    let late = visit_at(
        "late-ready",
        INCIDENT_SESSION,
        |sf, _| owners_state_for(sf),
        &incident(INCIDENT_SESSION, 4, true),
        owners_screen(),
        &to_2_1_283(),
        3,
    );
    assert_eq!(late.step, "wait:changed-before-signal", "{late:?}");
    assert!(late.typed.is_empty());

    let mut spoken = incident(INCIDENT_SESSION, 4, true);
    spoken.push_str(&typed_row(
        "2026-09-27T14:34:47.664Z",
        INCIDENT_SESSION,
        "fix the bug in the harness",
        "typed",
    ));
    spoken.push('\n');
    spoken.push_str(&model_row(
        "2026-09-27T14:35:14.097Z",
        INCIDENT_SESSION,
        "Checking the facts first.",
        "end_turn",
    ));
    spoken.push('\n');
    assert!(!upgrade::transcript_has_ready(&spoken, LEDGER[3].1));
    let rearmed = visit_at(
        "rearmed",
        INCIDENT_SESSION,
        |sf, _| owners_state_for(sf),
        &spoken,
        owners_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(rearmed.step, "announced:1", "{rearmed:?}");
    assert!(
        rearmed.typed[0].contains(LEDGER[0].1),
        "{:?}",
        rearmed.typed
    );
    // The conversation gone on 400 KB past the notice: found further back.
    let mut far = spoken.clone();
    let filler = model_row(
        "2026-09-27T15:00:00.000Z",
        INCIDENT_SESSION,
        &"The keeper review goes on. ".repeat(40),
        "end_turn",
    );
    while far.len() < spoken.len() + 400 * 1024 {
        far.push_str(&filler);
        far.push('\n');
    }
    let tail = &far[far.len() - usize::try_from(TAIL_BYTES).expect("bytes")..];
    assert_eq!(
        upgrade::notice_fate(tail, LEDGER[3].1),
        None,
        "out of the tail"
    );
    let deep = visit_at(
        "rearmed-far",
        INCIDENT_SESSION,
        |sf, _| owners_state_for(sf),
        &far,
        owners_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(deep.step, "announced:1", "{deep:?}");

    // A newer target: a round of its own.
    let newer_round = visit_at(
        "newer",
        INCIDENT_SESSION,
        |sf, _| owners_state_for(sf),
        &spoken,
        owners_screen(),
        &newer(),
        usize::MAX,
    );
    let saved = newer_round.saved.clone().expect("state");
    assert_eq!(saved.to, "9.9.9");
    assert!(
        matches!(
            saved.phase,
            Phase::Pending | Phase::Announced { asks: 1, .. }
        ),
        "{newer_round:?}"
    );
    assert_ne!(saved.marker, LEDGER[3].1);

    // Negative control: the last notice read by the model, no READY.
    let read = spoken.replace(
        &auth_wall_row("2026-09-27T06:33:33.157Z", INCIDENT_SESSION),
        &model_row(
            "2026-09-27T06:33:40.000Z",
            INCIDENT_SESSION,
            "Not yet: two workflows still run.",
            "end_turn",
        ),
    );
    // A give-up that stands, stopped at this look: it rests, `wait:failed`.
    let stays = visit_at(
        "stays",
        INCIDENT_SESSION,
        |sf, now| St {
            failed_at: now,
            ..owners_state_for(sf)
        },
        &read,
        owners_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(stays.step, "wait:failed", "{stays:?}");
    assert!(stays.typed.is_empty());
    // The same give-up as 0.93.0 wrote it (no `failed_at`, a stop older than
    // the stamp): no stop is final, so it is re-armed at its first look (a new
    // round, new markers, nothing typed at that look).
    let old = visit_at(
        "stays-unstamped",
        INCIDENT_SESSION,
        |sf, _| owners_state_for(sf),
        &read,
        owners_screen(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(old.step, "rearmed:unanswered", "{old:?}");
    assert!(old.typed.is_empty());
    let saved = old.saved.clone().expect("state");
    assert_eq!(saved.phase, Phase::Pending);
    assert!(saved.marker.is_empty() && saved.markers.is_empty());
    assert!(saved.salt > INCIDENT_SALT + u64::from(upgrade::MAX_ASKS));
    assert!(
        old.ledger
            .iter()
            .any(|l| l.contains(r#""step":"rearmed:unanswered""#)),
        "{:?}",
        old.ledger
    );
}

/// A give-up taken back counts no ask twice (`St::notices`, the status's "Asked N times"): the
/// round typed four notices and counted four; the model read the first two and the wall answered
/// the last two, so once the login is back the give-up is taken back as announced twice
/// ([`upgrade::rearmed`]) and its third ask is typed again, already counted: the record still says
/// four. A record an older build wrote, with no list of its round's markers, cannot tell the ask
/// was typed before: one notice counts one at most.
#[cfg(unix)]
#[test]
fn a_give_up_taken_back_counts_no_ask_twice() {
    // The first two notices read by the model, the last two the wall's.
    let read_two = |transcript: String| {
        [
            ("2026-09-27T05:03:21.041Z", "2026-09-27T05:03:30.000Z"),
            ("2026-09-27T05:33:25.086Z", "2026-09-27T05:33:40.000Z"),
        ]
        .iter()
        .fold(transcript, |t, (wall, answer)| {
            t.replace(
                &auth_wall_row(wall, INCIDENT_SESSION),
                &model_row(
                    answer,
                    INCIDENT_SESSION,
                    "Not yet: two workflows still run.",
                    "end_turn",
                ),
            )
        })
    };
    let transcript = read_two(up_to_the_login(4));
    let round: Vec<String> = LEDGER.iter().map(|(_, m)| m.to_string()).collect();
    assert_eq!(upgrade::notices_received(&transcript, &round), 2);
    let taken_back = visit_at(
        "taken-back-count",
        INCIDENT_SESSION,
        // On the stand-in's build, as [`owners_state_for`] puts it.
        |sf, now| St {
            from: "1.0.0".to_string(),
            notices: 4,
            ..incident_state(Phase::Failed(GAVE_UP.to_string()), 4)(sf, now)
        },
        &transcript,
        logged_in_screen_json(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(taken_back.step, "announced:3", "{taken_back:?}");
    assert_eq!(taken_back.typed.len(), 1);
    assert!(taken_back.typed[0].contains(LEDGER[2].1));
    let saved = taken_back.saved.expect("state");
    assert!(matches!(saved.phase, Phase::Announced { asks: 3, .. }));
    assert_eq!(
        saved.notices, 4,
        "the third ask was counted when first typed"
    );
    // The record 0.93.0 wrote: no list of the round's markers.
    let legacy = visit_at(
        "taken-back-count-legacy",
        INCIDENT_SESSION,
        |sf, _| St {
            notices: 4,
            ..owners_state_for(sf)
        },
        &transcript,
        at_owners_seq(logged_in_screen_json()),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(legacy.step, "announced:3", "{legacy:?}");
    assert_eq!(
        legacy.saved.expect("state").notices,
        5,
        "one notice, one ask"
    );
}

/// The clock is held through the wall: a notice the model read ten hours ago,
/// whose wind-down turn the login wall then answered, is not asked again the
/// moment the login is back; its window starts at the lift the transcript
/// records (`wait:awaiting-ready`). Negative control: the same notice with no
/// wall after it has run its window out and is asked again.
#[cfg(unix)]
#[test]
fn the_clock_is_held_through_the_login_wall() {
    let now = now_s();
    let marker = LEDGER[0].1;
    // The conversation has a task (a person's prompt before the notice), or
    // the taskless rule would restart it afresh with no notice.
    let person = typed_row(
        &ts(now - 40_000),
        INCIDENT_SESSION,
        "Land the keeper phases, then keep going.",
        "typed",
    );
    let read_then_walled = [
        person.clone(),
        typed_row(
            &ts(now - 36_000),
            INCIDENT_SESSION,
            &notice_text(marker),
            "typed",
        ),
        model_row(
            &ts(now - 35_990),
            INCIDENT_SESSION,
            "Winding down; two workflows still run.",
            "end_turn",
        ),
        notification_row(&ts(now - 35_000), INCIDENT_SESSION),
        auth_wall_row(&ts(now - 35_000), INCIDENT_SESSION),
        duration_row(&ts(now - 35_000), INCIDENT_SESSION, 50),
    ]
    .join("\n")
        + "\n"
        + &login_rows(&ts(now - 60), INCIDENT_SESSION).join("\n")
        + "\n";
    let asked = |_: &SessionFile, _: u64| Phase::Announced {
        at_s: now - 36_000,
        asks: 1,
    };
    let st = |sf: &SessionFile, t: u64| St {
        phase: asked(sf, t),
        ..incident_state(Phase::Pending, 1)(sf, t)
    };
    let held = visit_at(
        "held",
        INCIDENT_SESSION,
        st,
        &read_then_walled,
        logged_in_screen_json(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(held.step, "wait:awaiting-ready", "{held:?}");
    assert!(held.typed.is_empty());
    assert!(matches!(
        held.saved.expect("state").phase,
        Phase::Announced { at_s, asks: 1 } if at_s >= now - 60
    ));
    // Negative control: no wall after it.
    let read_only = [
        person,
        typed_row(
            &ts(now - 36_000),
            INCIDENT_SESSION,
            &notice_text(marker),
            "typed",
        ),
        model_row(
            &ts(now - 35_990),
            INCIDENT_SESSION,
            "Winding down; two workflows still run.",
            "end_turn",
        ),
    ]
    .join("\n")
        + "\n";
    let st = |sf: &SessionFile, t: u64| St {
        phase: asked(sf, t),
        ..incident_state(Phase::Pending, 1)(sf, t)
    };
    let reasked = visit_at(
        "reasked",
        INCIDENT_SESSION,
        st,
        &read_only,
        logged_in_screen_json(),
        &to_2_1_283(),
        usize::MAX,
    );
    assert_eq!(reasked.step, "announced:2", "{reasked:?}");
}

/// The notices queued behind the weekly limit: none typed behind the first, a
/// copy typed again straight away at most `upgrade::REQUEUE_MAX` times and
/// past that once per rest or on the owner's word, a give-up spent on them
/// rested and re-armed, and the Tier-1 bind of
/// `harness_upgrade_limit_queue_model`, over this module's apparatus.
#[path = "upgrade_queued_tests.rs"]
mod queued;

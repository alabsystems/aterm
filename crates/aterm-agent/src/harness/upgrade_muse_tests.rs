// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

const S1: &str = "01a148f8-9301-7752-96db-fef456f88074";
const STORE: &str =
    "/Users//u/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse";

/// The relaunch carries how Muse reaches its provider and runs its tools,
/// drops what the session keeps (permission mode, model, effort) and the
/// first prompt, and resumes the witnessed session by its UUID — root
/// options on either side of `resume` read alike. NEGATIVE CONTROLS: the
/// launch's own ref (a name, `--last`) is never carried, and a dropped flag's
/// value never stands as a positional.
#[test]
fn a_relaunch_carries_the_provider_and_drops_what_the_session_keeps() {
    let launch = argv(&[
        STORE,
        "--base-url",
        "http://127.0.0.1:60955",
        "-c",
        "provider_retry.max_attempts=3",
        "--yolo",
        "--model",
        "lab-stub",
        "--reasoning-effort=xhigh",
        // `--effort` is `--reasoning-effort`'s alias: it too rewrites the
        // global settings.json, so carried it would move every later
        // session's default.
        "--effort",
        "high",
        "--effort=low",
        "--approval-mode",
        "never",
        // The sandbox's network mode is the session's permission snapshot.
        "--sandbox-network",
        "allow",
        "--disable-sandbox",
        "--disable-approval",
        "--approval-judge",
        "auto",
        "--trust-workspace",
        "resume",
        "my-session-name",
        "--permission-profile",
        "p1",
        "--provider=meta",
    ]);
    assert_eq!(
        rewrite_argv(&launch, Some(S1)),
        Ok(argv(&[
            "--base-url",
            "http://127.0.0.1:60955",
            "-c",
            "provider_retry.max_attempts=3",
            "--trust-workspace",
            "--provider=meta",
            "resume",
            S1,
        ]))
    );
    assert_eq!(resumed_ref(&launch).as_deref(), Some("my-session-name"));
    // A first prompt and `--last` are dropped; a plain launch resumes.
    let prompt = argv(&[STORE, "--base-url", "u", "positional hello"]);
    assert_eq!(
        rewrite_argv(&prompt, Some(S1)),
        Ok(argv(&["--base-url", "u", "resume", S1]))
    );
    assert_eq!(resumed_ref(&prompt), None);
    let last = argv(&[STORE, "resume", "--last", "--disable-shell"]);
    assert_eq!(
        rewrite_argv(&last, Some(S1)),
        Ok(argv(&["--disable-shell", "resume", S1]))
    );
    assert_eq!(resumed_ref(&last), None, "--last names no session");
    // With no session, the kept flags alone: a fresh start.
    assert_eq!(rewrite_argv(&prompt, None), Ok(argv(&["--base-url", "u"])));
    // After `--`, the prompt's words, never options.
    let dashed = argv(&[STORE, "--", "--yolo", "resume"]);
    assert_eq!(rewrite_argv(&dashed, Some(S1)), Ok(argv(&["resume", S1])));
}

/// A launch no relaunch brings back is refused with its word, and a flag
/// the table does not know refuses (nothing guesses its arity). NEGATIVE
/// CONTROL: a Session Name is never typed in place of the UUID.
#[test]
fn a_launch_no_resume_carries_is_refused() {
    for (words, why) in [
        (&[STORE, "--no-session-log"][..], "--no-session-log"),
        (&[STORE, "-w"], "-w"),
        (&[STORE, "--worktree", "create"], "--worktree"),
        (&[STORE, "--worktree-base", "HEAD"], "--worktree-base"),
        (&[STORE, "--workspace", "/w"], "--workspace"),
        (&[STORE, "exec", "hi"], "exec"),
        (&[STORE, "--base-url", "u", "login"], "login"),
        (&[STORE, "--help"], "--help"),
        (&[STORE, "resume", "-V"], "-V"),
    ] {
        assert_eq!(
            rewrite_argv(&argv(words), Some(S1)),
            Err(ArgvRefusal::NotResumable(why.to_string())),
            "{words:?}"
        );
    }
    assert_eq!(
        rewrite_argv(&argv(&[STORE, "--brand-new", "resume"]), Some(S1)),
        Err(ArgvRefusal::UnknownFlag("--brand-new".to_string()))
    );
    assert_eq!(
        rewrite_argv(&argv(&[STORE, "--model"]), Some(S1)),
        Err(ArgvRefusal::UnknownFlag("--model".to_string())),
        "a value-taking flag with no value"
    );
    assert!(matches!(
        rewrite_argv(&argv(&[STORE]), Some("my-session-name")),
        Err(ArgvRefusal::NotResumable(_))
    ));
}

/// The TUI is a plain launch or `resume`; `exec`, the tools, `--help` and
/// `--version` end as they were launched to. An unknown flag leaves it a
/// TUI (its relaunch is refused at the exit, with its word). NEGATIVE
/// CONTROL: `resume` itself is no tool.
#[test]
fn only_the_interactive_tui_is_snapshotted() {
    for words in [
        &[STORE][..],
        &[STORE, "--base-url", "u", "resume", S1],
        &[STORE, "hello"],
        &[STORE, "--yolo", "resume"],
        &[STORE, "--brand-new", "resume"],
        &[STORE, "--no-session-log"],
    ] {
        assert!(is_tui_argv(&argv(words)), "{words:?}");
    }
    for words in [
        &[STORE, "exec", "hi"][..],
        &[STORE, "--base-url", "u", "exec"],
        &[STORE, "skills"],
        &[STORE, "--help"],
        &[STORE, "-V"],
        &[STORE, "--brand-new", "--version"],
        &[STORE, "--brand-new", "x", "login"],
        &[STORE, "help"],
    ] {
        assert!(!is_tui_argv(&argv(words)), "{words:?}");
    }
}

/// Muse Code's measured exit table: its own exit and someone's SIGINT or
/// SIGTERM are clean, SIGHUP and SIGKILL crashes, 1 and 2 refusals of its
/// start. NEGATIVE CONTROLS: a table that knows only its own exit and the
/// signals someone sends (`0`, `129`, `130`, `143`: the model's
/// `RefusalAsCrash`) reads SIGHUP as someone's and 1 as a crash — neither is
/// Muse Code's.
#[test]
fn the_exit_table_is_muse_codes_own() {
    for (status, kind) in [
        (0, ExitKind::Clean),
        (130, ExitKind::Clean),
        (143, ExitKind::Clean),
        (1, ExitKind::Refused),
        (2, ExitKind::Refused),
        (129, ExitKind::Crashed),
        (137, ExitKind::Crashed),
        (139, ExitKind::Crashed),
        (134, ExitKind::Crashed),
        (101, ExitKind::Crashed),
    ] {
        assert_eq!(exit_kind(status), kind, "{status}");
    }
    let signals_only_crash = |status: i64| !matches!(status, 0 | 129 | 130 | 143);
    assert!(!signals_only_crash(129) && exit_kind(129) == ExitKind::Crashed);
    assert!(signals_only_crash(1) && exit_kind(1) == ExitKind::Refused);
    // The relaunch line's own block: a program the shell could not run is
    // a refusal of the line, never a crash to type it again for; every
    // other status reads as Muse Code's own. NEGATIVE CONTROL: Muse Code's
    // own table reads 127 as a crash.
    for (status, kind) in [
        (126, ExitKind::Refused),
        (127, ExitKind::Refused),
        (1, ExitKind::Refused),
        (2, ExitKind::Refused),
        (0, ExitKind::Clean),
        (143, ExitKind::Clean),
        (137, ExitKind::Crashed),
        (129, ExitKind::Crashed),
    ] {
        assert_eq!(line_exit_kind(status), kind, "{status}");
    }
    assert_eq!(exit_kind(127), ExitKind::Crashed);
}

/// The lines of a session log, its lifecycle records in the shape 1.4.10
/// writes them (the measured `session-main.jsonl`), the frame and other
/// records between.
fn log_of(events: &[&str]) -> String {
    let mut out = vec![
        r#"{"retained_frame":"session_permission_transaction","children":[{"record_json":"{\"payload_type\":\"session.started\"}"}]}"#.to_string(),
    ];
    for (i, e) in events.iter().enumerate() {
        let line = match *e {
            "start" => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"runtime.session_start.intake_v3","payload":{{"kind":"session_start_intake_v3","payload":{{"schema_version":3,"session_metadata":{{"workspace_root":"/private/tmp/at.DKNx/proj","provider_id":"meta"}}}}}}}}"#
            ),
            "started" => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"session.started","payload":{{"kind":"session_started","previous_session_stream":null}}}}"#
            ),
            "resumed" => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"session.resumed","payload":{{"kind":"session_resumed","record":{{"prior_turn_count":1}}}}}}"#
            ),
            "turn" => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"runtime.session","payload":{{"kind":"x"}}}}"#
            ),
            "moved" => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"runtime.session.metadata","payload":{{"kind":"metadata","record":{{"workspace_root":"/w/other"}}}}}}"#
            ),
            reason => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"session.end","payload":{{"kind":"session_end","record":{{"exit_reason":"{reason}","uptime_ms":5}}}}}}"#
            ),
        };
        out.push(line);
    }
    out.join("\n")
}

/// The fallback witness: a clean `session.end` after the LAST start is a
/// clean end, none a crash; the next run's `crash_inferred` (written before
/// its own start) is never that run's end. NEGATIVE CONTROLS: an earlier
/// run's clean end does not speak for a later run that recorded none, a log
/// with no start says nothing, and the nested frame's words are not records.
#[test]
fn the_session_log_tells_a_clean_end_from_a_crash() {
    assert_eq!(
        log_verdict(&log_of(&["start", "started", "turn", "clean"])),
        Some(ExitKind::Clean)
    );
    assert_eq!(
        log_verdict(&log_of(&["start", "started", "turn"])),
        Some(ExitKind::Crashed),
        "SIGKILL: no end"
    );
    assert_eq!(
        log_verdict(&log_of(&[
            "start", "started", "clean", "start", "started", "resumed", "turn"
        ])),
        Some(ExitKind::Crashed),
        "an earlier run's clean end is not this run's"
    );
    assert_eq!(
        log_verdict(&log_of(&[
            "start",
            "started",
            "turn",
            "crash_inferred",
            "start",
            "started",
            "resumed",
            "clean"
        ])),
        Some(ExitKind::Clean),
        "the next run's inferred crash is written before its own start"
    );
    assert_eq!(
        log_verdict(&log_of(&["start", "started", "error"])),
        Some(ExitKind::Crashed)
    );
    assert_eq!(log_verdict(&log_of(&["turn", "clean"])), None);
    assert_eq!(log_verdict(""), None);
}

/// The session's workspace is the newest its log names. NEGATIVE CONTROL:
/// a log naming none says none.
#[test]
fn the_workspace_is_the_newest_the_log_names() {
    assert_eq!(
        log_workspace(&log_of(&["start", "started"])).as_deref(),
        Some("/private/tmp/at.DKNx/proj")
    );
    assert_eq!(
        log_workspace(&log_of(&["start", "started", "moved"])).as_deref(),
        Some("/w/other")
    );
    assert_eq!(log_workspace(&log_of(&["started", "turn"])), None);
}

/// A lock names its pid until a clean exit truncates it. NEGATIVE
/// CONTROLS: an empty lock, another body, a zero pid.
#[test]
fn a_lock_names_its_holders_pid() {
    assert_eq!(lock_pid("pid=16390\nhost=Mac.localdomain\n"), Some(16390));
    assert_eq!(lock_pid(""), None);
    assert_eq!(lock_pid("host=x\n"), None);
    assert_eq!(lock_pid("pid=0\n"), None);
    assert_eq!(lock_pid("pid=abc\n"), None);
}

/// Only a lock at SESSION depth names a session. NEGATIVE CONTROLS: a
/// subagent's lock, a view directory's, a date that is not one, a session
/// that is no UUID, another root.
#[test]
fn only_a_session_depth_lock_names_a_session() {
    let root = Path::new("/h/.local/share/muse/sessions");
    let at = |rest: &str| root.join(rest);
    assert_eq!(
        session_of_lock(root, &at(&format!("2026/10/17/{S1}/.session.lock"))).as_deref(),
        Some(S1)
    );
    for rest in [
        format!("2026/10/17/{S1}/subagent/{S1}/.session.lock"),
        format!(".msp-view-g6/{S1}/.session.lock"),
        format!("2026/10/7/{S1}/.session.lock"),
        "2026/10/17/my-name/.session.lock".to_string(),
        format!("2026/10/17/{S1}/session.jsonl"),
    ] {
        assert_eq!(session_of_lock(root, &at(&rest)), None, "{rest}");
    }
    assert_eq!(
        session_of_lock(
            root,
            Path::new(&format!("/other/2026/10/17/{S1}/.session.lock"))
        ),
        None
    );
}

/// The data home is the process's `XDG_DATA_HOME`, else its `HOME`'s
/// `.local/share`, else the fallback's. NEGATIVE CONTROL: a relative
/// `XDG_DATA_HOME` names none; an empty one is unset.
#[test]
fn the_data_home_is_the_processs_own() {
    let fb = Path::new("/fallback");
    assert_eq!(
        data_home(Some("/x/data"), Some("/h"), fb),
        Some(PathBuf::from("/x/data"))
    );
    assert_eq!(
        data_home(None, Some("/h"), fb),
        Some(PathBuf::from("/h/.local/share"))
    );
    assert_eq!(
        data_home(Some(""), None, fb),
        Some(PathBuf::from("/fallback/.local/share"))
    );
    assert_eq!(data_home(Some("rel/data"), Some("/h"), fb), None);
    assert_eq!(
        data_home(None, Some("rel"), fb),
        Some(PathBuf::from("/fallback/.local/share"))
    );
}

/// The relaunch carries exactly the data and config homes the process was
/// started with, and its `HOME` where that is another than the user's (a
/// Muse under another `HOME` keeps its sessions there: relaunched under the
/// shell's, `resume` refuses), and a record keeps them. NEGATIVE CONTROLS:
/// the user's own `HOME`, a relative one and any other variable are never
/// carried or read back.
#[test]
fn the_relaunch_carries_the_homes_the_process_had() {
    let user = Path::new("/h");
    let env = relaunch_env(
        |name| match name {
            "XDG_DATA_HOME" => Some("/h/.local/share"),
            "XDG_CONFIG_HOME" => Some(""),
            "HOME" => Some("/h"),
            _ => None,
        },
        user,
    );
    assert_eq!(
        env,
        vec![
            ("XDG_DATA_HOME".to_string(), "/h/.local/share".to_string()),
            ("XDG_CONFIG_HOME".to_string(), String::new()),
        ]
    );
    assert_eq!(env_of_words(&env_words(&env)), env);
    // Another HOME, and no XDG variable: its HOME is what names its data.
    let other = relaunch_env(
        |name| match name {
            "HOME" => Some("/scratch/home"),
            "PATH" => Some("/bin"),
            _ => None,
        },
        user,
    );
    assert_eq!(
        other,
        vec![("HOME".to_string(), "/scratch/home".to_string())]
    );
    assert_eq!(env_of_words(&env_words(&other)), other);
    assert_eq!(
        told_line(S1, &other).as_deref(),
        Some(format!("HOME=/scratch/home muse resume {S1}").as_str())
    );
    assert_eq!(
        relaunch_env(|n| (n == "HOME").then_some("/h/"), user),
        Vec::new(),
        "the user's own HOME, however written"
    );
    assert_eq!(
        relaunch_env(|n| (n == "HOME").then_some("rel"), user),
        Vec::new()
    );
    assert_eq!(
        env_of_words(&["PATH=/bin".to_string(), "junk".to_string()]),
        Vec::new()
    );
    assert_eq!(relaunch_env(|_| None, user), Vec::new());
}

/// A store build is what a twin execs, never typed. NEGATIVE CONTROLS: the
/// twin itself, another program, another store's agent.
#[test]
fn a_store_build_is_told_from_the_twin() {
    assert!(store_build(Path::new(STORE)));
    assert!(!store_build(Path::new(
        "/Users//u/Library/Application Support/aterm/pkg/agents/muse"
    )));
    assert!(!store_build(Path::new("/opt/homebrew/bin/muse")));
    assert!(!store_build(Path::new("/p/store/codex/1/bin/muse")));
}

/// The told line is Muse's own, behind the homes. NEGATIVE CONTROLS: a
/// Session Name and a control character tell none.
#[test]
fn the_told_line_is_muses_own_resume_line() {
    assert_eq!(
        told_line(S1, &[]).as_deref(),
        Some(format!("muse resume {S1}").as_str())
    );
    let env = vec![(
        "XDG_DATA_HOME".to_string(),
        "/Users//u/Application Support/d".to_string(),
    )];
    assert_eq!(
        told_line(S1, &env).as_deref(),
        Some(format!("XDG_DATA_HOME='/Users//u/Application Support/d' muse resume {S1}").as_str())
    );
    assert_eq!(told_line("my-session-name", &[]), None);
    assert_eq!(
        told_line(S1, &[("XDG_DATA_HOME".into(), "a\nb".into())]),
        None
    );
}

/// Muse's own ids are UUIDv7, their birth in their first 48 bits.
/// NEGATIVE CONTROLS: a name, a v4 UUID's birth.
#[test]
fn a_session_id_is_a_uuid() {
    assert!(is_session_uuid(S1));
    assert!(is_session_uuid("0badbeef-0000-4000-8000-000000000000"));
    assert!(!is_session_uuid("my-session-name"));
    assert!(!is_session_uuid(&format!("{S1}0")));
    assert!(session_created_ms(S1).is_some());
    assert_eq!(
        session_created_ms("0badbeef-0000-4000-8000-000000000000"),
        None
    );
}

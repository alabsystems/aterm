// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// A conversation id as agy 1.4.0 printed one (`agy --conversation=…`).
const C1: &str = "eaeec4fb-4e43-47da-b46f-8a10c0fee186";
const STORE: &str =
    "/Users//u/Library/Application Support/aterm/pkg/store/agy/1000001000004000000/bin/agy";

/// Relaunch carries run-level flags, drops conversation-kept flags, and resumes the witnessed conversation.
#[test]
fn a_relaunch_carries_the_runs_choices_and_drops_what_the_conversation_keeps() {
    let launch = argv(&[
        STORE,
        "-effort=high",
        "--model",
        "gemini-3.6-flash-low",
        "-mode",
        "plan",
        "--gemini_dir=/Users//u/alt-gemini",
        "-app_data_dir",
        "agyalt",
        "--agent",
        "labagent",
        "-add-dir",
        "/w/other",
        "--project=default-cli-project",
        "--new-project",
        "-conversation",
        "11111111-2222-4333-8444-555555555555",
        "-c",
        "-i",
        "carry on from argv",
        "--dangerously-skip-permissions",
        "--sandbox=true",
        "--log-file",
        "/tmp/agy.log",
    ]);
    assert_eq!(
        rewrite_argv(&launch, Some(C1)),
        Ok(argv(&[
            "-effort=high",
            "--model",
            "gemini-3.6-flash-low",
            "-mode",
            "plan",
            "--gemini_dir=/Users//u/alt-gemini",
            "-app_data_dir",
            "agyalt",
            "--dangerously-skip-permissions",
            "--sandbox=true",
            "--log-file",
            "/tmp/agy.log",
            &format!("--conversation={C1}"),
        ]))
    );
    assert_eq!(
        resumed_conversation(&launch).as_deref(),
        Some("11111111-2222-4333-8444-555555555555"),
        "the launch's own, which the witness may have moved past"
    );
    let plain = argv(&[STORE, "--effort", "low"]);
    assert_eq!(rewrite_argv(&plain, None), Ok(argv(&["--effort", "low"])));
    assert_eq!(resumed_conversation(&plain), None);
    let switch = argv(&[STORE, "--sandbox", "--model", "x"]);
    assert_eq!(
        rewrite_argv(&switch, Some(C1)),
        Ok(argv(&[
            "--sandbox",
            "--model",
            "x",
            &format!("--conversation={C1}")
        ]))
    );
    assert_eq!(
        rewrite_argv(&argv(&[STORE, "--effort=low", "--"]), None),
        Ok(argv(&["--effort=low"]))
    );
}

/// Non-resumable launches, unknown flags, and non-UUIDv4 conversation ids are refused.
#[test]
fn a_launch_no_resume_carries_is_refused() {
    for (words, why) in [
        (&[STORE, "--remote-control"][..], "remote-control"),
        (&[STORE, "-print-timeout=5s"], "print-timeout"),
        (&[STORE, "--input-format", "stream-json"], "input-format"),
        (&[STORE, "--json-schema", "{}"], "json-schema"),
        (
            &[STORE, "--disable-slash-commands"],
            "disable-slash-commands",
        ),
        (&[STORE, "-p", "hi"], "p"),
        (&[STORE, "--output-format", "json"], "output-format"),
        (&[STORE, "--version"], "version"),
        (&[STORE, "models"], "models"),
        (&[STORE, "--effort", "high", "hello"], "positional"),
        (&[STORE, "--", "hello"], "positional"),
    ] {
        assert_eq!(
            rewrite_argv(&argv(words), Some(C1)),
            Err(ArgvRefusal::NotResumable(why.to_string())),
            "{words:?}"
        );
    }
    assert_eq!(
        rewrite_argv(&argv(&[STORE, "--brand-new", "x"]), Some(C1)),
        Err(ArgvRefusal::UnknownFlag("brand-new".to_string()))
    );
    assert_eq!(
        rewrite_argv(&argv(&[STORE, "--model"]), Some(C1)),
        Err(ArgvRefusal::UnknownFlag("model".to_string())),
        "a value-taking flag with no value"
    );
    for bad in [
        "my-conversation",
        "01a148f8-9301-7752-96db-fef456f88074",
        "EAEEC4FB-4E43-47DA-B46F-8A10C0FEE186",
    ] {
        assert!(
            matches!(
                rewrite_argv(&argv(&[STORE]), Some(bad)),
                Err(ArgvRefusal::NotResumable(_))
            ),
            "{bad}"
        );
    }
}

/// Only interactive TUI launches are snapshotted; one-shots, subcommands, and positionals are not.
#[test]
fn only_the_interactive_tui_is_snapshotted() {
    for words in [
        &[STORE][..],
        &[STORE, &format!("--conversation={C1}")],
        &[STORE, "-conversation", C1, "--effort", "high"],
        &[STORE, "--remote-control"],
        &[STORE, "--brand-new"],
        &[STORE, "-i", "start here"],
        &[STORE, "--"],
    ] {
        assert!(is_tui_argv(&argv(words)), "{words:?}");
    }
    for words in [
        &[STORE, "-p", "hi"][..],
        &[STORE, "--print=hi"],
        &[STORE, "--output-format", "json"],
        &[STORE, "--version"],
        &[STORE, "-h"],
        &[STORE, "help"],
        &[STORE, "models"],
        &[STORE, "mcp", "list"],
        &[STORE, "hello"],
        &[STORE, "--", "hello"],
        &[STORE, "--brand-new", "x", "models"],
        &[STORE, "--brand-new", "--version"],
    ] {
        assert!(!is_tui_argv(&argv(words)), "{words:?}");
    }
}

/// Ctrl-Z's status (`128 + SIGTSTP`).
#[cfg(unix)]
fn ctrl_z() -> i64 {
    128 + i64::from(libc::SIGTSTP)
}

/// Exit status and crash-file presence classify clean exits, stops, crashes, and refusals.
#[test]
fn the_exit_table_is_agys_own() {
    let status_alone = |status: i64| status != 0;
    #[cfg(unix)]
    {
        let stop = |signal: libc::c_int| 128 + i64::from(signal);
        #[cfg(target_os = "macos")]
        assert_eq!(ctrl_z(), 146, "Ctrl-Z's, as measured on macOS");
        for signal in [libc::SIGTSTP, libc::SIGSTOP, libc::SIGTTIN, libc::SIGTTOU] {
            assert!(stopped_status(stop(signal)), "{signal}");
            for crash_left in [true, false] {
                assert_eq!(
                    exit_kind(stop(signal), crash_left),
                    ExitKind::Stopped,
                    "{signal} {crash_left}"
                );
            }
            assert_eq!(line_exit_kind(stop(signal), true), ExitKind::Stopped);
        }
        for status in [0, 1, 2, 137, 143, stop(libc::SIGCONT)] {
            assert!(!stopped_status(status), "{status}");
        }
        assert!(status_alone(ctrl_z()));
        assert_ne!(
            exit_kind(ctrl_z(), true),
            ExitKind::Crashed,
            "crash file alone"
        );
    }
    for (status, crash_left, kind) in [
        (0, false, ExitKind::Clean),
        (0, true, ExitKind::Clean),
        (137, true, ExitKind::Crashed),
        (2, true, ExitKind::Crashed),
        (139, true, ExitKind::Crashed),
        (2, false, ExitKind::Refused),
        (137, false, ExitKind::Unwitnessed),
        (1, false, ExitKind::Unwitnessed),
    ] {
        assert_eq!(exit_kind(status, crash_left), kind, "{status} {crash_left}");
    }
    for (status, crash_left) in [(2, false), (137, false)] {
        assert!(status_alone(status));
        assert_ne!(exit_kind(status, crash_left), ExitKind::Crashed, "{status}");
    }
    for (status, crash_left, kind) in [
        (126, false, ExitKind::Refused),
        (127, false, ExitKind::Refused),
        (1, false, ExitKind::Refused),
        (2, false, ExitKind::Refused),
        (0, false, ExitKind::Clean),
        (137, true, ExitKind::Crashed),
        (1, true, ExitKind::Crashed),
        (137, false, ExitKind::Unwitnessed),
    ] {
        assert_eq!(line_exit_kind(status, crash_left), kind, "{status}");
    }
    assert_eq!(exit_kind(127, false), ExitKind::Unwitnessed);
}

/// Config directory resolves from `$HOME` and `--gemini_dir` / `--app_data_dir`.
#[test]
fn the_config_is_where_home_and_the_hidden_flags_put_it() {
    let fb = Path::new("/fallback");
    assert_eq!(
        config_dir(&argv(&[STORE]), Some("/h"), fb),
        Some(PathBuf::from("/h/.gemini/antigravity-cli"))
    );
    assert_eq!(
        config_dir(&argv(&[STORE, "--gemini_dir=/g"]), Some("/h"), fb),
        Some(PathBuf::from("/g/antigravity-cli"))
    );
    assert_eq!(
        config_dir(&argv(&[STORE, "-app_data_dir", "agyalt"]), Some("/h"), fb),
        Some(PathBuf::from("/h/.gemini/agyalt"))
    );
    assert_eq!(
        config_dir(
            &argv(&[
                STORE,
                "-gemini_dir",
                "/g1",
                "--gemini_dir=/g2",
                "--app_data_dir=x/y"
            ]),
            None,
            fb
        ),
        Some(PathBuf::from("/g2/x/y"))
    );
    assert_eq!(
        config_dir(&argv(&[STORE, "--gemini_dir=.gemini"]), Some("/h"), fb),
        Some(PathBuf::from("/h/.gemini/antigravity-cli")),
        "a relative --gemini_dir is ignored"
    );
    assert_eq!(
        config_dir(&argv(&[STORE]), Some("rel"), fb),
        Some(PathBuf::from("/fallback/.gemini/antigravity-cli"))
    );
    for bad in ["/abs", "../up", "a/../b", ""] {
        assert_eq!(
            config_dir(
                &argv(&[STORE, &format!("--app_data_dir={bad}")]),
                Some("/h"),
                fb
            ),
            None,
            "{bad}"
        );
    }
    assert_eq!(
        config_flags(&argv(&[
            STORE,
            "--effort=high",
            "-gemini_dir",
            "/g",
            "--app_data_dir=x"
        ])),
        argv(&["-gemini_dir", "/g", "--app_data_dir=x"])
    );
}

/// Presence locks name their conversation UUIDv4 and crash files match their pid.
#[test]
fn the_lock_names_the_conversation_and_the_crash_file_its_pid() {
    let config = Path::new("/h/.gemini/antigravity-cli");
    assert_eq!(
        presence_lock(config, C1),
        config.join(format!("presence/{C1}.lock"))
    );
    assert_eq!(
        lock_conversation(config, &presence_lock(config, C1)).as_deref(),
        Some(C1)
    );
    for rest in [
        format!("conversations/{C1}.db"),
        format!("presence/{C1}.db"),
        "presence/01a148f8-9301-7752-96db-fef456f88074.lock".to_string(),
        format!("presence/sub/{C1}.lock"),
    ] {
        assert_eq!(
            lock_conversation(config, &config.join(&rest)),
            None,
            "{rest}"
        );
    }
    assert_eq!(
        lock_conversation(config, Path::new(&format!("/other/presence/{C1}.lock"))),
        None
    );
    let crash = config.join("crashes/crash_41234_9c1f0d6e-2b7a-4c55-9a3e-1f2e3d4c5b6a.log");
    assert!(crash_file(config, &crash, 41234));
    assert!(!crash_file(config, &crash, 41235), "another pid's");
    assert!(!crash_file(
        config,
        &config.join("log/crash_41234_9c1f0d6e-2b7a-4c55-9a3e-1f2e3d4c5b6a.log"),
        41234
    ));
    assert_eq!(crash_file_pid("crash_7_x.log"), None);
    assert_eq!(
        crash_file_pid("crash_0_9c1f0d6e-2b7a-4c55-9a3e-1f2e3d4c5b6a.log"),
        None
    );
}

/// An agy with an open conversation database and no presence lock under its config holds no lock.
#[test]
fn an_agy_without_a_lock_is_told_by_what_it_holds() {
    const C2: &str = "c688fa56-6a00-49a5-a1d1-5386b0df8b4d";
    let config = Path::new("/h/.gemini/antigravity-cli");
    let db = conversation_db(config, C1);
    assert_eq!(db, config.join(format!("conversations/{C1}.db")));
    let presence = config.join(PRESENCE);
    let opener = [
        db.clone(),
        config.join(format!("conversations/{C1}.db-wal")),
        presence.clone(),
    ];
    assert!(
        holds_no_lock(config, &opener),
        "opened on a held conversation"
    );
    let holder = [db.clone(), presence_lock(config, C1)];
    assert!(!holds_no_lock(config, &holder), "the lock's holder");
    let cleared = [
        db.clone(),
        conversation_db(config, C2),
        presence_lock(config, C2),
    ];
    assert!(!holds_no_lock(config, &cleared), "moved on by a /clear");
    assert!(
        holds_no_lock(config, &[presence]),
        "fresh: the directory alone"
    );
    let elsewhere = Path::new("/other/.gemini/antigravity-cli");
    assert!(holds_no_lock(config, &[db, presence_lock(elsewhere, C1)]));
}

/// Relaunch environment carries only a non-default absolute `HOME`.
#[test]
fn the_relaunch_carries_another_home_alone() {
    let user = Path::new("/h");
    let other = relaunch_env(
        |name| match name {
            "HOME" => Some("/scratch/agy-home"),
            "XDG_CONFIG_HOME" => Some("/x"),
            _ => None,
        },
        user,
    );
    assert_eq!(
        other,
        vec![("HOME".to_string(), "/scratch/agy-home".to_string())]
    );
    assert_eq!(env_of_words(&env_words(&other)), other);
    assert_eq!(line_env(&other), vec![("HOME", "/scratch/agy-home")]);
    for home in ["/h", "/h/", "rel"] {
        assert_eq!(
            relaunch_env(|n| (n == "HOME").then_some(home), user),
            Vec::new(),
            "{home}"
        );
    }
    assert_eq!(
        env_of_words(&["XDG_CONFIG_HOME=/x".to_string(), "junk".to_string()]),
        Vec::new()
    );
}

/// A store build path is distinguished from the agent twin and other binaries.
#[test]
fn a_store_build_is_told_from_the_twin() {
    assert!(store_build(Path::new(STORE)));
    assert!(!store_build(Path::new(
        "/Users//u/Library/Application Support/aterm/pkg/agents/agy"
    )));
    assert!(!store_build(Path::new("/opt/homebrew/bin/agy")));
    assert!(!store_build(Path::new("/p/store/muse/1/bin/agy")));
}

/// The resume hint includes `HOME` and config-moving flags before `--conversation=<id>`.
#[test]
fn the_told_line_is_agys_own_resume_hint() {
    assert_eq!(
        told_line(C1, &[], &argv(&[STORE, "--effort", "high"])).as_deref(),
        Some(format!("agy --conversation={C1}").as_str())
    );
    let env = vec![("HOME".to_string(), "/Users//u/agy home".to_string())];
    assert_eq!(
        told_line(C1, &env, &argv(&[STORE, "--gemini_dir=/g", "--model", "m"])).as_deref(),
        Some(format!("HOME='/Users//u/agy home' agy --gemini_dir=/g --conversation={C1}").as_str())
    );
    assert_eq!(told_line("my-conversation", &[], &argv(&[STORE])), None);
    assert_eq!(
        told_line(C1, &[("HOME".into(), "a\nb".into())], &argv(&[STORE])),
        None
    );
}

/// Conversation ids must be lowercase UUIDv4.
#[test]
fn a_conversation_id_is_a_uuid_v4() {
    assert!(is_conversation_uuid(C1));
    assert!(is_conversation_uuid("11111111-2222-4333-8444-555555555555"));
    for bad in [
        "01a148f8-9301-7752-96db-fef456f88074",
        "my-conversation",
        &format!("{C1}0"),
        "EAEEC4FB-4E43-47DA-B46F-8A10C0FEE186",
        "eaeec4fb-4e43-47da-c46f-8a10c0fee186",
    ] {
        assert!(!is_conversation_uuid(bad), "{bad}");
    }
}

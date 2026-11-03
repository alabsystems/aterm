// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::host_path::abs;

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

const S1: &str = "01a148f8-9301-7752-96db-fef456f88074";
const STORE: &str =
    "/Users//u/Library/Application Support/aterm/pkg/store/muse/2001004010006427002/bin/muse";

#[test]
fn a_relaunch_carries_the_provider_and_its_launch_policy_and_drops_what_the_session_keeps() {
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
        "--effort",
        "high",
        "--effort=low",
        "--approval-mode",
        "never",
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
            "--yolo",
            "--approval-mode",
            "never",
            "--sandbox-network",
            "allow",
            "--disable-sandbox",
            "--disable-approval",
            "--approval-judge",
            "auto",
            "--trust-workspace",
            "--permission-profile",
            "p1",
            "--provider=meta",
            "--contributor-mode",
            "off",
            "resume",
            S1,
        ]))
    );
    assert_eq!(resumed_ref(&launch).as_deref(), Some("my-session-name"));
    let prompt = argv(&[STORE, "--base-url", "u", "positional hello"]);
    assert_eq!(
        rewrite_argv(&prompt, Some(S1)),
        Ok(argv(&[
            "--base-url",
            "u",
            "--contributor-mode",
            "off",
            "resume",
            S1
        ]))
    );
    assert_eq!(resumed_ref(&prompt), None);
    let last = argv(&[STORE, "resume", "--last", "--disable-shell"]);
    assert_eq!(
        rewrite_argv(&last, Some(S1)),
        Ok(argv(&[
            "--disable-shell",
            "--contributor-mode",
            "off",
            "resume",
            S1
        ]))
    );
    assert_eq!(resumed_ref(&last), None, "--last names no session");
    assert_eq!(
        rewrite_argv(&prompt, None),
        Ok(argv(&[
            "--base-url",
            "u",
            "--contributor-mode",
            "off",
            "--model",
            "muse-wonder-full-1.0",
            "--reasoning-effort",
            "xhigh",
        ]))
    );
    let dashed = argv(&[STORE, "--", "--yolo", "resume"]);
    assert_eq!(
        rewrite_argv(&dashed, Some(S1)),
        Ok(argv(&["--contributor-mode", "off", "resume", S1]))
    );
    for attached in ["-cprovider=meta", "-c=provider=meta"] {
        assert_eq!(
            rewrite_argv(&argv(&[STORE, attached]), Some(S1)),
            Ok(argv(&[attached, "--contributor-mode", "off", "resume", S1])),
            "{attached}"
        );
    }
}

#[test]
fn a_worker_follows_how_its_starter_reaches_its_provider() {
    let launch = argv(&[
        STORE,
        "--yolo",
        "--base-url",
        "http://127.0.0.1:60955",
        "--model",
        "lab-stub",
        "-c",
        "provider_retry.max_attempts=3",
        "--trust-workspace",
        "resume",
        "my-session-name",
        "--config=a.b=1",
        "--provider=meta",
        "--preset",
        "fast",
        "--",
        "--provider",
        "x",
    ]);
    assert_eq!(
        provider_flags(&launch),
        Ok(argv(&[
            "--base-url",
            "http://127.0.0.1:60955",
            "-c",
            "provider_retry.max_attempts=3",
            "--config=a.b=1",
            "--provider=meta",
            "--preset",
            "fast",
        ]))
    );
    assert_eq!(
        provider_flags(&argv(&[STORE, "--yolo", "--model", "m", "a prompt"])),
        Ok(Vec::new())
    );
    assert_eq!(
        provider_flags(&argv(&[STORE, "-cprovider=meta", "-c=provider=local"])),
        Ok(argv(&["-cprovider=meta", "-c=provider=local"]))
    );
    assert_eq!(
        provider_flags(&argv(&[STORE, "--brand-new", "x", "--provider", "p"])),
        Err(ArgvRefusal::UnknownFlag("--brand-new".to_string()))
    );
    assert_eq!(
        provider_flags(&argv(&[STORE, "--provider"])),
        Err(ArgvRefusal::UnknownFlag("--provider".to_string())),
        "a value-taking flag with no value"
    );
}

#[test]
fn a_launch_no_resume_carries_is_refused() {
    for (words, why) in [
        (&[STORE, "--no-session-log"][..], "--no-session-log"),
        (&[STORE, "-w"], "-w"),
        (&[STORE, "-wcreate"], "-w"),
        (&[STORE, "-w=create"], "-w"),
        (&[STORE, "--worktree", "create"], "--worktree"),
        (&[STORE, "--worktree=create"], "--worktree"),
        (&[STORE, "--worktree-base", "HEAD"], "--worktree-base"),
        (&[STORE, "--workspace", "/w"], "--workspace"),
        (&[STORE, "exec", "hi"], "exec"),
        (&[STORE, "usage"], "usage"),
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
        rewrite_argv(&argv(&[STORE, "-hfoo"]), Some(S1)),
        Err(ArgvRefusal::UnknownFlag("-hfoo".to_string())),
        "a switch takes no attached value"
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

#[test]
fn only_the_interactive_tui_is_snapshotted() {
    for words in [
        &[STORE][..],
        &[STORE, "--base-url", "u", "resume", S1],
        &[STORE, "hello"],
        &[STORE, "--yolo", "resume"],
        &[STORE, "--brand-new", "resume"],
        &[STORE, "--no-session-log"],
        &[STORE, "--model", "exec", "--brand-new"],
        &[STORE, "--brand-new", "resume", "login"],
        &[STORE, "fix login", "login"],
    ] {
        assert!(is_tui_argv(&argv(words)), "{words:?}");
    }
    for words in [
        &[STORE, "exec", "hi"][..],
        &[STORE, "--base-url", "u", "exec"],
        &[STORE, "skills"],
        &[STORE, "--base-url", "u", "usage"],
        &[STORE, "--help"],
        &[STORE, "-V"],
        &[STORE, "--brand-new", "--version"],
        &[STORE, "--brand-new", "x", "login"],
        &[STORE, "help"],
        &[STORE, "--workspace", "/w", "exec", "hi"],
        &[STORE, "--no-session-log", "login"],
        &[STORE, "--no-session-log", "--help"],
        &[STORE, "--model", "--help"],
    ] {
        assert!(!is_tui_argv(&argv(words)), "{words:?}");
    }
    assert!(resumes(&argv(&[STORE, "--workspace", "/w", "resume"])));
    assert!(!resumes(&argv(&[STORE, "resume", "--help"])));
    assert!(shared_key_defaults(&argv(&["exec", "usage"])));
    assert!(shared_key_defaults(&argv(&["explain", "usage"])));
    assert!(!shared_key_defaults(&argv(&[
        "explain",
        "exec",
        "--prompt-file",
        "/tmp/p"
    ])));
}

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

fn log_of(events: &[&str]) -> String {
    let (proj, other) = (abs("/private/tmp/at.DKNx/proj"), abs("/w/other"));
    let mut out = vec![
        r#"{"retained_frame":"session_permission_transaction","children":[{"record_json":"{\"payload_type\":\"session.started\"}"}]}"#.to_string(),
    ];
    for (i, e) in events.iter().enumerate() {
        let line = match *e {
            "start" => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"runtime.session_start.intake_v3","payload":{{"kind":"session_start_intake_v3","payload":{{"schema_version":3,"session_metadata":{{"workspace_root":"{proj}","provider_id":"meta"}}}}}}}}"#
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
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"runtime.session.metadata","payload":{{"kind":"metadata","record":{{"workspace_root":"{other}"}}}}}}"#
            ),
            reason => format!(
                r#"{{"schema_version":1,"sequence":{i},"record_type":"event","payload_type":"session.end","payload":{{"kind":"session_end","record":{{"exit_reason":"{reason}","uptime_ms":5}}}}}}"#
            ),
        };
        out.push(line);
    }
    out.join("\n")
}

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

#[test]
fn the_workspace_is_the_newest_the_log_names() {
    assert_eq!(
        log_workspace(&log_of(&["start", "started"])),
        Some(abs("/private/tmp/at.DKNx/proj"))
    );
    assert_eq!(
        log_workspace(&log_of(&["start", "started", "moved"])),
        Some(abs("/w/other"))
    );
    assert_eq!(log_workspace(&log_of(&["started", "turn"])), None);
}

#[test]
fn a_lock_names_its_holders_pid() {
    assert_eq!(lock_pid("pid=16390\nhost=Mac.localdomain\n"), Some(16390));
    assert_eq!(lock_pid(""), None);
    assert_eq!(lock_pid("host=x\n"), None);
    assert_eq!(lock_pid("pid=0\n"), None);
    assert_eq!(lock_pid("pid=1\n"), None);
    assert_eq!(lock_pid("pid=016390\n"), None);
    assert_eq!(lock_pid("pid=abc\n"), None);
}

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

#[test]
fn the_data_home_is_the_processs_own() {
    let (data, h, fb) = (abs("/x/data"), abs("/h"), PathBuf::from(abs("/fallback")));
    assert_eq!(
        data_home(Some(&data), Some(&h), &fb),
        Some(PathBuf::from(&data))
    );
    assert_eq!(
        data_home(None, Some(&h), &fb),
        Some(PathBuf::from(abs("/h/.local/share")))
    );
    assert_eq!(
        data_home(Some(""), None, &fb),
        Some(PathBuf::from(abs("/fallback/.local/share")))
    );
    assert_eq!(data_home(Some("rel/data"), Some(&h), &fb), None);
    assert_eq!(
        data_home(None, Some("rel"), &fb),
        Some(PathBuf::from(abs("/fallback/.local/share")))
    );
}

#[test]
fn the_relaunch_carries_the_homes_the_process_had() {
    let (h, data, scratch) = (abs("/h"), abs("/h/.local/share"), abs("/scratch/home"));
    let user = Path::new(&h);
    let env = relaunch_env(
        |name| match name {
            "XDG_DATA_HOME" => Some(data.as_str()),
            "XDG_CONFIG_HOME" => Some(""),
            "HOME" => Some(h.as_str()),
            _ => None,
        },
        user,
    );
    assert_eq!(
        env,
        vec![
            ("XDG_DATA_HOME".to_string(), data.clone()),
            ("XDG_CONFIG_HOME".to_string(), String::new()),
        ]
    );
    assert_eq!(env_of_words(&env_words(&env)), env);
    let other = relaunch_env(
        |name| match name {
            "HOME" => Some(scratch.as_str()),
            "PATH" => Some("/bin"),
            _ => None,
        },
        user,
    );
    assert_eq!(other, vec![("HOME".to_string(), scratch.clone())]);
    assert_eq!(env_of_words(&env_words(&other)), other);
    assert_eq!(
        told_line(S1, &other),
        Some(format!(
            "HOME={scratch} muse --contributor-mode off resume {S1}"
        ))
    );
    let slashed = abs("/h/");
    assert_eq!(
        relaunch_env(|n| (n == "HOME").then_some(slashed.as_str()), user),
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

#[test]
fn a_store_build_is_told_from_the_twin() {
    assert!(store_build(Path::new(STORE)));
    assert!(!store_build(Path::new(
        "/Users//u/Library/Application Support/aterm/pkg/agents/muse"
    )));
    assert!(!store_build(Path::new("/opt/homebrew/bin/muse")));
    assert!(!store_build(Path::new("/p/store/codex/1/bin/muse")));
    assert!(!store_build(Path::new("/p/store/muse/01/bin/muse")));
    assert!(!store_build(Path::new(
        "/p/store/muse/not-a-build/bin/muse"
    )));
    assert!(!store_build(Path::new("/p/store/muse/../bin/muse")));
    assert!(!store_build(Path::new("/p/../store/muse/1/bin/muse")));
    assert!(!store_build(Path::new("store/muse/1/bin/muse")));
}

#[test]
fn the_told_line_is_muses_own_resume_line() {
    assert_eq!(
        told_line(S1, &[]).as_deref(),
        Some(format!("muse --contributor-mode off resume {S1}").as_str())
    );
    let env = vec![(
        "XDG_DATA_HOME".to_string(),
        "/Users//u/Application Support/d".to_string(),
    )];
    assert_eq!(
        told_line(S1, &env).as_deref(),
        Some(
            format!(
                "XDG_DATA_HOME='/Users//u/Application Support/d' muse --contributor-mode off \
                 resume {S1}"
            )
            .as_str()
        )
    );
    assert_eq!(told_line("my-session-name", &[]), None);
    assert_eq!(
        told_line(S1, &[("XDG_DATA_HOME".into(), "a\nb".into())]),
        None
    );
}

#[test]
fn a_relaunch_runs_contributor_mode_off_and_a_fresh_start_the_default_model() {
    // A launch that turned contributor mode on (either spelling), and one that
    // never named it: the relaunch carries it off, exactly once.
    for words in [
        &[STORE, "--contributor-mode", "on"][..],
        &[STORE, "--contributor-mode=on"][..],
        &[STORE][..],
    ] {
        assert_eq!(
            rewrite_argv(&argv(words), Some(S1)),
            Ok(argv(&["--contributor-mode", "off", "resume", S1])),
            "{words:?}"
        );
    }
    // A fresh start (no session to resume) runs the default model at xhigh.
    assert_eq!(
        rewrite_argv(&argv(&[STORE]), None),
        Ok(argv(&[
            "--contributor-mode",
            "off",
            "--model",
            "muse-wonder-full-1.0",
            "--reasoning-effort",
            "xhigh",
        ]))
    );
}

#[test]
fn a_session_id_is_a_uuid() {
    assert!(is_session_uuid(S1));
    assert!(is_session_uuid("0badbeef-0000-4000-8000-000000000000"));
    assert!(!is_session_uuid("my-session-name"));
    assert!(!is_session_uuid(&format!("{S1}0")));
}

#[test]
fn launch_endpoint_guards_mtls_auth_and_custom_ca_env_vars() {
    assert_eq!(
        launch_endpoint(
            &argv(&[
                STORE,
                "--base-url",
                "https://muse.internal/v1",
                "-c",
                "endpoint_transport.auth=\"bearer\"",
            ]),
            &[],
        ),
        LaunchEndpoint::BaseUrl {
            url: "https://muse.internal/v1".to_string(),
            source: "Muse Code's --base-url",
        },
        "bearer transport auth stays probeable"
    );
    assert_eq!(
        launch_endpoint(
            &argv(&[
                STORE,
                "-c",
                "endpoint_transport={\"base_url\":\"https://muse.internal/v1\",\"auth\":{\"type\":\"bearer\"}}",
            ]),
            &[],
        ),
        LaunchEndpoint::BaseUrl {
            url: "https://muse.internal/v1".to_string(),
            source: "Muse Code's -c endpoint_transport.base_url",
        },
        "typed bearer object stays probeable"
    );

    for mtls in [
        "endpoint_transport.auth={\"client_cert\":\"/etc/tls/c.pem\",\"client_key\":\"/etc/tls/k.pem\"}",
        "endpoint_transport={\"base_url\":\"https://muse.internal/v1\",\"auth\":{\"client_cert\":\"/etc/tls/c.pem\"}}",
        "endpoint_transport.auth.client_cert=\"/etc/tls/c.pem\"",
        "endpoint_transport.auth=mtls",
    ] {
        let got = launch_endpoint(
            &argv(&[STORE, "--base-url", "https://muse.internal/v1", "-c", mtls]),
            &[],
        );
        assert!(
            matches!(got, LaunchEndpoint::Gap(ref why) if why.contains("endpoint_transport.auth")),
            "expected mTLS gap for {mtls}, got {got:?}"
        );
    }

    for ca_var in ["TBH_CA_CERTIFICATE", "SSL_CERT_FILE", "SSL_CERT_DIR"] {
        let env = vec![format!("{ca_var}=/etc/ssl/corp-ca.pem")];
        let https_got = launch_endpoint(
            &argv(&[STORE, "--base-url", "https://muse.internal/v1"]),
            &env,
        );
        assert!(
            matches!(https_got, LaunchEndpoint::Gap(ref why) if why.contains(ca_var)),
            "expected custom CA gap for {ca_var}, got {https_got:?}"
        );
        let http_got = launch_endpoint(
            &argv(&[STORE, "--base-url", "http://127.0.0.1:18080/v1"]),
            &env,
        );
        assert_eq!(
            http_got,
            LaunchEndpoint::BaseUrl {
                url: "http://127.0.0.1:18080/v1".to_string(),
                source: "Muse Code's --base-url",
            },
            "plain http lab stub stays probeable even when {ca_var} is set in env"
        );
    }
}

#[test]
fn launch_endpoint_guards_presets_partial_transport_merges_and_provider_precedence() {
    // `--preset` and non-empty `-c preset=...` select external profile transport settings.
    for preset_argv in [
        argv(&[
            STORE,
            "--preset",
            "corp-mtls",
            "--base-url",
            "https://muse.internal/v1",
        ]),
        argv(&[
            STORE,
            "-c",
            "preset=\"corp-mtls\"",
            "--base-url",
            "https://muse.internal/v1",
        ]),
    ] {
        let got = launch_endpoint(&preset_argv, &[]);
        assert!(
            matches!(got, LaunchEndpoint::Gap(ref why) if why.contains("preset")),
            "expected preset gap for {preset_argv:?}, got {got:?}"
        );
    }

    // `-c preset=null` clears an earlier `-c preset=...`.
    assert_eq!(
        launch_endpoint(
            &argv(&[
                STORE,
                "-c",
                "preset=\"corp\"",
                "-c",
                "preset=null",
                "--base-url",
                "https://muse.internal/v1",
            ]),
            &[],
        ),
        LaunchEndpoint::BaseUrl {
            url: "https://muse.internal/v1".to_string(),
            source: "Muse Code's --base-url",
        }
    );

    // A partial `-c endpoint_transport={...}` object preserves earlier proxy/CA/auth/base_url keys
    // unless `endpoint_transport=null` clears the whole object.
    let partial_after_proxy = launch_endpoint(
        &argv(&[
            STORE,
            "-c",
            "endpoint_transport.proxy=\"http://127.0.0.1:9\"",
            "-c",
            "endpoint_transport={\"base_url\":\"https://muse.internal/v1\"}",
        ]),
        &[],
    );
    assert!(
        matches!(partial_after_proxy, LaunchEndpoint::Gap(ref why) if why.contains("proxy")),
        "partial endpoint_transport object must not erase earlier proxy: {partial_after_proxy:?}"
    );
    assert_eq!(
        launch_endpoint(
            &argv(&[
                STORE,
                "-c",
                "endpoint_transport.base_url=\"https://muse.internal/v1\"",
                "-c",
                "endpoint_transport={\"auth\":{\"type\":\"bearer\"}}",
            ]),
            &[],
        ),
        LaunchEndpoint::BaseUrl {
            url: "https://muse.internal/v1".to_string(),
            source: "Muse Code's -c endpoint_transport.base_url",
        },
        "partial endpoint_transport object must preserve earlier base_url"
    );

    // `${VAR}` resolution in `endpoint_transport.proxy` / `ca_bundle`, and `--provider` outranking `-c provider`.
    let unset_proxy = launch_endpoint(
        &argv(&[
            STORE,
            "--base-url",
            "https://muse.internal/v1",
            "-c",
            "endpoint_transport.proxy=\"${UNSET_PROXY}\"",
        ]),
        &[],
    );
    assert!(
        matches!(unset_proxy, LaunchEndpoint::Gap(ref why) if why.contains("unset environment variable")),
        "unset proxy env var must report gap: {unset_proxy:?}"
    );
    let cleared_first = launch_endpoint(
        &argv(&[STORE, "-c", "endpoint_transport.base_url=\"${MUSE_STUB}\""]),
        &[
            "MUSE_STUB=".to_string(),
            "MUSE_STUB=http://127.0.0.1:4102".to_string(),
        ],
    );
    assert!(
        matches!(cleared_first, LaunchEndpoint::Gap(ref why) if why.contains("unset environment variable")),
        "empty first env entry must shadow duplicate trailing entries like POSIX getenv: {cleared_first:?}"
    );
    assert_eq!(
        launch_endpoint(
            &argv(&[STORE, "-c", "endpoint_transport.base_url=\"${MUSE_STUB}\""]),
            &[
                "MUSE_STUB=http://127.0.0.1:4102".to_string(),
                "MUSE_STUB=http://127.0.0.1:9999".to_string(),
            ],
        ),
        LaunchEndpoint::BaseUrl {
            url: "http://127.0.0.1:4102".to_string(),
            source: "Muse Code's -c endpoint_transport.base_url",
        },
        "first matching env entry wins like POSIX getenv"
    );
    assert_eq!(
        launch_endpoint(
            &argv(&[
                STORE,
                "--provider",
                "local",
                "-c",
                "provider=\"echo\"",
                "--base-url",
                "http://127.0.0.1:18080/v1",
            ]),
            &[],
        ),
        LaunchEndpoint::BaseUrl {
            url: "http://127.0.0.1:18080/v1".to_string(),
            source: "Muse Code's --base-url",
        },
        "--provider outranks -c provider regardless of order"
    );
}

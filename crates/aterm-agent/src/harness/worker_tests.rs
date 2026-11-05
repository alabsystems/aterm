// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

use super::super::host_path::abs;

#[test]
fn native_worker_keeps_exact_custom_config_off_and_explicit_local_profile() {
    let temp = aterm_tempfile::tempdir().unwrap();
    let config = temp.path().join("custom.toml");
    aterm_uds::atomic_file::write(&config, b"[harness]\naccounts = 'off'\n", 0o600).unwrap();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let expected = config.clone();
    super::super::native_keys::with_test_launcher(
        move |provider, config| {
            assert_eq!(provider, super::super::native_keys::Provider::Google);
            assert_eq!(config, expected);
            observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Some(PathBuf::from("/checked/aterm")))
        },
        || {
            let cli = row(Program::Agy);
            let fresh = Fresh {
                config: Some(&config),
                ..Fresh::default()
            };
            let line = line_of(
                cli,
                Dialect::Zsh,
                Path::new("/checked/agy"),
                &[],
                &[],
                &[],
                &fresh,
            )
            .unwrap();
            assert!(!line.contains("native-run"));
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
            aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
            let line = line_of(
                cli,
                Dialect::Zsh,
                Path::new("/checked/agy"),
                &[],
                &[],
                &[],
                &fresh,
            )
            .unwrap();
            assert!(line.contains("native-run") && line.contains(config.to_str().unwrap()));
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
            let line = line_of(
                cli,
                Dialect::Zsh,
                Path::new("/checked/agy"),
                &["-gemini_dir=/person/profile".into()],
                &[],
                &[],
                &fresh,
            )
            .unwrap();
            assert!(!line.contains("native-run"));
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        },
    );
}

use std::path::Path;

/// [`super::line`] for `starter` taken as admitted: these tests check the line's form, and
/// [`admit`]'s own tests check admission.
fn line(
    cli: &WorkerCli,
    dialect: Dialect,
    starter: &Starter,
    fresh: &Fresh<'_>,
) -> Result<String, String> {
    super::line(cli, dialect, &Admitted::assumed(starter.clone()), fresh)
}

/// The row of `agent`, which every test here names.
fn row(agent: Program) -> &'static WorkerCli {
    worker_cli(agent).unwrap_or_else(|| panic!("no worker row for {agent:?}"))
}

/// A process's exec arguments with `argv` and `env` (`NAME=VALUE` words).
fn proc_args(argv: &[&str], env: &[&str]) -> ProcArgs {
    ProcArgs {
        exec_path: argv.first().map(|a| (*a).to_string()).unwrap_or_default(),
        argv: argv.iter().map(|a| (*a).to_string()).collect(),
        env: env.iter().map(|e| (*e).to_string()).collect(),
    }
}

/// EVERY WORKER'S FIRST LINE HANDS ITS TASK OVER AS ONE ARGUMENT, BYTE FOR
/// BYTE, IN THE CLI'S OWN FORM: each shell runs the line its row writes,
/// and the program it starts — a stand-in that writes its argv
/// NUL-separated, named as the CLI — gets the trust flag (Muse Code's, where
/// the folder is the trusted one) and its default flags (Muse Code's
/// contributor mode off at xhigh), `--model <m>`, then the task file's text
/// with its trailing newlines trimmed: Claude Code's, codex-alab's, Muse
/// Code's and Grok Build's after `--`, agy's as the attached value of
/// `--prompt-interactive=`.
#[cfg(unix)]
#[test]
fn every_workers_first_line_hands_the_task_over_as_one_argument() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-worker-line")
        .tempdir()
        .expect("scratch dir");
    let ran = tmp.path().join("ran");
    let task = format!(
        "-leading dash\nline two with 'single' and \"double\" quotes\n\
         $(touch {ran}) and `touch {ran}` stay text\n\n\n",
        ran = ran.display()
    );
    let file = tmp.path().join("task.txt");
    std::fs::write(&file, &task).expect("write the task");
    let trimmed = task.trim_end_matches('\n');
    let mut shells = vec![(Dialect::Zsh, "/bin/zsh"), (Dialect::Bash, "/bin/bash")];
    if !cfg!(target_os = "macos") {
        shells.retain(|(_, sh)| Path::new(sh).exists());
    }
    let prompt_flag = format!("--prompt-interactive={trimmed}");
    let cases: [(Program, Vec<&str>, &str); 5] = [
        (
            Program::Claude,
            vec!["--model", "m-1.5", "--", trimmed],
            "CLAUDE_CONFIG_DIR",
        ),
        (
            Program::Codex,
            vec!["--model", "m-1.5", "--", trimmed],
            "CODEX_HOME",
        ),
        (
            Program::Agy,
            vec!["--model", "m-1.5", prompt_flag.as_str()],
            "HOME",
        ),
        (
            Program::Muse,
            vec![
                "--trust-workspace",
                "--contributor-mode",
                "off",
                "--reasoning-effort",
                "xhigh",
                "--model",
                "m-1.5",
                "--",
                trimmed,
            ],
            "XDG_DATA_HOME",
        ),
        (
            Program::Grok,
            vec!["--model", "m-1.5", "--", trimmed],
            "GROK_HOME",
        ),
    ];
    for (agent, want, home_var) in cases {
        let cli = row(agent);
        let exe = tmp.path().join(cli.word);
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\000' \"$a\"; done; \
                 printf 'HOME_VAR=%s\\000' \"${home_var}\"; }} > \"$ARGV_OUT\"\n"
            ),
        )
        .expect("write the stand-in");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        aterm_spec::wait::first_exec(&exe, &[]);
        let starter = Starter {
            exe: exe.clone(),
            env: vec![(home_var.to_string(), "/homes/w".to_string())],
            args: Ok(Vec::new()),
            unset: Vec::new(),
        };
        let fresh = Fresh {
            config: None,
            model: Some("m-1.5"),
            trusted_here: true,
            permission_mode: None,
            chrome: false,
            tools: None,
            settings: None,
        };
        let line =
            line(cli, Dialect::Zsh, &starter, &fresh).unwrap_or_else(|e| panic!("{agent:?}: {e}"));
        assert!(line.ends_with(cli.operand), "{agent:?}: {line}");
        for (dialect, shell) in &shells {
            let line = self::line(cli, *dialect, &starter, &fresh)
                .unwrap_or_else(|e| panic!("{agent:?} {shell}: {e}"));
            let argv_out = tmp
                .path()
                .join(format!("argv-{}-{}", cli.word, dialect.hook_ext()));
            let run = |line: &str| {
                let _ = std::fs::remove_file(&argv_out);
                let out = aterm_uds::deadline::output_until(
                    aterm_uds::spawnfd::command(crate::test_program::kept(shell))
                        .args(["-f", "-c", line])
                        .env("ATERM_TASK_FILE", &file)
                        .env("ARGV_OUT", &argv_out),
                    std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                )
                .unwrap_or_else(|e| panic!("{shell} within the hang detector: {e}"));
                assert!(
                    out.status.success(),
                    "{shell}: {line}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                std::fs::read(&argv_out).expect("the stand-in ran")
            };
            let got = run(&line);
            let got: Vec<&[u8]> = got.split(|b| *b == 0).collect();
            let home = b"HOME_VAR=/homes/w".as_slice();
            let mut expected: Vec<&[u8]> = want.iter().map(|a| a.as_bytes()).collect();
            expected.extend([home, b"".as_slice()]);
            assert_eq!(got, expected, "{agent:?} under {shell}: {line}");
            assert!(!ran.exists(), "{shell} ran the task's text");
            let unquoted = line
                .replace(
                    r#""$(command cat -- "$ATERM_TASK_FILE")""#,
                    r#"$(command cat -- "$ATERM_TASK_FILE")"#,
                )
                .replace(
                    r#""--prompt-interactive=$(command cat -- "$ATERM_TASK_FILE")""#,
                    r#"--prompt-interactive=$(command cat -- "$ATERM_TASK_FILE")"#,
                );
            assert_ne!(unquoted, line, "{agent:?}: the control changed nothing");
            let split = run(&unquoted);
            let split = split.split(|b| *b == 0).count();
            assert!(
                split > expected.len(),
                "{agent:?} under {shell}: the unquoted control did not split ({split})"
            );
        }
        let args: Vec<String> = want.iter().map(|a| (*a).to_string()).collect();
        let process = |head: &[&str]| -> Vec<String> {
            head.iter()
                .map(|h| (*h).to_string())
                .chain(args.clone())
                .collect()
        };
        let kept = match agent {
            Program::Claude => continue,
            Program::Agy => super::super::upgrade_agy::rewrite_argv(&process(&["agy"]), None),
            Program::Muse => super::super::upgrade_muse::rewrite_argv(&process(&["muse"]), None),
            Program::Codex => super::super::codex_alab::rewrite_argv(&process(&["codex"]), None),
            // aterm has no Grok Build relaunch lane (grok-alab carries its relaunches): a Grok
            // worker's own argv, read as the starter of the next Grok worker, lends it nothing,
            // neither its task nor its model.
            Program::Grok => super::grok::config_flags(&process(&["grok"]))
                .map_err(super::super::upgrade::ArgvRefusal::UnknownFlag)
                .inspect(|kept| assert!(kept.is_empty(), "{kept:?}")),
            Program::Kimi | Program::Generic => unreachable!(),
        }
        .unwrap_or_else(|e| panic!("{agent:?}: the lane refused the worker's argv: {e:?}"));
        assert!(
            kept.iter().all(|k| !k.contains("leading dash")),
            "{agent:?}: a relaunch would send the task again: {kept:?}"
        );
        if let Some(flag) = cli.trust_flag {
            assert!(kept.iter().any(|k| k == flag), "{agent:?}: {kept:?}");
        }
    }
}

/// `model=` IS EACH CLI'S OWN `--model <m>`; MUSE CODE'S TRUST FLAG rides
/// only where the worker's shell stands in its trusted folder, and no
/// worker's line trusts every folder (`--skip-trust`).
#[test]
fn model_and_the_trust_are_each_clis_own() {
    for cli in &WORKERS {
        let plain = cli.plain();
        assert_eq!(plain.exe, Path::new(cli.word));
        let fresh = |model, trusted_here| Fresh {
            config: None,
            model,
            trusted_here,
            permission_mode: None,
            chrome: false,
            tools: None,
            settings: None,
        };
        let named = line(cli, Dialect::Zsh, &plain, &fresh(Some("big-1"), false)).expect("a line");
        assert!(named.contains(" '--model' 'big-1' "), "{named}");
        let none = line(cli, Dialect::Zsh, &plain, &fresh(None, false)).expect("a line");
        match cli.agent {
            // Muse Code's row names a default model where `model=` names none.
            Program::Muse => assert!(
                none.contains(" '--model' 'muse-wonder-full-1.0' "),
                "{none}"
            ),
            _ => assert!(!none.contains("--model"), "{none}"),
        }
        let err = line(cli, Dialect::Zsh, &plain, &fresh(Some("--yolo"), false))
            .expect_err("a model that is a flag");
        assert!(err.contains("--yolo"), "{err}");
        let err = line(cli, Dialect::Zsh, &plain, &fresh(Some("-x"), false))
            .expect_err("a model that is a short flag");
        assert!(err.contains("opens with `-`"), "{err}");
        let all = line(cli, Dialect::Zsh, &plain, &fresh(None, true)).expect("a line");
        match cli.agent {
            Program::Muse => assert!(all.contains(" '--trust-workspace' "), "{all}"),
            _ => assert_eq!(all, none, "{:?}", cli.agent),
        }
        assert!(!all.contains("--skip-trust"), "{all}");
        assert!(!none.contains("--trust-workspace"), "{none}");
    }
}

/// A PERMISSION MODE IS CLAUDE CODE'S `--permission-mode <m>` (a credit claim worker's,
/// [`super::super::credit_offers::claim_task`], which no floor moves), after the model, and only
/// a mode Claude Code takes. CONTROL: no mode named, no flag rides, so a starter no Claude Code
/// lent types the bare word with no posture marker and atpkg's twin decides its posture; a mode
/// it does not take, and any mode for a CLI whose line has no such flag, start no worker.
#[test]
fn a_permission_mode_is_claude_codes_own_flag() {
    let fresh = |mode| Fresh {
        config: None,
        model: Some("big-1"),
        permission_mode: mode,
        ..Fresh::default()
    };
    let claude = row(Program::Claude);
    for mode in CLAUDE_PERMISSION_MODE.modes {
        let typed =
            line(claude, Dialect::Zsh, &claude.plain(), &fresh(Some(mode))).expect("a line");
        assert!(
            typed.contains(&format!(" '--model' 'big-1' '--permission-mode' '{mode}' ")),
            "{typed}"
        );
    }
    let plain = line(claude, Dialect::Zsh, &claude.plain(), &fresh(None)).expect("a line");
    assert!(!plain.contains("--permission-mode"), "{plain}");
    for bad in ["manual", "Default", "--dangerously-skip-permissions", ""] {
        let err = line(claude, Dialect::Zsh, &claude.plain(), &fresh(Some(bad)))
            .expect_err("a mode Claude Code does not take");
        assert!(err.contains("none of Claude Code's"), "{bad}: {err}");
    }
    for cli in WORKERS.iter().filter(|w| w.agent != Program::Claude) {
        let err = line(cli, Dialect::Zsh, &cli.plain(), &fresh(Some("default")))
            .expect_err("a CLI with no permission-mode flag");
        assert!(
            err.contains("takes no permission mode"),
            "{:?}: {err}",
            cli.agent
        );
    }
}

/// A WORKER THAT DRIVES THE PERSON'S BROWSER IS CLAUDE CODE'S `--chrome` (a credit claim
/// worker's, [`super::super::credit_offers::claim_task`]). CONTROL: not asked, no flag rides;
/// asked of a CLI with no such flag, no worker starts.
#[test]
fn the_browser_is_claude_codes_own_flag() {
    let claude = row(Program::Claude);
    let with = Fresh {
        chrome: true,
        ..Fresh::default()
    };
    let typed = line(claude, Dialect::Zsh, &claude.plain(), &with).expect("a line");
    assert!(typed.contains(" '--chrome' "), "{typed}");
    assert_eq!(typed.matches("--chrome").count(), 1, "{typed}");
    let plain = line(claude, Dialect::Zsh, &claude.plain(), &Fresh::default()).expect("a line");
    assert!(!plain.contains("--chrome"), "{plain}");
    for cli in WORKERS.iter().filter(|w| w.agent != Program::Claude) {
        let err =
            line(cli, Dialect::Zsh, &cli.plain(), &with).expect_err("a CLI with no browser flag");
        assert!(err.contains("no browser flag"), "{:?}: {err}", cli.agent);
    }
}

/// A WORKER HANDED A SETTINGS DOCUMENT CARRIES CLAUDE CODE'S `--settings` once, before the task (a
/// credit claim worker's auto-mode rules, [`super::super::credit_offers::claim_settings`]), and a
/// line carrying it is one aterm's usage tap stands aside for (only the last `--settings` on a
/// line is read). CONTROL: not handed one, no flag rides; handed one by a CLI with no such flag,
/// or handed text that is no JSON object, no worker starts.
#[test]
fn a_settings_document_is_claude_codes_own_flag_and_the_tap_stands_aside() {
    let claude = row(Program::Claude);
    let doc = r#"{"autoMode":{"hard_deny":["Opening a billing page"]}}"#;
    let handed = |settings| Fresh {
        settings,
        ..Fresh::default()
    };
    let typed = line(claude, Dialect::Zsh, &claude.plain(), &handed(Some(doc))).expect("a line");
    assert!(
        typed.contains(&format!(" '--settings' '{doc}' ")),
        "{typed}"
    );
    assert_eq!(typed.matches("--settings").count(), 1, "{typed}");
    assert!(
        typed.find("'--settings'") < typed.find(" -- "),
        "before the task: {typed}"
    );
    let argv: Vec<String> = ["claude", "--settings", doc].map(String::from).to_vec();
    assert!(
        !super::super::tap::launch_admits(&argv),
        "the tap stands aside: {argv:?}"
    );
    let plain = line(claude, Dialect::Zsh, &claude.plain(), &Fresh::default()).expect("a line");
    assert!(!plain.contains("--settings"), "CONTROL: {plain}");
    for bad in ["", "[]", "hard_deny", "{\"a\":1"] {
        let err = line(claude, Dialect::Zsh, &claude.plain(), &handed(Some(bad)))
            .expect_err("no JSON object");
        assert!(err.contains("no JSON object"), "{bad:?}: {err}");
    }
    for cli in WORKERS.iter().filter(|w| w.agent != Program::Claude) {
        let err = line(cli, Dialect::Zsh, &cli.plain(), &handed(Some(doc)))
            .expect_err("a CLI with no settings flag");
        assert!(
            err.contains("no settings document"),
            "{:?}: {err}",
            cli.agent
        );
    }
}

/// A CLAIM WORKER'S WHOLE LINE FITS macOS's TERMINAL LINE with room to spare: Claude Code by its
/// store path under a long home, a roster login's directory, auto mode, the browser, its one tool
/// and its auto-mode rules ([`super::super::credit_offers::claim_settings`]). CONTROL: the same
/// line with an 800-byte rule does not fit, so the budget is the one this reads.
#[test]
fn a_claim_workers_line_fits_the_terminal_with_its_rules() {
    let claude = row(Program::Claude);
    let home = "/Users//a-twenty-char-user";
    let starter = Starter {
        exe: PathBuf::from(format!(
            "{home}/Library/Application Support/aterm/pkg/store/claude/1000002000001000318/bin/claude"
        )),
        env: vec![(
            super::super::accounts::HOME_ENV.to_string(),
            format!("{home}/Library/Application Support/aterm/accounts/claude/login-m9-2"),
        )],
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let settings = super::super::credit_offers::claim_settings();
    let claim = |settings| Fresh {
        permission_mode: Some("auto"),
        chrome: true,
        tools: Some(super::super::credit_offers::CLAIM_TOOLS),
        settings: Some(settings),
        ..Fresh::default()
    };
    let typed = line(claude, Dialect::Zsh, &starter, &claim(settings)).expect("a line");
    assert!(typed.len() + 150 <= 1000, "{} bytes: {typed}", typed.len());
    let long = format!(r#"{{"autoMode":{{"hard_deny":["{}"]}}}}"#, "x".repeat(800));
    let err = line(claude, Dialect::Zsh, &starter, &claim(&long)).expect_err("CONTROL: too long");
    assert!(err.contains("bytes (max"), "{err}");
}

/// A WORKER HELD TO A TOOL LIST CARRIES CLAUDE CODE'S `--tools`, comma-joined, before the task
/// (a credit claim worker's, [`super::super::credit_offers::CLAIM_TOOLS`]). CONTROL: not asked,
/// no list rides; asked of a CLI with no such flag, an empty list, or a name that is not one word
/// (a flag, a comma), no worker starts.
#[test]
fn a_tool_list_is_claude_codes_own_flag_and_one_word_a_tool() {
    let claude = row(Program::Claude);
    fn held<'a>(tools: &'a [&'a str]) -> Fresh<'a> {
        Fresh {
            tools: Some(tools),
            ..Fresh::default()
        }
    }
    let typed = line(
        claude,
        Dialect::Zsh,
        &claude.plain(),
        &held(&["ToolSearch", "Read"]),
    )
    .expect("a line");
    assert!(typed.contains(" '--tools' 'ToolSearch,Read' "), "{typed}");
    assert!(
        typed.find("'--tools'") < typed.find(" -- "),
        "before the task: {typed}"
    );
    let plain = line(claude, Dialect::Zsh, &claude.plain(), &Fresh::default()).expect("a line");
    assert!(!plain.contains("--tools"), "CONTROL: {plain}");
    let bad: [&[&str]; 4] = [&[], &["-p"], &["Read,Bash"], &[""]];
    for bad in bad {
        let err = line(claude, Dialect::Zsh, &claude.plain(), &held(bad)).expect_err("refused");
        assert!(err.contains("not one word"), "{bad:?}: {err}");
    }
    for cli in WORKERS.iter().filter(|w| w.agent != Program::Claude) {
        let err = line(cli, Dialect::Zsh, &cli.plain(), &held(&["Read"]))
            .expect_err("a CLI with no tools flag");
        assert!(err.contains("no tool list"), "{:?}: {err}", cli.agent);
    }
}

/// A STARTER'S CONFIG AND PROVIDER FLAGS RIDE ITS WORKER'S LINE, first
/// after the program: agy's `--gemini_dir` and `--app_data_dir` (its login
/// and conversations move with them), Muse Code's `--provider`, `--preset`,
/// `--base-url` and `-c`/`--config` (where its task goes), each with its
/// value, in the starter's order.
#[test]
fn a_starters_config_and_provider_flags_ride_its_workers_line() {
    let user = Path::new("/Users//u");
    let agy = row(Program::Agy);
    let argv = [
        "/opt/bin/agy",
        "--model",
        "m",
        "--gemini_dir=/Users//u/agy-work",
        "--dangerously-skip-permissions",
        "--app_data_dir",
        "alt-cli",
    ];
    let s = starter_of(
        agy,
        Some(Path::new(argv[0])),
        &proc_args(&argv, &[]),
        user,
        || None,
    )
    .expect("an agy starter");
    let args = s.args.clone().expect("agy's flags read");
    assert_eq!(
        args,
        [
            "--gemini_dir=/Users//u/agy-work",
            "--app_data_dir",
            "alt-cli",
            "--dangerously-skip-permissions"
        ]
    );
    let typed = line(agy, Dialect::Zsh, &s, &Fresh::default()).expect("a line");
    assert!(
        typed.contains(
            "'/opt/bin/agy' '--gemini_dir=/Users//u/agy-work' '--app_data_dir' 'alt-cli' "
        ),
        "{typed}"
    );
    assert!(
        typed.contains("'--dangerously-skip-permissions'"),
        "{typed}"
    );
    let muse = row(Program::Muse);
    let argv = [
        "/opt/bin/muse",
        "--yolo",
        "--provider",
        "openai",
        "--base-url=https://proxy.internal/v1",
        "--model",
        "m",
        "-c",
        "provider_retry.max_attempts=3",
        "--preset",
        "fast",
        "--",
        "--provider",
        "a prompt",
    ];
    let s = starter_of(
        muse,
        Some(Path::new(argv[0])),
        &proc_args(&argv, &[]),
        user,
        || None,
    )
    .expect("a Muse Code starter");
    assert_eq!(
        s.args.clone().expect("Muse Code's flags read"),
        [
            "--yolo",
            "--provider",
            "openai",
            "--base-url=https://proxy.internal/v1",
            "-c",
            "provider_retry.max_attempts=3",
            "--preset",
            "fast",
        ]
    );
    let unknown = ["/opt/bin/muse", "--brand-new-flag", "x", "--provider", "p"];
    let s = starter_of(
        muse,
        Some(Path::new(unknown[0])),
        &proc_args(&unknown, &[]),
        user,
        || None,
    )
    .expect("a Muse Code starter");
    let err = line(muse, Dialect::Zsh, &s, &Fresh::default()).expect_err("an unread argv");
    assert!(err.contains("--brand-new-flag"), "{err}");
    for cli in &WORKERS {
        assert_eq!(cli.plain().args, Ok(Vec::new()), "{:?}", cli.agent);
    }
}

/// THE HOMES A STARTER'S OWN AGENT CARRIES are its relaunch lane's, each
/// directory absolute or the line refused: agy's other `HOME`, Muse Code's
/// XDG homes and other `HOME`.
type HomesCase = (
    Program,
    &'static [&'static str],
    Vec<(&'static str, &'static str)>,
);

#[test]
fn a_starters_homes_are_its_lanes_and_absolute() {
    // Homes absolute as this platform reads a path: Windows roots one at a drive.
    let (other, mine) = if cfg!(windows) {
        ("C:/Users//other", "C:/Users//u")
    } else {
        ("/Users//other", "/Users//u")
    };
    let user = Path::new(mine);
    let (home_other, home_mine) = (format!("HOME={other}"), format!("HOME={mine}"));
    let env = [
        home_other.as_str(),
        "XDG_DATA_HOME=/d",
        "XDG_CONFIG_HOME=/c",
        "CLAUDE_CONFIG_DIR=/claude",
    ];
    let cases: [HomesCase; 2] = [
        (Program::Agy, &["/opt/bin/agy"], vec![("HOME", other)]),
        (
            Program::Muse,
            &["/opt/bin/muse"],
            vec![
                ("XDG_DATA_HOME", "/d"),
                ("XDG_CONFIG_HOME", "/c"),
                ("HOME", other),
            ],
        ),
    ];
    for (agent, argv, want) in cases {
        let cli = row(agent);
        let image = Path::new(argv[0]);
        let args = proc_args(argv, &env);
        let s = starter_of(cli, Some(image), &args, user, || None)
            .unwrap_or_else(|| panic!("{agent:?}: no starter"));
        let got: Vec<(&str, &str)> = s
            .env
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(got, want, "{agent:?}");
        let own = proc_args(argv, &[home_mine.as_str()]);
        let s = starter_of(cli, Some(image), &own, user, || None).expect("a starter");
        assert!(
            s.env.iter().all(|(n, _)| n != "HOME"),
            "{agent:?}: {:?}",
            s.env
        );
        for var in cli.dirs {
            let with = |value: &str| Starter {
                exe: PathBuf::from(cli.word),
                env: vec![((*var).to_string(), value.to_string())],
                args: Ok(Vec::new()),
                unset: Vec::new(),
            };
            let err = line(cli, Dialect::Bash, &with("rel/dir"), &Fresh::default())
                .expect_err("relative refused");
            assert!(err.contains(var), "{agent:?}: {err}");
            let empty = line(cli, Dialect::Bash, &with(""), &Fresh::default());
            if var.starts_with("XDG_") {
                let empty = empty.unwrap_or_else(|e| panic!("{agent:?} {var} empty: {e}"));
                assert!(empty.contains(&format!("{var}=")), "{empty}");
            } else {
                assert!(
                    empty.is_err(),
                    "{agent:?}: an empty {var} names the worker's cwd"
                );
            }
        }
    }
    let claude = row(Program::Claude);
    assert!(starter_of(claude, None, &proc_args(&["claude"], &env), user, || None).is_none());
    let rel = Starter {
        exe: PathBuf::from("claude"),
        env: vec![("CLAUDE_CONFIG_DIR".to_string(), String::new())],
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    assert!(line(claude, Dialect::Zsh, &rel, &Fresh::default()).is_err());
}

/// THE STARTER'S PROGRAM RIDES ONLY WHERE THE STARTER RUNS THE SAME CLI: an
/// agy or Muse Code image as it is, a store build as atpkg's managed twin
/// (the word where there is none).
#[test]
fn a_starters_program_rides_only_where_it_runs_the_same_cli() {
    let user = Path::new("/Users//u");
    let args = |argv: &[&str]| proc_args(argv, &[]);
    for agent in [Program::Agy, Program::Muse, Program::Grok] {
        let cli = row(agent);
        let word = cli.word;
        let own = PathBuf::from(format!("/usr/local/bin/{word}"));
        let s = starter_of(cli, Some(&own), &args(&[word]), user, || {
            panic!("a twin asked of a non-store image")
        })
        .expect("its own image");
        assert_eq!(s.exe, own);
        let store = PathBuf::from(format!("/p/store/{word}/2001004010006427002/bin/{word}"));
        let twin = PathBuf::from(format!("/p/agents/{word}"));
        let s = starter_of(cli, Some(&store), &args(&[word]), user, || {
            Some(twin.clone())
        })
        .expect("a store build");
        assert_eq!(s.exe, twin, "{agent:?}");
        let s = starter_of(cli, Some(&store), &args(&[word]), user, || None).expect("no twin");
        assert_eq!(s.exe, Path::new(word), "{agent:?}");
        let other = if agent == Program::Agy { "muse" } else { "agy" };
        assert!(
            starter_of(
                cli,
                Some(Path::new(&format!("/bin/{other}"))),
                &args(&[other]),
                user,
                || None
            )
            .is_none(),
            "{agent:?} followed {other}"
        );
        assert!(starter_of(cli, None, &args(&[word]), user, || None).is_none());
    }
}

/// A CLAUDE CODE STARTER LENDS ITS WORKER THE MODE IT RUNS IN, by argv until its screen shows
/// another, which [`a_claude_worker_never_starts_below_auto`] holds to the floor.
#[test]
fn a_claude_starters_permissions_ride_its_workers_line() {
    let cli = row(Program::Claude);
    let flags = |argv: &[&str]| {
        (cli.flags)(&argv.iter().map(|a| (*a).to_string()).collect::<Vec<_>>()).expect("read")
    };
    assert_eq!(
        flags(&[
            "claude",
            "--settings",
            "{}",
            "--dangerously-skip-permissions"
        ]),
        ["--dangerously-skip-permissions"]
    );
    assert_eq!(
        flags(&["claude", "--permission-mode", "acceptEdits"]),
        ["--permission-mode", "acceptEdits"]
    );
    assert_eq!(
        flags(&["claude", "--permission-mode=bypassPermissions"]),
        ["--dangerously-skip-permissions"]
    );
    assert_eq!(
        flags(&[
            "claude",
            "--dangerously-skip-permissions",
            "--permission-mode",
            "plan"
        ]),
        ["--dangerously-skip-permissions"],
        "plan mode says what it is doing, not what it may do"
    );
    assert!(flags(&["claude", "--permission-mode", "plan"]).is_empty());
    assert!(flags(&["claude", "--permission-mode=plan"]).is_empty());
    assert_eq!(
        flags(&[
            "claude",
            "--dangerously-skip-permissions",
            "--permission-mode",
            "acceptEdits"
        ]),
        ["--permission-mode", "acceptEdits"],
        "the last wins"
    );
    assert_eq!(
        flags(&["claude", "--", "--dangerously-skip-permissions"]),
        Vec::<String>::new(),
        "a task's words are no flag"
    );
    assert_eq!(flags(&["claude"]), Vec::<String>::new());

    // The pill read through a route that keeps the starter's mode, so no floor moves it.
    let gateway = super::super::netwatch::RouteFacts {
        env: Some(strings(&[
            "HOME=/Users//u",
            "ANTHROPIC_BASE_URL=https://gateway.invalid",
        ])),
        settings: Vec::new(),
    };
    let bypassed = strings(&["claude", "--dangerously-skip-permissions"]);
    let lent = |rows: Option<&[String]>| {
        claude_starter_args(cli, &bypassed, rows, std::slice::from_ref(&gateway)).expect("read")
    };
    assert_eq!(
        lent(Some(&pill(
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)"
        ))),
        ["--permission-mode", "auto"],
        "the pill is the mode it runs in now"
    );
    assert_eq!(
        lent(Some(&["no pill here".to_string()])),
        ["--dangerously-skip-permissions"],
        "CONTROL: a screen with no pill leaves the argv's"
    );
    assert_eq!(
        lent(None),
        ["--dangerously-skip-permissions"],
        "CONTROL: no screen read leaves the argv's"
    );
    assert_eq!(
        lent(Some(&pill("  \u{23F8} plan mode on (shift+tab to cycle)"))),
        ["--dangerously-skip-permissions"],
        "a parent planning lends what it may do, not what it is doing"
    );
    let started_bypassed = Starter {
        args: Ok(lent(None)),
        ..cli.plain()
    };
    let typed = line(cli, Dialect::Zsh, &started_bypassed, &Fresh::default()).expect("a line");
    assert!(typed.contains("--dangerously-skip-permissions"), "{typed}");
}

/// A screen whose prompt shows the permission pill `row`.
fn pill(row: &str) -> Vec<String> {
    let rule = "\u{2500}".repeat(38);
    vec![rule.clone(), "\u{276F} ".to_string(), rule, row.to_string()]
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// A CLAUDE CODE WORKER NEVER STARTS BELOW AUTO (owner, 2026-11-04: "these workers likely need to
/// be started in auto / yolo mode!", which supersedes, for this floor, the 2026-11-02 ruling that
/// a worker runs with its parent's permissions): a starter in manual or `acceptEdits`, or one
/// whose argv names no mode or plans, lends `auto`, and its worker's line types it; one in auto,
/// `dontAsk` or bypass lends its own, so none is raised to bypass. CONTROL: a starter whose
/// requests go through a gateway or a cloud provider lends its own mode, whatever it is, as every
/// starter did before the floor.
#[test]
fn a_claude_worker_never_starts_below_auto() {
    use super::super::netwatch::RouteFacts;
    let cli = row(Program::Claude);
    let via = |env: &[&str]| RouteFacts {
        env: Some(strings(env)),
        settings: Vec::new(),
    };
    let first_party = via(&["HOME=/Users//u", "HTTPS_PROXY=http://127.0.0.1:9"]);
    let lent = |argv: &[&str], facts: &RouteFacts| {
        claude_starter_args(cli, &strings(argv), None, std::slice::from_ref(facts)).expect("read")
    };
    let auto = ["--permission-mode", "auto"];
    let bypass = ["--dangerously-skip-permissions"];
    for argv in [
        &["claude", "--permission-mode", "default"][..],
        &["claude", "--permission-mode=default"],
        &["claude", "--permission-mode", "acceptEdits"],
        &["claude"],
        &["claude", "--permission-mode", "plan"],
        &["claude", "--permission-mode", "auto"],
    ] {
        assert_eq!(lent(argv, &first_party), auto, "{argv:?}");
    }
    for argv in [
        &["claude", "--dangerously-skip-permissions"][..],
        &["claude", "--permission-mode", "bypassPermissions"],
        &[
            "claude",
            "--dangerously-skip-permissions",
            "--permission-mode",
            "plan",
        ],
    ] {
        assert_eq!(lent(argv, &first_party), bypass, "{argv:?}");
    }
    assert_eq!(
        lent(&["claude", "--permission-mode", "dontAsk"], &first_party),
        ["--permission-mode", "dontAsk"]
    );
    let floored = Starter {
        args: Ok(lent(
            &["claude", "--permission-mode", "default"],
            &first_party,
        )),
        ..cli.plain()
    };
    let typed = line(cli, Dialect::Zsh, &floored, &Fresh::default()).expect("a line");
    assert!(typed.contains(" '--permission-mode' 'auto' "), "{typed}");

    for route in [
        "ANTHROPIC_BASE_URL=https://gateway.invalid",
        "CLAUDE_CODE_USE_BEDROCK=1",
        "CLAUDE_CODE_USE_VERTEX=1",
    ] {
        let routed = via(&["HOME=/Users//u", route]);
        assert_eq!(
            lent(&["claude", "--permission-mode", "default"], &routed),
            ["--permission-mode", "default"],
            "CONTROL: {route}"
        );
        assert_eq!(
            lent(&["claude", "--permission-mode", "acceptEdits"], &routed),
            ["--permission-mode", "acceptEdits"],
            "CONTROL: {route}"
        );
        assert!(lent(&["claude"], &routed).is_empty(), "CONTROL: {route}");
        assert!(
            lent(&["claude", "--permission-mode", "plan"], &routed).is_empty(),
            "CONTROL: {route}"
        );
        assert_eq!(
            lent(&["claude", "--dangerously-skip-permissions"], &routed),
            bypass,
            "{route}"
        );
        let kept = Starter {
            args: Ok(lent(&["claude", "--permission-mode", "default"], &routed)),
            ..cli.plain()
        };
        let typed = line(cli, Dialect::Zsh, &kept, &Fresh::default()).expect("a line");
        assert!(typed.contains(" '--permission-mode' 'default' "), "{typed}");
    }
}

/// THE PILL IS HELD TO THE FLOOR AS THE ARGV IS, BY THE ROUTE ALONE: where the starter's route is
/// Anthropic's own, a pill showing manual or `acceptEdits` lends `auto`, and one showing bypass,
/// `dontAsk` or auto its own, whatever the argv names. CONTROL: where the route keeps the
/// starter's mode (a gateway), the pill's mode is lent as it is, whatever the argv names, a
/// bypass argv among them (atpkg's twin starts Claude Code in bypass): a person who moved a
/// gateway starter to `acceptEdits` lends `acceptEdits`, never `auto` behind the gateway.
#[test]
fn a_claude_starters_pill_is_held_to_its_floor() {
    use super::super::netwatch::RouteFacts;
    let cli = row(Program::Claude);
    let manual = pill("  \u{23F8} manual mode on (shift+tab to cycle)");
    let edits = pill("  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle)");
    let dont_ask = pill("  \u{23F5}\u{23F5} don't ask on");
    let bypassed = pill("  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)");
    let auto = pill("  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)");
    let via = |env: &[&str]| RouteFacts {
        env: Some(strings(env)),
        settings: Vec::new(),
    };
    let first_party = via(&["HOME=/Users//u"]);
    let gateway = via(&[
        "HOME=/Users//u",
        "ANTHROPIC_BASE_URL=https://gateway.invalid",
    ]);
    let shown = |argv: &[&str], rows: &[String], route: &RouteFacts| {
        claude_starter_args(cli, &strings(argv), Some(rows), std::slice::from_ref(route))
            .expect("read")
    };
    for argv in [
        &["claude", "--permission-mode", "auto"][..],
        &["claude", "--permission-mode", "default"],
        &["claude", "--dangerously-skip-permissions"],
        &["claude"],
    ] {
        let floored = |rows: &[String]| shown(argv, rows, &first_party);
        assert_eq!(floored(&manual), ["--permission-mode", "auto"], "{argv:?}");
        assert_eq!(floored(&edits), ["--permission-mode", "auto"], "{argv:?}");
        assert_eq!(floored(&dont_ask), ["--permission-mode", "dontAsk"]);
        assert_eq!(floored(&bypassed), ["--dangerously-skip-permissions"]);
        assert_eq!(floored(&auto), ["--permission-mode", "auto"]);
        let kept = |rows: &[String]| shown(argv, rows, &gateway);
        assert_eq!(
            kept(&manual),
            ["--permission-mode", "default"],
            "CONTROL {argv:?}"
        );
        assert_eq!(
            kept(&edits),
            ["--permission-mode", "acceptEdits"],
            "CONTROL {argv:?}: a gateway starter in bypass that moved to accept edits"
        );
        assert_eq!(kept(&auto), ["--permission-mode", "auto"], "{argv:?}");
        assert_eq!(kept(&bypassed), ["--dangerously-skip-permissions"]);
    }
}

/// A ROUTE THE WORKER'S OWN FOLDER NAMES KEEPS THE STARTER'S MODE TOO: a worker spawned with
/// `cwd=` into a project whose settings set a gateway reads them where its starter's folder did
/// not, so the floor holds only where every folder's route is Anthropic's own. CONTROL: the same
/// two readings with neither folder naming a route floor the mode.
#[test]
fn a_route_the_workers_folder_names_keeps_the_starters_mode() {
    use super::super::netwatch::{RouteFacts, Source};
    let cli = row(Program::Claude);
    let at = |text: &str| RouteFacts {
        env: Some(strings(&["HOME=/Users//u"])),
        settings: vec![(
            "/work/corp/.claude/settings.json".to_string(),
            Source::Text(text.to_string()),
        )],
    };
    let quiet = at(r#"{"model":"big-1"}"#);
    let corp = at(r#"{"env":{"ANTHROPIC_BASE_URL":"https://gw.corp.invalid"}}"#);
    let argv = strings(&["claude", "--permission-mode", "default"]);
    let lent = |routes: &[RouteFacts]| claude_starter_args(cli, &argv, None, routes).expect("read");
    assert_eq!(
        lent(&[quiet.clone(), corp.clone()]),
        ["--permission-mode", "default"]
    );
    assert_eq!(
        lent(&[corp, quiet.clone()]),
        ["--permission-mode", "default"]
    );
    assert_eq!(
        lent(&[quiet.clone(), quiet.clone()]),
        ["--permission-mode", "auto"],
        "CONTROL"
    );
    assert_eq!(lent(&[quiet]), ["--permission-mode", "auto"], "CONTROL");
}

/// A STARTER'S ROUTE KEEPS ITS MODE only where its requests leave Anthropic's own API: a
/// gateway's base URL or a cloud provider's switch, set in its environment or in a settings
/// document's `env`, or a source that could not be read or parsed, which leaves the route
/// untold. CONTROL: a blank variable, a proxy (which carries requests to the same API), a
/// variable of no route, a settings document that names none and one that is not there keep
/// the floor.
#[test]
fn a_route_off_anthropics_api_keeps_the_starters_mode() {
    use super::super::netwatch::{RouteFacts, Source};
    let facts = |env: &[&str], settings: Vec<(String, Source)>| RouteFacts {
        env: Some(strings(env)),
        settings,
    };
    let kept = |facts: &RouteFacts| claude_route_kept(facts);
    assert_eq!(kept(&facts(&["HOME=/u"], Vec::new())), None);
    for (pair, key) in [
        (
            "ANTHROPIC_BASE_URL=https://gateway.invalid",
            "ANTHROPIC_BASE_URL",
        ),
        ("CLAUDE_CODE_USE_BEDROCK=1", "CLAUDE_CODE_USE_BEDROCK"),
        ("CLAUDE_CODE_USE_VERTEX=1", "CLAUDE_CODE_USE_VERTEX"),
        ("CLAUDE_CODE_USE_FOUNDRY=1", "CLAUDE_CODE_USE_FOUNDRY"),
    ] {
        let why = kept(&facts(&["HOME=/u", pair], Vec::new())).expect(pair);
        assert_eq!(why, format!("{key} in its environment"));
    }
    for pair in [
        "ANTHROPIC_BASE_URL=",
        "ANTHROPIC_BASE_URL=  ",
        "HTTPS_PROXY=http://127.0.0.1:9",
        "ALL_PROXY=socks5://127.0.0.1:9",
        "CLAUDE_CODE_USE_COWORK_PLUGINS=1",
    ] {
        assert_eq!(
            kept(&facts(&["HOME=/u", pair], Vec::new())),
            None,
            "CONTROL: {pair}"
        );
    }
    let at = "/u/.claude/settings.json".to_string();
    let source = |source: Source| vec![(at.clone(), source)];
    let text = |text: &str| source(Source::Text(text.to_string()));
    assert_eq!(
        kept(&facts(
            &["HOME=/u"],
            text(r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:4100"}}"#)
        )),
        Some(format!("ANTHROPIC_BASE_URL in {at}"))
    );
    assert_eq!(
        kept(&facts(
            &["HOME=/u"],
            text(r#"{"env":{"HTTPS_PROXY":"http://127.0.0.1:9"},"model":"big-1"}"#)
        )),
        None,
        "CONTROL: a proxy in the settings"
    );
    assert_eq!(
        kept(&facts(&["HOME=/u"], source(Source::Missing))),
        None,
        "CONTROL"
    );
    assert_eq!(
        kept(&facts(&["HOME=/u"], source(Source::Unreadable))),
        Some(format!("{at} could not be read"))
    );
    assert_eq!(
        kept(&facts(&["HOME=/u"], text("[1, 2]"))),
        Some(format!("{at} could not be parsed"))
    );
    assert_eq!(
        kept(&RouteFacts {
            env: None,
            settings: Vec::new()
        }),
        Some("its environment could not be read".to_string())
    );
}

/// A STARTER'S ROUTE IS READ WHERE CLAUDE CODE READS IT: its user settings (under its
/// `CLAUDE_CONFIG_DIR`) and its project's, beside its environment; a starter whose folder is not
/// known has project settings that cannot be found, so its route is untold. CONTROL: the same
/// documents naming no route leave it Anthropic's own.
#[test]
fn a_starters_settings_name_its_route() {
    let d = crate::supervise::ScratchDir::new("claude", "worker-route");
    let home = d.join("home");
    let config = home.join(".claude-work");
    let project = d.join("project");
    let (managed, prefs) = (d.join("managed"), d.join("prefs"));
    for dir in [&config, &project.join(".claude"), &managed, &prefs] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    let user = config.join("settings.json");
    let shared = project.join(".claude/settings.json");
    let quiet = r#"{"env":{"DISABLE_AUTOUPDATER":"1"}}"#;
    std::fs::write(&user, quiet).expect("user settings");
    std::fs::write(&shared, quiet).expect("project settings");
    let process = ProcArgs {
        argv: strings(&["claude"]),
        env: vec![
            format!("HOME={}", home.display()),
            format!("CLAUDE_CONFIG_DIR={}", config.display()),
            "USER=u".to_string(),
        ],
        ..ProcArgs::default()
    };
    let read = |cwd: Option<&Path>| {
        claude_route_kept(&claude_route_facts(
            &process,
            cwd,
            &home,
            Some(&managed),
            &prefs,
        ))
    };
    assert_eq!(read(Some(&project)), None, "CONTROL");
    std::fs::write(
        &user,
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.invalid"}}"#,
    )
    .expect("user settings");
    let why = read(Some(&project)).expect("the user's gateway");
    assert!(why.starts_with("ANTHROPIC_BASE_URL in "), "{why}");
    std::fs::write(&user, quiet).expect("user settings");
    std::fs::write(&shared, r#"{"env":{"CLAUDE_CODE_USE_BEDROCK":"1"}}"#)
        .expect("project settings");
    let why = read(Some(&project)).expect("the project's provider");
    assert!(why.starts_with("CLAUDE_CODE_USE_BEDROCK in "), "{why}");
    std::fs::write(&shared, quiet).expect("project settings");
    assert_eq!(read(Some(&project)), None, "CONTROL");
    let why = read(None).expect("no folder");
    assert!(why.contains("could not be read"), "{why}");
}

/// A CODEX STARTER IS codex-alab, its package's ALab bytes ([`super::super::codex_alab::alab_bytes`]):
/// its worker types atpkg's managed twin where that twin execs ALab bytes too, else the
/// starter's own image; it carries the starter's `CODEX_HOME` (an empty one is unset, a
/// relative one refused; with none, another `HOME`'s `.codex`) and the flags that move its
/// config, say how it reaches its provider or say what it may do unasked (its parent's permissions,
/// owner ruling 2026-11-02), never its model or folders (by flag or `-c`); a vendor Codex image of the same name, which never
/// carries itself, is no starter (the line is then the managed twin's, else the word's);
/// a starter no relaunch could resume (`exec`, `--remote`) lends no flags, and one whose
/// argv holds a flag no table knows starts no worker.
#[test]
fn a_codex_starter_is_codex_alab_and_carries_its_config() {
    let d = crate::supervise::ScratchDir::new("codex", "worker-starter");
    let (mine, other) = (abs("/Users//u"), abs("/Users//other"));
    let user = Path::new(&mine);
    // The image as atpkg lays a build's executable (`bin/codex.exe` on Windows).
    let entry = format!(
        "bin/{}",
        atpkg::store::ToolName::new("codex").unwrap().exe_file()
    );
    let package = |name: &str, alab: bool| -> PathBuf {
        let root = d.join(name);
        std::fs::create_dir_all(root.join("bin")).expect("bin");
        std::fs::write(root.join(&entry), "").expect("exe");
        std::fs::write(
            root.join(super::super::codex_alab::PACKAGE_JSON),
            format!(r#"{{"layoutVersion": 1, "version": "0.165.0", "entrypoint": "{entry}"}}"#),
        )
        .expect("manifest");
        if alab {
            std::fs::write(root.join(super::super::codex_alab::ALAB_BUILD_JSON), "{}")
                .expect("marker");
        }
        std::fs::canonicalize(root.join(&entry)).expect("the image")
    };
    let alab = package("alab", true);
    let vendor = package("vendor", false);
    let twin_of = |name: &str, exe: &Path| -> PathBuf {
        let twin = d.join(name);
        std::fs::write(&twin, super::super::codex_alab::twin_text(exe)).expect("twin");
        twin
    };
    let alab_twin = twin_of("twin-alab", &alab);
    let vendor_twin = twin_of("twin-vendor", &vendor);
    let cli = row(Program::Codex);
    let argv = [
        alab.to_str().expect("UTF-8"),
        "--dangerously-bypass-approvals-and-sandbox",
        "--yolo",
        "-m",
        "gpt-x",
        "-c",
        "model_reasoning_effort=high",
        "-c",
        "approval_policy=\"never\"",
        "--config=sandbox_mode=danger-full-access",
        "-cmodel=gpt-y",
        "--config",
        "sandbox_workspace_write.network_access=true",
        "-c",
        "default_permissions=\":danger-full-access\"",
        "-c",
        "permissions.dev.filesystem=\"write\"",
        "-c",
        "\"model\"=\"gpt-z\"",
        "-c",
        "sandbox_workspace_write . network_access = true",
        "--profile",
        "work",
        "-C",
        "/elsewhere",
        "--add-dir",
        "/x",
        "-s",
        "workspace-write",
        "--oss",
        "--",
        "-c",
        "the task",
    ];
    let env = ["CODEX_HOME=/cx", "HOME=/Users//other"];
    let s = starter_of(cli, Some(&alab), &proc_args(&argv, &env), user, || None)
        .expect("a codex-alab starter");
    assert_eq!(s.exe, alab, "no twin: its own image");
    assert_eq!(
        s.env,
        [("CODEX_HOME".to_string(), "/cx".to_string())],
        "its home, and no other variable"
    );
    let with_auth = starter_of(
        cli,
        Some(&alab),
        &proc_args(
            &argv,
            &[
                "CODEX_HOME=/cx",
                "CODEX_AUTH_PATH=/u/accounts-codex/work/auth.json",
            ],
        ),
        user,
        || None,
    )
    .expect("a codex-alab starter with auth path");
    assert_eq!(
        with_auth.env,
        [
            ("CODEX_HOME".to_string(), "/cx".to_string()),
            (
                "CODEX_AUTH_PATH".to_string(),
                "/u/accounts-codex/work/auth.json".to_string()
            ),
        ]
    );
    assert_eq!(
        s.args.clone().expect("its flags read"),
        [
            "--dangerously-bypass-approvals-and-sandbox",
            "--yolo",
            "-c",
            "model_reasoning_effort=high",
            "-c",
            "approval_policy=\"never\"",
            "--config=sandbox_mode=danger-full-access",
            "--config",
            "sandbox_workspace_write.network_access=true",
            "-c",
            "default_permissions=\":danger-full-access\"",
            "-c",
            "permissions.dev.filesystem=\"write\"",
            "-c",
            "sandbox_workspace_write . network_access = true",
            "--profile",
            "work",
            "-s",
            "workspace-write",
            "--oss"
        ]
    );
    let typed = line(cli, Dialect::Zsh, &s, &Fresh::default()).expect("a line");
    for gone in ["gpt-x", "gpt-y", "gpt-z", "/elsewhere", "/x'"] {
        assert!(!typed.contains(gone), "{gone}: {typed}");
    }
    let s = starter_of(cli, Some(&alab), &proc_args(&argv, &env), user, || {
        Some(alab_twin.clone())
    })
    .expect("a codex-alab starter");
    assert_eq!(s.exe, alab_twin, "a twin that execs ALab bytes");
    let s = starter_of(cli, Some(&alab), &proc_args(&argv, &env), user, || {
        Some(vendor_twin.clone())
    })
    .expect("a codex-alab starter");
    assert_eq!(s.exe, alab, "a twin that execs a vendor Codex is not typed");
    assert!(
        starter_of(cli, Some(&vendor), &proc_args(&argv, &env), user, || {
            Some(alab_twin.clone())
        })
        .is_none(),
        "a vendor Codex starts no worker"
    );
    let unset = starter_of(
        cli,
        Some(&alab),
        &proc_args(&argv, &["CODEX_HOME="]),
        user,
        || None,
    )
    .expect("a starter");
    assert!(unset.env.is_empty(), "{:?}", unset.env);
    let homes = |env: &[&str]| {
        starter_of(cli, Some(&alab), &proc_args(&argv, env), user, || None)
            .expect("a starter")
            .env
    };
    let others = Path::new(&other)
        .join(".codex")
        .to_str()
        .expect("UTF-8")
        .to_owned();
    assert_eq!(
        homes(&[&format!("HOME={other}")]),
        [("CODEX_HOME".to_string(), others)],
        "another home's .codex"
    );
    assert!(
        homes(&["CODEX_HOME=", &format!("HOME={mine}")]).is_empty(),
        "the user's own home"
    );
    assert!(
        homes(&["HOME=rel"]).is_empty(),
        "a relative HOME names no home"
    );
    assert_eq!(cli.unstarted_by(|| Some(alab_twin.clone())).exe, alab_twin);
    for twin in [Some(vendor_twin.clone()), None] {
        let s = cli.unstarted_by(|| twin.clone());
        assert_eq!(
            (s.exe, s.env, s.args),
            (PathBuf::from("codex"), Vec::new(), Ok(Vec::new()))
        );
    }
    assert_eq!(
        row(Program::Claude)
            .unstarted_by(|| Some(alab_twin.clone()))
            .exe,
        Path::new("claude"),
        "only codex-alab's line names a twin"
    );
    let rel = Starter {
        exe: PathBuf::from("codex"),
        env: vec![("CODEX_HOME".to_string(), "rel/home".to_string())],
        args: Ok(Vec::new()),
        unset: Vec::new(),
    };
    let err = line(cli, Dialect::Bash, &rel, &Fresh::default()).expect_err("relative refused");
    assert!(err.contains("CODEX_HOME"), "{err}");
    for (unresumable, flag) in [
        (
            vec![alab.to_str().expect("UTF-8"), "-c", "x=1", "exec", "x"],
            "exec",
        ),
        (
            vec![alab.to_str().expect("UTF-8"), "--remote", "ws://h"],
            "--remote",
        ),
    ] {
        let s = starter_of(
            cli,
            Some(&alab),
            &proc_args(&unresumable, &[]),
            user,
            || None,
        )
        .expect("a starter");
        assert_eq!(s.args, Ok(Vec::new()), "{flag}: it lends no flags");
        let typed = line(cli, Dialect::Zsh, &s, &Fresh::default()).expect("a fresh codex-alab");
        assert!(!typed.contains(flag), "{typed}");
    }
    let unknown = [alab.to_str().expect("UTF-8"), "--frobnicate"];
    let s =
        starter_of(cli, Some(&alab), &proc_args(&unknown, &[]), user, || None).expect("a starter");
    let err = line(cli, Dialect::Zsh, &s, &Fresh::default()).expect_err("an unread argv");
    assert!(err.contains("--frobnicate"), "{err}");
}

/// A MUSE CODE WORKER RUNS THE DEFAULT MODEL AT XHIGH WITH CONTRIBUTOR MODE
/// OFF. CONTROL: a named `model=` replaces the default model (and only it);
/// no other CLI's line gains a model it was not named.
#[test]
fn a_muse_worker_runs_the_default_model_with_contributor_mode_off() {
    let muse = row(Program::Muse);
    let plain = line(muse, Dialect::Zsh, &muse.plain(), &Fresh::default()).expect("a line");
    assert!(plain.contains(" '--contributor-mode' 'off' "), "{plain}");
    assert!(
        plain.contains(" '--model' 'muse-wonder-full-1.0' "),
        "{plain}"
    );
    assert!(plain.contains(" '--reasoning-effort' 'xhigh' "), "{plain}");
    assert_eq!(plain.matches("--contributor-mode").count(), 1, "{plain}");
    let named = line(
        muse,
        Dialect::Zsh,
        &muse.plain(),
        &Fresh {
            model: Some("big-1"),
            ..Fresh::default()
        },
    )
    .expect("a line");
    assert!(named.contains(" '--model' 'big-1' "), "{named}");
    assert!(!named.contains("muse-wonder-full-1.0"), "{named}");
    assert!(named.contains(" '--contributor-mode' 'off' "), "{named}");
}

#[test]
fn a_parents_empty_flags_type_the_build_the_twin_execs_never_the_twin() {
    let cli = row(Program::Claude);
    let admitted = |args: Vec<String>| Admitted {
        starter: Starter {
            args: Ok(args),
            ..cli.plain()
        },
        readiness: Readiness::Present,
    };
    let build = PathBuf::from("/store/claude/2.1.318/claude");
    let word = PathBuf::from("claude");

    let past = admitted(Vec::new()).past_the_twin_by(cli, None, || Some(build.clone()));
    assert_eq!(
        past.starter().exe,
        build,
        "no marker: the twin would add yolo"
    );

    let flags = vec!["--dangerously-skip-permissions".to_string()];
    let kept = admitted(flags).past_the_twin_by(cli, None, || Some(build.clone()));
    assert_eq!(kept.starter().exe, word, "its flags are a marker");

    let modelled = admitted(Vec::new()).past_the_twin_by(cli, Some("opus"), || Some(build.clone()));
    assert_eq!(
        modelled.starter().exe,
        word,
        "the twin runs `--model` as typed"
    );

    let no_twin = admitted(Vec::new()).past_the_twin_by(cli, None, || None);
    assert_eq!(no_twin.starter().exe, word, "no twin to pass");

    let other = row(Program::Codex);
    let codex = Admitted {
        starter: other.plain(),
        readiness: Readiness::Present,
    }
    .past_the_twin_by(other, None, || Some(build.clone()));
    assert_eq!(
        codex.starter().exe,
        PathBuf::from(other.word),
        "Claude Code's rule only"
    );
}

/// The real twin reader: the build a laid `sh` twin execs.
#[cfg(unix)]
#[test]
fn past_the_twin_reads_the_build_a_laid_twin_execs() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let agents = dir.path().join("agents");
    std::fs::create_dir_all(&agents).expect("agents");
    let store = dir.path().join("store/claude");
    let build = store.join("1000002000001000318/bin/claude");
    std::fs::create_dir_all(build.parent().expect("bin")).expect("the build's folder");
    std::fs::write(&build, "a build").expect("build");
    std::fs::write(
        store.join("1000002000001000318.shim-env"),
        "DISABLE_AUTOUPDATER=1\n",
    )
    .expect("the build's shim record");
    let body = format!("#!/bin/sh\nexec '{}' \"$@\"\n", build.display());
    std::fs::write(agents.join("claude"), body).expect("twin");
    let cli = row(Program::Claude);
    let admitted = || Admitted {
        starter: cli.plain(),
        readiness: Readiness::Present,
    };
    super::super::upgrade_drive::AGENTS_DIR.with(|d| *d.borrow_mut() = Some(agents.clone()));
    let past = admitted().past_the_twin(cli, None);
    super::super::upgrade_drive::AGENTS_DIR.with(|d| *d.borrow_mut() = None);
    assert_eq!(past.starter().exe, build);
    let line = super::line(cli, Dialect::Zsh, &past, &Fresh::default()).expect("a line");
    assert!(
        line.contains("DISABLE_AUTOUPDATER=1"),
        "past the twin, its updater stays off: {line}"
    );
}

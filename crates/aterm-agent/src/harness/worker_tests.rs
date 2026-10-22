// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

use std::path::Path;

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
/// the folder is the trusted one), `--model <m>`, Gemini CLI's
/// `--session-id <id>`, then the task file's text with its trailing
/// newlines trimmed: Claude Code's and Muse Code's after `--`, Gemini CLI's
/// and agy's as the attached value of `--prompt-interactive=`. The task
/// holds lines, both quotes, a `$(…)` and backticks the shell must not run,
/// and a leading `-`. The homes the line carries reach the program's
/// environment, and so does Gemini CLI's own trusted-folders file (no
/// other row names one). The argv each relaunch lane reads back off that
/// process drops the task (a crashed worker is never handed it again) and
/// Gemini CLI's session id (the relaunch resumes it by `--resume`), and
/// keeps the trust flag.
/// NEGATIVE CONTROL: the same line with the operand's double quotes taken
/// off splits the task into many arguments (so the one-argument check can
/// see a split).
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
    // (row, the argv its stand-in must get, the home var the line carries,
    // the argv a relaunch lane keeps of the process).
    let prompt_flag = format!("--prompt-interactive={trimmed}");
    let session = "0b5c6c33-1f7e-4a59-9d2a-6f1e2d3c4b5a";
    let trust_file = tmp.path().join("trust.json");
    let trust_file = trust_file.to_str().expect("a UTF-8 scratch path");
    let cases: [(Program, Vec<&str>, &str); 4] = [
        (
            Program::Claude,
            vec!["--model", "m-1.5", "--", trimmed],
            "CLAUDE_CONFIG_DIR",
        ),
        (
            Program::Gemini,
            vec![
                "--model",
                "m-1.5",
                "--session-id",
                session,
                prompt_flag.as_str(),
            ],
            "GEMINI_CLI_HOME",
        ),
        (
            Program::Agy,
            vec!["--model", "m-1.5", prompt_flag.as_str()],
            "HOME",
        ),
        (
            Program::Muse,
            vec!["--trust-workspace", "--model", "m-1.5", "--", trimmed],
            "XDG_DATA_HOME",
        ),
    ];
    for (agent, want, home_var) in cases {
        let cli = row(agent);
        let exe = tmp.path().join(cli.word);
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\000' \"$a\"; done; \
                 printf 'HOME_VAR=%s\\000' \"${home_var}\"; \
                 printf 'TRUST=%s\\000' \"$GEMINI_CLI_TRUSTED_FOLDERS_PATH\"; }} > \"$ARGV_OUT\"\n"
            ),
        )
        .expect("write the stand-in");
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        // The system's assessment of the stand-in's first exec, under its own
        // hang detector.
        aterm_spec::wait::first_exec(&exe, &[]);
        let starter = Starter {
            exe: exe.clone(),
            env: vec![(home_var.to_string(), "/homes/w".to_string())],
            args: Ok(Vec::new()),
        };
        let fresh = Fresh {
            model: Some("m-1.5"),
            trusted_here: true,
            trust_file: Some(Path::new(trust_file)),
            session: Some(session),
        };
        let line =
            line(cli, Dialect::Zsh, &starter, &fresh).unwrap_or_else(|e| panic!("{agent:?}: {e}"));
        assert!(line.ends_with(cli.operand), "{agent:?}: {line}");
        for (dialect, shell) in &shells {
            let line = super::line(cli, *dialect, &starter, &fresh)
                .unwrap_or_else(|e| panic!("{agent:?} {shell}: {e}"));
            let argv_out = tmp
                .path()
                .join(format!("argv-{}-{}", cli.word, dialect.hook_ext()));
            let run = |line: &str| {
                let _ = std::fs::remove_file(&argv_out);
                let out = aterm_uds::deadline::output_until(
                    aterm_uds::spawnfd::command(shell)
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
            let trust = if agent == Program::Gemini {
                format!("TRUST={trust_file}")
            } else {
                "TRUST=".to_string()
            };
            let mut expected: Vec<&[u8]> = want.iter().map(|a| a.as_bytes()).collect();
            expected.extend([home, trust.as_bytes(), b"".as_slice()]);
            assert_eq!(got, expected, "{agent:?} under {shell}: {line}");
            assert!(!ran.exists(), "{shell} ran the task's text");
            // The control: unquoted, the substitution splits.
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
        // What each lane carries into a relaunch of that very process: the
        // task never; the trust flag where its CLI keeps it.
        let args: Vec<String> = want.iter().map(|a| (*a).to_string()).collect();
        let process = |head: &[&str]| -> Vec<String> {
            head.iter()
                .map(|h| (*h).to_string())
                .chain(args.clone())
                .collect()
        };
        let kept = match agent {
            Program::Claude => continue,
            Program::Gemini => super::super::upgrade_gemini::rewrite_argv(
                &process(&["node", "/opt/homebrew/bin/gemini"]),
                None,
            ),
            Program::Agy => super::super::upgrade_agy::rewrite_argv(&process(&["agy"]), None),
            Program::Muse => super::super::upgrade_muse::rewrite_argv(&process(&["muse"]), None),
            Program::Codex | Program::Generic => unreachable!(),
        }
        .unwrap_or_else(|e| panic!("{agent:?}: the lane refused the worker's argv: {e:?}"));
        assert!(
            kept.iter().all(|k| !k.contains("leading dash")),
            "{agent:?}: a relaunch would send the task again: {kept:?}"
        );
        assert!(
            kept.iter().all(|k| k != session && k != "--session-id"),
            "{agent:?}: a relaunch would start the worker's session afresh: {kept:?}"
        );
        if let Some(flag) = cli.trust_flag {
            assert!(kept.iter().any(|k| k == flag), "{agent:?}: {kept:?}");
        }
    }
}

/// `model=` IS EACH CLI'S OWN `--model <m>`; MUSE CODE'S TRUST FLAG rides
/// only where the worker's shell stands in its trusted folder; GEMINI CLI'S
/// FOLDER IS TRUSTED BY A FILE OF ITS OWN, never `--skip-trust` (which
/// trusts every folder for every Gemini CLI the worker's tools start), and
/// its session is the line's own (`--session-id`). Claude Code and agy carry
/// none of these (their dialog is the window harness's rule's). A model
/// opening with `-` is refused: Gemini CLI's yargs and Muse Code's clap read
/// it as a flag and exit.
/// NEGATIVE CONTROLS: no model named, no `--model`; a folder that is not the
/// trusted one, no trust flag; no file or id handed over, none named; a
/// model with a `-` inside it is a model.
#[test]
fn model_and_the_trust_are_each_clis_own() {
    let file = Path::new("/state/tasks/s-1-1.trust.json");
    let id = "0b5c6c33-1f7e-4a59-9d2a-6f1e2d3c4b5a";
    for cli in &WORKERS {
        let plain = cli.plain();
        assert_eq!(plain.exe, Path::new(cli.word));
        let fresh = |model, trusted_here, trust_file, session| Fresh {
            model,
            trusted_here,
            trust_file,
            session,
        };
        let named = line(
            cli,
            Dialect::Zsh,
            &plain,
            &fresh(Some("big-1"), false, None, None),
        )
        .expect("a line");
        assert!(named.contains(" '--model' 'big-1' "), "{named}");
        let none =
            line(cli, Dialect::Zsh, &plain, &fresh(None, false, None, None)).expect("a line");
        assert!(!none.contains("--model"), "{none}");
        let err = line(
            cli,
            Dialect::Zsh,
            &plain,
            &fresh(Some("--yolo"), false, None, None),
        )
        .expect_err("a model that is a flag");
        assert!(err.contains("--yolo"), "{err}");
        let err = line(
            cli,
            Dialect::Zsh,
            &plain,
            &fresh(Some("-x"), false, None, None),
        )
        .expect_err("a model that is a short flag");
        assert!(err.contains("opens with `-`"), "{err}");
        let all = line(
            cli,
            Dialect::Zsh,
            &plain,
            &fresh(None, true, Some(file), Some(id)),
        )
        .expect("a line");
        match cli.agent {
            Program::Gemini => {
                assert!(
                    all.contains("'GEMINI_CLI_TRUSTED_FOLDERS_PATH=/state/tasks/s-1-1.trust.json'")
                        || all.contains(
                            "GEMINI_CLI_TRUSTED_FOLDERS_PATH=/state/tasks/s-1-1.trust.json "
                        ),
                    "{all}"
                );
                assert!(all.contains(&format!(" '--session-id' '{id}' ")), "{all}");
            }
            Program::Muse => assert!(all.contains(" '--trust-workspace' "), "{all}"),
            _ => assert_eq!(all, none, "{:?}", cli.agent),
        }
        assert!(!all.contains("--skip-trust"), "{all}");
        for flag in [
            "--trust-workspace",
            "--session-id",
            "GEMINI_CLI_TRUSTED_FOLDERS_PATH",
        ] {
            assert!(!none.contains(flag), "{none}");
        }
    }
}

/// A WORKER'S TRUSTED-FOLDERS FILE TRUSTS ITS ONE FOLDER and no other:
/// Gemini CLI's `trustedFolders.json` form, the folder spelled as given
/// whatever it holds (quotes, a backslash, a space), `TRUST_FOLDER`.
/// NEGATIVE CONTROL: the folder's parent is no key of it.
#[test]
fn a_workers_trusted_folders_file_names_its_folder_alone() {
    let folder = r#"/w/a "quoted" \ proj"#;
    let text = trusted_folders_text(folder);
    let value: aterm_json::Value = aterm_json::from_str(&text).expect("JSON");
    let object = value.as_object().expect("an object");
    assert_eq!(object.len(), 1, "{text}");
    assert_eq!(
        object.get(folder).and_then(aterm_json::Value::as_str),
        Some("TRUST_FOLDER"),
        "{text}"
    );
    assert!(object.get("/w").is_none(), "{text}");
}

/// A NEW SESSION'S ID IS A VERSION-4 UUID the Gemini CLI lane types back
/// (`upgrade_gemini::is_session_uuid`), whatever the random bytes: all zero
/// and all ones each get the version and variant bits.
/// NEGATIVE CONTROL: the bytes as given, unmasked, are no version-4 id.
#[test]
fn a_new_session_id_is_a_version_four_uuid() {
    for bytes in [[0u8; 16], [0xff; 16], *b"0123456789abcdef"] {
        let id = session_id_of(bytes);
        assert!(super::super::upgrade_gemini::is_session_uuid(&id), "{id}");
        assert_eq!(id.as_bytes()[14], b'4', "{id}");
        assert!(
            matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "{id}"
        );
    }
    let raw = aterm_codec::hex::encode_lower(&[0xffu8; 16]);
    assert_ne!(&raw[12..13], "4", "the control: unmasked bytes");
    let one = fresh_session_id().expect("the random source");
    assert!(super::super::upgrade_gemini::is_session_uuid(&one), "{one}");
}

/// A STARTER'S CONFIG AND PROVIDER FLAGS RIDE ITS WORKER'S LINE, first
/// after the program: agy's `--gemini_dir` and `--app_data_dir` (its login
/// and conversations move with them), Muse Code's `--provider`, `--preset`,
/// `--base-url` and `-c`/`--config` (where its task goes), each with its
/// value, in the starter's order. A Muse Code argv holding a flag no table
/// knows refuses the line rather than start the worker on a default
/// provider.
/// NEGATIVE CONTROLS: the starter's other flags (`--model`, `--yolo`, its
/// first prompt) stay behind; Gemini CLI's and Claude Code's rows carry
/// none; without a starter none ride ([`WorkerCli::plain`]).
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
            "alt-cli"
        ]
    );
    let typed = line(agy, Dialect::Zsh, &s, &Fresh::default()).expect("a line");
    assert!(
        typed.contains(
            "'/opt/bin/agy' '--gemini_dir=/Users//u/agy-work' '--app_data_dir' 'alt-cli' "
        ),
        "{typed}"
    );
    assert!(!typed.contains("dangerously"), "{typed}");
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
    // NEGATIVE CONTROLS.
    let gemini = row(Program::Gemini);
    let s = starter_of(
        gemini,
        Some(Path::new("/opt/homebrew/bin/node")),
        &proc_args(
            &["node", "/opt/homebrew/bin/gemini", "--model", "m", "-y"],
            &[],
        ),
        user,
        || None,
    )
    .expect("a Gemini CLI starter");
    assert_eq!(s.args, Ok(Vec::new()));
    for cli in &WORKERS {
        assert_eq!(cli.plain().args, Ok(Vec::new()), "{:?}", cli.agent);
    }
}

/// THE HOMES A STARTER'S OWN AGENT CARRIES are its relaunch lane's, each
/// directory absolute or the line refused: Gemini CLI's `GEMINI_CLI_HOME`
/// and another `HOME`, agy's other `HOME`, Muse Code's XDG homes and other
/// `HOME`. NEGATIVE CONTROLS: the user's own `HOME` and another CLI's
/// variables are not carried; a relative home refuses the line rather than
/// name another folder under the worker's `cwd=`.
/// One starter case: the agent, its process's argv, and the homes its line
/// carries.
type HomesCase = (
    Program,
    &'static [&'static str],
    Vec<(&'static str, &'static str)>,
);

#[test]
fn a_starters_homes_are_its_lanes_and_absolute() {
    let user = Path::new("/Users//u");
    let env = [
        "HOME=/Users//other",
        "GEMINI_CLI_HOME=/g",
        "XDG_DATA_HOME=/d",
        "XDG_CONFIG_HOME=/c",
        "CLAUDE_CONFIG_DIR=/claude",
    ];
    let cases: [HomesCase; 3] = [
        (
            Program::Gemini,
            &["node", "/opt/homebrew/bin/gemini"],
            vec![("HOME", "/Users//other"), ("GEMINI_CLI_HOME", "/g")],
        ),
        (
            Program::Agy,
            &["/opt/bin/agy"],
            vec![("HOME", "/Users//other")],
        ),
        (
            Program::Muse,
            &["/opt/bin/muse"],
            vec![
                ("XDG_DATA_HOME", "/d"),
                ("XDG_CONFIG_HOME", "/c"),
                ("HOME", "/Users//other"),
            ],
        ),
    ];
    for (agent, argv, want) in cases {
        let cli = row(agent);
        let image = if agent == Program::Gemini {
            Path::new("/opt/homebrew/bin/node")
        } else {
            Path::new(argv[0])
        };
        let args = proc_args(argv, &env);
        let s = starter_of(cli, Some(image), &args, user, || None)
            .unwrap_or_else(|| panic!("{agent:?}: no starter"));
        let got: Vec<(&str, &str)> = s
            .env
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        assert_eq!(got, want, "{agent:?}");
        // Under the user's own HOME, no HOME rides.
        let own = proc_args(argv, &["HOME=/Users//u"]);
        let s = starter_of(cli, Some(image), &own, user, || None).expect("a starter");
        assert!(
            s.env.iter().all(|(n, _)| n != "HOME"),
            "{agent:?}: {:?}",
            s.env
        );
        // A relative directory refuses the line; an empty XDG base
        // directory is unset (the XDG rule), and rides as the starter had it.
        for var in cli.dirs {
            let with = |value: &str| Starter {
                exe: PathBuf::from(cli.word),
                env: vec![((*var).to_string(), value.to_string())],
                args: Ok(Vec::new()),
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
    // A starter Gemini CLI's own trusted-folders file (its own spawn's) is
    // its relaunch's to carry, never a worker's of it.
    let gemini = row(Program::Gemini);
    let args = proc_args(
        &["node", "/opt/homebrew/bin/gemini"],
        &["GEMINI_CLI_TRUSTED_FOLDERS_PATH=/state/tasks/s-9-1.trust.json"],
    );
    let lane = super::super::upgrade_gemini::relaunch_env(|n| args.env_var(n), user);
    assert_eq!(
        lane,
        [(
            "GEMINI_CLI_TRUSTED_FOLDERS_PATH".to_string(),
            "/state/tasks/s-9-1.trust.json".to_string()
        )],
        "the relaunch keeps the worker trusted"
    );
    let s = starter_of(
        gemini,
        Some(Path::new("/opt/homebrew/bin/node")),
        &args,
        user,
        || None,
    )
    .expect("a Gemini CLI starter");
    assert!(s.env.is_empty(), "{:?}", s.env);
    // Claude Code's row keeps its own reader and its own refusal.
    let claude = row(Program::Claude);
    assert!(starter_of(claude, None, &proc_args(&["claude"], &env), user, || None).is_none());
    let rel = Starter {
        exe: PathBuf::from("claude"),
        env: vec![("CLAUDE_CONFIG_DIR".to_string(), String::new())],
        args: Ok(Vec::new()),
    };
    assert!(line(claude, Dialect::Zsh, &rel, &Fresh::default()).is_err());
}

/// THE STARTER'S PROGRAM RIDES ONLY WHERE THE STARTER RUNS THE SAME CLI: an
/// agy or Muse Code image as it is, a store build as atpkg's managed twin
/// (the word where there is none), Gemini CLI by its PATH word whatever node
/// runs it. NEGATIVE CONTROLS: another CLI's image, a `node` running another
/// script and a twin never asked of a non-store image are no starter of the
/// row's.
#[test]
fn a_starters_program_rides_only_where_it_runs_the_same_cli() {
    let user = Path::new("/Users//u");
    let args = |argv: &[&str]| proc_args(argv, &[]);
    for agent in [Program::Agy, Program::Muse] {
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
    let gemini = row(Program::Gemini);
    let node = Path::new("/opt/homebrew/bin/node");
    let s = starter_of(
        gemini,
        Some(node),
        &args(&["node", "/opt/homebrew/Cellar/gemini-cli/0.46.0/bin/gemini"]),
        user,
        || None,
    )
    .expect("a Gemini CLI");
    assert_eq!(s.exe, Path::new("gemini"));
    assert!(
        starter_of(
            gemini,
            Some(node),
            &args(&["node", "/srv/app.js"]),
            user,
            || None
        )
        .is_none()
    );
    assert!(
        starter_of(
            gemini,
            Some(Path::new("/bin/agy")),
            &args(&["agy"]),
            user,
            || None
        )
        .is_none()
    );
}

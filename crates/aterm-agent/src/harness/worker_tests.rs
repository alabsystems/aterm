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
/// newlines trimmed: Claude Code's, codex-alab's and Muse Code's after `--`,
/// Gemini CLI's and agy's as the attached value of `--prompt-interactive=`.
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
    let session = "0b5c6c33-1f7e-4a59-9d2a-6f1e2d3c4b5a";
    let trust_file = tmp.path().join("trust.json");
    let trust_file = trust_file.to_str().expect("a UTF-8 scratch path");
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
            Program::Gemini => super::super::upgrade_gemini::rewrite_argv(
                &process(&["node", "/opt/homebrew/bin/gemini"]),
                None,
            ),
            Program::Agy => super::super::upgrade_agy::rewrite_argv(&process(&["agy"]), None),
            Program::Muse => super::super::upgrade_muse::rewrite_argv(&process(&["muse"]), None),
            Program::Codex => super::super::codex_alab::rewrite_argv(&process(&["codex"]), None),
            Program::Generic => unreachable!(),
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
/// its session is the line's own (`--session-id`).
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
/// `HOME`.
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
        "GEMINI_CLI_HOME=/g",
        "XDG_DATA_HOME=/d",
        "XDG_CONFIG_HOME=/c",
        "CLAUDE_CONFIG_DIR=/claude",
    ];
    let cases: [HomesCase; 3] = [
        (
            Program::Gemini,
            &["node", "/opt/homebrew/bin/gemini"],
            vec![("HOME", other), ("GEMINI_CLI_HOME", "/g")],
        ),
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
/// runs it.
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

/// A CODEX STARTER IS codex-alab, its package's ALab bytes ([`super::super::codex_alab::alab_bytes`]):
/// its worker types atpkg's managed twin where that twin execs ALab bytes too, else the
/// starter's own image; it carries the starter's `CODEX_HOME` (an empty one is unset, a
/// relative one refused; with none, another `HOME`'s `.codex`) and the flags that move its
/// config or say how it reaches its provider, never its model, folders or approval posture
/// (by flag, hidden alias or `-c`); a vendor Codex image of the same name, which never
/// carries itself, is no starter (the line is then the managed twin's, else the word's);
/// a starter no relaunch could resume (`exec`, `--remote`) lends no flags, and one whose
/// argv holds a flag no table knows starts no worker.
#[test]
fn a_codex_starter_is_codex_alab_and_carries_its_config() {
    let d = crate::supervise::ScratchDir::new("codex", "worker-starter");
    let user = Path::new("/Users//u");
    let package = |name: &str, alab: bool| -> PathBuf {
        let root = d.join(name);
        std::fs::create_dir_all(root.join("bin")).expect("bin");
        std::fs::write(root.join("bin/codex"), "").expect("exe");
        std::fs::write(
            root.join(super::super::codex_alab::PACKAGE_JSON),
            r#"{"layoutVersion": 1, "version": "0.165.0", "entrypoint": "bin/codex"}"#,
        )
        .expect("manifest");
        if alab {
            std::fs::write(root.join(super::super::codex_alab::ALAB_BUILD_JSON), "{}")
                .expect("marker");
        }
        std::fs::canonicalize(root.join("bin/codex")).expect("the image")
    };
    let alab = package("alab", true);
    let vendor = package("vendor", false);
    let twin_of = |name: &str, exe: &Path| -> PathBuf {
        let twin = d.join(name);
        std::fs::write(
            &twin,
            format!("#!/bin/sh\nexec '{}' \"$@\"\n", exe.display()),
        )
        .expect("twin");
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
    assert_eq!(
        s.args.clone().expect("its flags read"),
        [
            "-c",
            "model_reasoning_effort=high",
            "--profile",
            "work",
            "--oss"
        ]
    );
    let typed = line(cli, Dialect::Zsh, &s, &Fresh::default()).expect("a line");
    for gone in [
        "dangerously",
        "yolo",
        "gpt-x",
        "gpt-y",
        "approval_policy",
        "sandbox_mode",
        "network_access",
        "permissions",
        "/elsewhere",
        "/x'",
        "workspace-write",
    ] {
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
    assert_eq!(
        homes(&["HOME=/Users//other"]),
        [("CODEX_HOME".to_string(), "/Users//other/.codex".to_string())],
        "another home's .codex"
    );
    assert!(
        homes(&["CODEX_HOME=", "HOME=/Users//u"]).is_empty(),
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

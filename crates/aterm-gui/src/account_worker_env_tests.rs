// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::accounts::Cli;
use aterm_agent::harness::accounts_pool::LoginEnv;
use aterm_agent::harness::worker::Starter;

#[path = "account_worker_env_model_tests.rs"]
mod model;

#[cfg(unix)]
#[test]
fn worker_command_replaces_the_shells_previous_login() {
    use aterm_agent::harness::worker::{self, Fresh};
    let root = aterm_tempfile::tempdir().unwrap();
    let task = root.path().join("task");
    std::fs::write(&task, "task fixture").unwrap();
    for own in [false, true] {
        let pick = LoginEnv {
            label: "main".into(),
            var: "CODEX_AUTH_PATH",
            value: own.then(|| "/chosen/auth.json".into()),
        };
        let onto = pool_onto(
            Starter {
                // The real command writer launches a shell stand-in that reports only
                // the variable being tested, never this process's environment.
                exe: PathBuf::from("/bin/sh"),
                env: vec![(pick.var.into(), "/previous/auth.json".into())],
                args: Ok(vec![
                    "-c".into(),
                    "printf '%s' \"${CODEX_AUTH_PATH-unset}\"".into(),
                ]),
                unset: Vec::new(),
            },
            Cli::Codex,
            &pick,
            root.path(),
        )
        .unwrap();
        let cli = worker::worker_cli(Program::Codex).unwrap();
        let admitted =
            worker::admit(cli, onto, Some(root.path()), |_| true, |_| None, |_| Ok(())).unwrap();
        let line = worker::line(cli, Dialect::Bash, &admitted, &Fresh::default()).unwrap();
        let mut shell = aterm_uds::spawnfd::command("/bin/sh");
        shell
            .args(["-c", &line])
            .env("ATERM_TASK_FILE", &task)
            .env(pick.var, "/ambient/auth.json");
        let result =
            aterm_uds::deadline::output_within(&mut shell, Duration::from_secs(60)).unwrap();
        assert!(result.status.success(), "{:?}", result.stderr);
        assert_eq!(
            result.stdout,
            if own {
                b"/chosen/auth.json".as_slice()
            } else {
                b"unset".as_slice()
            }
        );
    }
}

#[test]
fn pooled_worker_removes_values_that_its_command_unsets() {
    let pick = LoginEnv {
        label: "work".into(),
        var: "GROK_AUTH_PATH",
        value: Some("/work/auth.json".into()),
    };
    let onto = pool_onto(
        Starter {
            exe: PathBuf::from("/opt/grok"),
            env: vec![
                ("GROK_AUTH".into(), "inline fixture".into()),
                ("GROK_LEADER_SOCKET".into(), "/old/socket".into()),
                (pick.var.into(), "/old/auth.json".into()),
                ("KEEP_ME".into(), "kept".into()),
            ],
            args: Ok(Vec::new()),
            unset: Vec::new(),
        },
        Cli::Grok,
        &pick,
        Path::new("/home/fixture"),
    )
    .unwrap();
    let env: Vec<_> = onto
        .env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    assert!(
        aterm_agent::harness::upgrade::task_line_env_clear(
            Dialect::Bash,
            &env,
            &onto.unset,
            &onto.exe,
            onto.args.as_ref().unwrap(),
            ""
        )
        .is_ok()
    );
    assert_eq!(
        onto.env,
        [
            ("KEEP_ME".into(), "kept".into()),
            (pick.var.into(), "/work/auth.json".into())
        ]
    );
}

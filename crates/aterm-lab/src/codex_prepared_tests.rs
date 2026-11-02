// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A prepared Codex child has its own jail while its preparation host keeps ownership.

use super::*;
use std::os::unix::fs::PermissionsExt as _;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

struct Fixture {
    root: aterm_tempfile::TempDir,
    run: Run,
}

#[derive(Debug, PartialEq, Eq)]
struct Host {
    record: Run,
    saved: Vec<u8>,
    settings: Vec<u8>,
    launcher: Vec<u8>,
    socket: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix("aterm-codex-prepared-")
            .tempdir()
            .unwrap();
        let base = fs::canonicalize(root.path()).unwrap();
        let run_dir = base.join("run with spaces");
        let w = run_dir.join("scratch");
        let runtime = base.join("runtime");
        let project = base.join("project ; literal");
        for path in [
            w.join("home"),
            w.join("cfg/aterm"),
            w.join("codex-home"),
            w.join("tmp"),
            runtime.clone(),
            project.clone(),
            run_dir.join("shim"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        let binary = run_dir.join("fake codex");
        fs::write(&binary, "#!/bin/sh\nprintf 'fake Codex\\n'\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        let run = Run {
            tag: "prepared-test".into(),
            run: run_dir,
            w,
            sockdir: runtime.to_string_lossy().into_owned(),
            sock: base.join("not-a-socket").to_string_lossy().into_owned(),
            proj: project.to_string_lossy().into_owned(),
            port: "48991".into(),
            proxy: "http://127.0.0.1:48992".into(),
            instance_pid: 42,
            stub_pid: 43,
            agent: "claude".into(),
            codex_mode: "signed-out".into(),
            codex_bin: binary.to_string_lossy().into_owned(),
            codex_ver: "0.166.0".into(),
            ..Default::default()
        };
        fs::write(&run.sock, "a regular file, never a control socket").unwrap();
        fs::write(
            run.w.join("cfg/aterm/aterm.toml"),
            "[harness]\nenabled = true\n",
        )
        .unwrap();
        fs::write(run.w.join("codex-home/config.toml"), "original config\n").unwrap();
        fs::write(run.w.join("codex-home/auth.json"), "original auth\n").unwrap();
        fs::write(run.run.join("codex.sh"), "normal launcher, untouched\n").unwrap();
        run.save();
        Self { root, run }
    }

    fn host(&self) -> Host {
        Host {
            record: self.run.clone(),
            saved: fs::read(self.run.run.join("env.sh")).unwrap(),
            settings: fs::read(self.run.w.join("cfg/aterm/aterm.toml")).unwrap(),
            launcher: fs::read(self.run.run.join("codex.sh")).unwrap(),
            socket: fs::read(&self.run.sock).unwrap(),
        }
    }

    fn no_preparation_written(&self, before: &Host) {
        assert_eq!(&self.host(), before, "the preparation host is unchanged");
        assert_eq!(
            fs::read_to_string(self.run.w.join("codex-home/config.toml")).unwrap(),
            "original config\n"
        );
        assert_eq!(
            fs::read_to_string(self.run.w.join("codex-home/auth.json")).unwrap(),
            "original auth\n"
        );
        assert!(!self.run.run.join("codex-prepared.sh").exists());
    }
}

fn selected(options: &[&str]) -> Launch {
    parse_launch(
        &options
            .iter()
            .map(|word| word.to_string())
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn preparing_codex_needs_no_input_socket_and_preserves_saved_host_ownership() {
    for (options, mode, plan_name) in [
        (vec!["--prepare-only"], "lab", None),
        (
            vec!["--prepare-only", "--chatgpt=pro"],
            "chatgpt",
            Some("pro"),
        ),
        (vec!["--prepare-only", "--signed-out"], "signed-out", None),
    ] {
        let fixture = Fixture::new();
        let before = fixture.host();
        let launch = selected(&options);
        let called = std::cell::Cell::new(false);
        let plan = prepare_with(&fixture.run, &launch, "lab-user", |run| {
            assert_eq!(run, &fixture.run);
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(called.get());
        assert_eq!(fixture.host(), before);
        assert_eq!(plan.cwd, Path::new(&fixture.run.proj));
        assert_eq!(plan.launcher, fixture.run.run.join("codex-prepared.sh"));
        assert_eq!(
            fs::read_to_string(&plan.launcher).unwrap(),
            launcher(&fixture.run, true, mode, &[], &[])
        );
        let config: aterm_toml::Value = aterm_toml::from_str(
            &fs::read_to_string(fixture.run.w.join("codex-home/config.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            config
                .get("model_provider")
                .and_then(|value| value.as_str()),
            if mode == "signed-out" {
                None
            } else {
                Some("lab")
            }
        );
        let auth = fixture.run.w.join("codex-home/auth.json");
        if let Some(plan_name) = plan_name {
            let auth = J::parse(&fs::read_to_string(auth).unwrap()).unwrap();
            assert_eq!(auth.get("auth_mode").and_then(J::as_str), Some("chatgpt"));
            assert!(auth.dump(COMPACT).contains("lab-refresh-A"));
            assert_eq!(launch.plan, plan_name);
        } else {
            assert!(!auth.exists());
        }
        let json = plan.json(&fixture.run.tag);
        assert_eq!(json.get("schema").and_then(J::as_i64), Some(1));
        assert_eq!(json.get("kind").and_then(J::as_str), Some("codex-prepared"));
        assert_eq!(json.get("requires_preparation_host"), Some(&J::Bool(true)));
        assert_eq!(
            json.get("cwd").and_then(J::as_str),
            Some(fixture.run.proj.as_str())
        );
        assert_eq!(
            json.get("argv")
                .and_then(J::as_arr)
                .unwrap()
                .iter()
                .map(|word| word.as_str().unwrap())
                .collect::<Vec<_>>(),
            plan.argv.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }
}

#[test]
fn invalid_prepared_codex_arguments_auth_and_environment_write_nothing() {
    let mut fixture = Fixture::new();
    let env = |value: &str| Launch {
        envs: vec![value.into()],
        ..selected(&["--prepare-only"])
    };
    let mut invalid = vec![
        Launch {
            mode: "other".into(),
            ..selected(&["--prepare-only"])
        },
        Launch {
            plan: "not a plan".into(),
            ..selected(&["--prepare-only", "--chatgpt"])
        },
        Launch {
            args: vec!["login".into(), "status".into()],
            ..selected(&["--prepare-only"])
        },
        Launch {
            args: vec!["argument\0nul".into()],
            ..selected(&["--prepare-only"])
        },
        env("MISSING_EQUALS"),
        env("BAD-NAME=value"),
        env("EXTRA=contains\0nul"),
    ];
    for name in [
        "HOME",
        "CODEX_HOME",
        "TMPDIR",
        "TMP",
        "TEMP",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "ATERM_STATE_HOME",
        "TERM",
        "COLORTERM",
        "TERM_PROGRAM",
        "PATH",
        "BROWSER",
        "LAB_SECURITY_LOG",
        "LAB_OPEN_LOG",
        "LAB_CODEX_KEY",
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "OPENAI_BASE_URL",
        "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
        "CODEX_REVOKE_TOKEN_URL_OVERRIDE",
        "NO_PROXY",
        "no_proxy",
        "DYLD_INSERT_LIBRARIES",
        "LABCLOCK_FILE",
    ] {
        invalid.push(env(&format!("{name}=other")));
    }
    for name in crate::lab::PROXY_KEYS {
        invalid.push(env(&format!("{name}=other")));
    }
    for launch in invalid {
        let before = fixture.host();
        let error = prepare_with(&fixture.run, &launch, "lab-user", |_| {
            panic!("invalid launch reached its witness")
        })
        .err()
        .expect("invalid launch refused");
        assert_eq!(error.code, 2);
        fixture.no_preparation_written(&before);
    }
    for recorded in ["HOME=/different", "TMPDIR=/outside-the-lab"] {
        fixture.run.instance_env = recorded.into();
        fixture.run.save();
        let before = fixture.host();
        assert!(
            prepare_with(
                &fixture.run,
                &selected(&["--prepare-only"]),
                "lab-user",
                |_| panic!("invalid roots reached witness")
            )
            .is_err()
        );
        fixture.no_preparation_written(&before);
    }
    fixture.run.instance_env.clear();
    fixture.run.save();
    let before = fixture.host();
    assert!(
        prepare_with(
            &fixture.run,
            &selected(&["--prepare-only"]),
            "lab-user",
            crate::procs::need_witness
        )
        .is_err()
    );
    fixture.no_preparation_written(&before);
    assert_eq!(
        launch(&fixture.run, &selected(&["--prepare-only"]))
            .unwrap_err()
            .code,
        2
    );
    fixture.no_preparation_written(&before);
}

#[test]
fn prepared_auth_never_follows_a_linked_codex_home() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let home = fixture.run.w.join("codex-home");
    let protected = fixture.root.path().join("synthetic-protected-home");
    fs::rename(&home, &protected).unwrap();
    symlink(&protected, &home).unwrap();
    let before = fixture.host();
    assert!(
        prepare_with(
            &fixture.run,
            &selected(&["--prepare-only", "--signed-out"]),
            "lab-user",
            |_| Ok(())
        )
        .is_err()
    );
    fixture.no_preparation_written(&before);
    assert_eq!(
        fs::read_to_string(protected.join("auth.json")).unwrap(),
        "original auth\n"
    );
}

/// A synthetic denied file proves this explicit child keeps the CLI jail, not the GUI host.
#[cfg(target_os = "macos")]
#[test]
fn prepared_codex_argv_runs_literal_program_with_private_env_cwd_and_jail() {
    let fixture = Fixture::new();
    let clear = fixture.run.run.join("shim/clear");
    fs::write(&clear, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&clear, fs::Permissions::from_mode(0o755)).unwrap();
    let denied = fixture.root.path().join("synthetic-external-file");
    fs::write(&denied, "synthetic external data\n").unwrap();
    let denied = fs::canonicalize(denied).unwrap();
    fs::write(
        fixture.run.run.join("jail.sb"),
        format!(
            "(version 1)(allow default)(deny file-read-data (literal {:?}))\n",
            denied.to_string_lossy()
        ),
    )
    .unwrap();
    fs::write(&fixture.run.codex_bin, r#"#!/bin/sh
for name in HOME CODEX_HOME XDG_CONFIG_HOME XDG_RUNTIME_DIR XDG_CACHE_HOME XDG_DATA_HOME XDG_STATE_HOME ATERM_STATE_HOME TMPDIR TMP TEMP PWD CODEX_REFRESH_TOKEN_URL_OVERRIDE CODEX_REVOKE_TOKEN_URL_OVERRIDE HTTP_PROXY HTTPS_PROXY ALL_PROXY NO_PROXY LAB_CODEX_KEY USER LOGNAME TERM COLORTERM TERM_PROGRAM PREPARED_EXTRA; do
    eval 'printf "%s=%s\n" "$name" "${'"$name"'}"'
done
printf 'PARENT=%s\n' "${ATERM_PREPARED_PARENT_ONLY-unset}"
printf 'OPENAI=%s\n' "${OPENAI_API_KEY-unset}"
printf 'ARG=%s\n' "$@"
if /bin/cat "$PREPARED_DENIED" >/dev/null 2>&1; then printf 'EXTERNAL=read\n'; else printf 'EXTERNAL=denied\n'; fi
"#).unwrap();
    let literal = r#"spaces ; $(printf expanded) `printf expanded` ' " \ literal"#;
    let argument = "an argument ; $HOME $(printf expanded)";
    let launch = Launch {
        envs: vec![
            format!("PREPARED_EXTRA={literal}"),
            format!("PREPARED_DENIED={}", denied.display()),
        ],
        args: vec![argument.into()],
        ..selected(&["--prepare-only"])
    };
    let before = fixture.host();
    let plan = prepare_with(&fixture.run, &launch, "lab-user", |_| Ok(())).unwrap();
    let mut command = aterm_uds::spawnfd::command(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(fixture.root.path())
        .env("ATERM_PREPARED_PARENT_ONLY", "parent-only")
        .env("OPENAI_API_KEY", "parent-only");
    let out =
        aterm_uds::deadline::output_until(&mut command, Instant::now() + Duration::from_secs(60))
            .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let w = &fixture.run.w;
    for expected in [
        format!("HOME={}/home", w.display()),
        format!("CODEX_HOME={}/codex-home", w.display()),
        format!("XDG_CONFIG_HOME={}/cfg", w.display()),
        format!("XDG_RUNTIME_DIR={}", fixture.run.sockdir),
        format!("XDG_CACHE_HOME={}/home/.cache", w.display()),
        format!("XDG_DATA_HOME={}/home/.local/share", w.display()),
        format!("XDG_STATE_HOME={}/home/.local/state", w.display()),
        format!("ATERM_STATE_HOME={}/state", w.display()),
        format!("TMPDIR={}/tmp", w.display()),
        format!("TMP={}/tmp", w.display()),
        format!("TEMP={}/tmp", w.display()),
        format!("PWD={}", fixture.run.proj),
        format!(
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE=http://127.0.0.1:{}/oauth/token",
            fixture.run.port
        ),
        format!(
            "CODEX_REVOKE_TOKEN_URL_OVERRIDE=http://127.0.0.1:{}/oauth/revoke",
            fixture.run.port
        ),
        format!("HTTP_PROXY={}", fixture.run.proxy),
        format!("HTTPS_PROXY={}", fixture.run.proxy),
        format!("ALL_PROXY={}", fixture.run.proxy),
        "NO_PROXY=127.0.0.1,localhost".into(),
        format!("LAB_CODEX_KEY={LAB_CODEX_KEY}"),
        "USER=lab-user".into(),
        "LOGNAME=lab-user".into(),
        "TERM=xterm-256color".into(),
        "COLORTERM=truecolor".into(),
        "TERM_PROGRAM=aterm".into(),
        format!("PREPARED_EXTRA={literal}"),
        "PARENT=unset".into(),
        "OPENAI=unset".into(),
        format!("ARG={argument}"),
        "EXTERNAL=denied".into(),
    ] {
        assert!(
            stdout.lines().any(|line| line == expected),
            "missing {expected:?}: {stdout}"
        );
    }
    assert_eq!(fixture.host(), before);
    assert_eq!(
        fs::read_to_string(denied).unwrap(),
        "synthetic external data\n"
    );
}
